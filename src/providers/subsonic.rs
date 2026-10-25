//! Subsonic API client (Navidrome, Gonic, Airsonic, Ampache…), JSON flavour
//! with token authentication.

use std::fmt;
use std::fmt::Write as _;

use md5::{Digest, Md5};
use serde::de::{DeserializeOwned, IgnoredAny};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use ureq::Agent;

use super::{
    de_lenient_string, de_one_or_many, get_bytes, join_dot, non_empty, normalize_base, parse_json,
    push_query, query_param, random_u64, unix_millis, ProviderError, Result,
};
use crate::media::{MediaItem, SourceKind};

/// Protocol version sent as `v`: the oldest version with token auth, so
/// older servers do not reject us with error 30.
pub const API_VERSION: &str = "1.13.0";
/// Client name sent as `c`.
pub const CLIENT_NAME: &str = "rustune";

const MAX_BODY: u64 = 16 << 20;

/// Subsonic settings for the application config.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct SubsonicConfig {
    /// Server base URL, e.g. `https://music.example.com`.
    pub url: String,
    pub user: String,
    pub password: String,
    /// Transcoding target (`mp3`, `opus`, `raw` for the original…).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub format: Option<String>,
    /// Transcoding bitrate cap in kbit/s.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_bitrate: Option<u32>,
    /// Report now-playing and plays.
    pub scrobble: bool,
}

impl Default for SubsonicConfig {
    fn default() -> Self {
        Self {
            url: String::new(),
            user: String::new(),
            password: String::new(),
            format: None,
            max_bitrate: None,
            scrobble: true,
        }
    }
}

impl SubsonicConfig {
    pub fn is_configured(&self) -> bool {
        !self.url.trim().is_empty() && !self.user.trim().is_empty()
    }
}

impl fmt::Debug for SubsonicConfig {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SubsonicConfig")
            .field("url", &self.url)
            .field("user", &self.user)
            .field("password", &"<redacted>")
            .field("format", &self.format)
            .field("max_bitrate", &self.max_bitrate)
            .field("scrobble", &self.scrobble)
            .finish()
    }
}

/// `getAlbumList2` orderings.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum AlbumListType {
    #[default]
    Newest,
    Recent,
    Frequent,
    Random,
    AlphabeticalByName,
    AlphabeticalByArtist,
    Starred,
    Highest,
}

impl AlbumListType {
    pub const ALL: [AlbumListType; 8] = [
        Self::Newest,
        Self::Recent,
        Self::Frequent,
        Self::Random,
        Self::AlphabeticalByName,
        Self::AlphabeticalByArtist,
        Self::Starred,
        Self::Highest,
    ];

    /// Wire value of the `type` parameter.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Newest => "newest",
            Self::Recent => "recent",
            Self::Frequent => "frequent",
            Self::Random => "random",
            Self::AlphabeticalByName => "alphabeticalByName",
            Self::AlphabeticalByArtist => "alphabeticalByArtist",
            Self::Starred => "starred",
            Self::Highest => "highest",
        }
    }

    pub fn display_name(self) -> &'static str {
        match self {
            Self::Newest => "Recently added",
            Self::Recent => "Recently played",
            Self::Frequent => "Most played",
            Self::Random => "Random",
            Self::AlphabeticalByName => "By name",
            Self::AlphabeticalByArtist => "By artist",
            Self::Starred => "Starred",
            Self::Highest => "Top rated",
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Album {
    #[serde(deserialize_with = "de_lenient_string")]
    pub id: String,
    /// `name` in ID3 responses, `title` in folder-based ones.
    #[serde(alias = "title", deserialize_with = "de_lenient_string")]
    pub name: String,
    pub artist: Option<String>,
    pub artist_id: Option<String>,
    pub year: Option<u32>,
    pub song_count: Option<u32>,
    /// Seconds.
    pub duration: Option<u64>,
    pub cover_art: Option<String>,
    pub genre: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Artist {
    #[serde(deserialize_with = "de_lenient_string")]
    pub id: String,
    #[serde(deserialize_with = "de_lenient_string")]
    pub name: String,
    pub album_count: Option<u32>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Playlist {
    #[serde(deserialize_with = "de_lenient_string")]
    pub id: String,
    #[serde(deserialize_with = "de_lenient_string")]
    pub name: String,
    pub song_count: Option<u32>,
    /// Seconds.
    pub duration: Option<u64>,
    pub owner: Option<String>,
}

/// `search3` results; songs are playable items.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct SearchResults {
    pub artists: Vec<Artist>,
    pub albums: Vec<Album>,
    pub songs: Vec<MediaItem>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase", default)]
struct Song {
    #[serde(deserialize_with = "de_lenient_string")]
    id: String,
    #[serde(deserialize_with = "de_lenient_string")]
    title: String,
    artist: Option<String>,
    album: Option<String>,
    duration: Option<u64>,
    is_video: Option<bool>,
}

/// Stateless client: every request carries a fresh salt and token.
#[derive(Clone)]
pub struct SubsonicClient {
    base: String,
    user: String,
    password: String,
    format: Option<String>,
    max_bitrate: Option<u32>,
}

impl fmt::Debug for SubsonicClient {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SubsonicClient")
            .field("base", &self.base)
            .field("user", &self.user)
            .finish_non_exhaustive()
    }
}

/// `t` parameter: hex MD5 of password + salt.
pub fn token(password: &str, salt: &str) -> String {
    let mut hasher = Md5::new();
    hasher.update(password.as_bytes());
    hasher.update(salt.as_bytes());
    let digest = hasher.finalize();
    let mut out = String::with_capacity(32);
    for b in digest {
        let _ = write!(out, "{b:02x}");
    }
    out
}

fn new_salt() -> String {
    format!("{:016x}", random_u64())
}

impl SubsonicClient {
    pub fn new(cfg: &SubsonicConfig) -> Self {
        Self {
            base: normalize_base(&cfg.url),
            user: cfg.user.trim().to_string(),
            password: cfg.password.clone(),
            format: cfg
                .format
                .as_deref()
                .map(str::trim)
                .filter(|f| !f.is_empty())
                .map(str::to_string),
            max_bitrate: cfg.max_bitrate.filter(|&b| b > 0),
        }
    }

    /// Full authenticated URL for `endpoint` with a fresh salt.
    pub fn url(&self, endpoint: &str, params: &[(&str, &str)]) -> String {
        self.url_with_salt(endpoint, params, &new_salt())
    }

    /// [`url`](Self::url) with a caller-chosen salt (deterministic tests).
    pub fn url_with_salt(&self, endpoint: &str, params: &[(&str, &str)], salt: &str) -> String {
        let token = token(&self.password, salt);
        let mut url = String::with_capacity(self.base.len() + 128);
        url.push_str(&self.base);
        url.push_str("/rest/");
        url.push_str(endpoint);
        let auth = [
            ("u", self.user.as_str()),
            ("t", token.as_str()),
            ("s", salt),
            ("v", API_VERSION),
            ("c", CLIENT_NAME),
            ("f", "json"),
        ];
        push_query(&mut url, auth.into_iter().chain(params.iter().copied()));
        url
    }

    /// Playable stream URL. The token never expires, so the URL can be
    /// stored as the item ID; it is a secret equivalent and must not be
    /// logged.
    pub fn stream_url(&self, song_id: &str) -> String {
        self.stream_url_with_salt(song_id, &new_salt())
    }

    fn stream_url_with_salt(&self, song_id: &str, salt: &str) -> String {
        let bitrate = self.max_bitrate.map(|b| b.to_string());
        let mut params: Vec<(&str, &str)> = Vec::with_capacity(3);
        params.push(("id", song_id));
        if let Some(format) = self.format.as_deref() {
            params.push(("format", format));
        }
        if let Some(bitrate) = bitrate.as_deref() {
            params.push(("maxBitRate", bitrate));
        }
        self.url_with_salt("stream", &params, salt)
    }

    /// Song ID of an item produced by this client (parsed from its stream
    /// URL), for scrobbling and starring.
    pub fn song_id(&self, item: &MediaItem) -> Option<String> {
        let rest = item.id.strip_prefix(self.base.as_str())?;
        if !rest.starts_with("/rest/stream?") {
            return None;
        }
        query_param(&item.id, "id").filter(|id| !id.is_empty())
    }

    #[cfg(test)]
    /// Checks reachability and credentials.
    pub fn ping(&self, agent: &Agent) -> Result<()> {
        self.call::<IgnoredAny>(agent, "ping", &[]).map(|_| ())
    }

    /// `search3`. `limit` caps each of artists, albums and songs.
    pub fn search(&self, agent: &Agent, query: &str, limit: u32) -> Result<SearchResults> {
        #[derive(Deserialize, Default)]
        #[serde(default)]
        struct Resp {
            #[serde(rename = "searchResult3")]
            result: SearchResult3,
        }
        #[derive(Deserialize, Default)]
        #[serde(default)]
        struct SearchResult3 {
            #[serde(deserialize_with = "de_one_or_many")]
            artist: Vec<Artist>,
            #[serde(deserialize_with = "de_one_or_many")]
            album: Vec<Album>,
            #[serde(deserialize_with = "de_one_or_many")]
            song: Vec<Song>,
        }
        let count = limit.max(1).to_string();
        let resp: Resp = self.call(
            agent,
            "search3",
            &[
                ("query", query),
                ("artistCount", count.as_str()),
                ("albumCount", count.as_str()),
                ("songCount", count.as_str()),
            ],
        )?;
        Ok(SearchResults {
            artists: resp.result.artist,
            albums: resp.result.album,
            songs: self.songs(resp.result.song),
        })
    }

    /// `getAlbumList2`: one page of albums.
    pub fn album_list(
        &self,
        agent: &Agent,
        kind: AlbumListType,
        offset: u32,
        size: u32,
    ) -> Result<Vec<Album>> {
        #[derive(Deserialize, Default)]
        #[serde(default)]
        struct Resp {
            #[serde(rename = "albumList2")]
            list: AlbumList,
        }
        #[derive(Deserialize, Default)]
        #[serde(default)]
        struct AlbumList {
            #[serde(deserialize_with = "de_one_or_many")]
            album: Vec<Album>,
        }
        let offset = offset.to_string();
        let size = size.clamp(1, 500).to_string();
        let resp: Resp = self.call(
            agent,
            "getAlbumList2",
            &[
                ("type", kind.as_str()),
                ("offset", offset.as_str()),
                ("size", size.as_str()),
            ],
        )?;
        Ok(resp.list.album)
    }

    /// `getAlbum`: album details and its songs.
    pub fn album(&self, agent: &Agent, album_id: &str) -> Result<(Album, Vec<MediaItem>)> {
        #[derive(Deserialize, Default)]
        #[serde(default)]
        struct Resp {
            album: AlbumWithSongs,
        }
        #[derive(Deserialize, Default)]
        #[serde(default)]
        struct AlbumWithSongs {
            #[serde(flatten)]
            album: Album,
            #[serde(deserialize_with = "de_one_or_many")]
            song: Vec<Song>,
        }
        let resp: Resp = self.call(agent, "getAlbum", &[("id", album_id)])?;
        let songs = self.songs(resp.album.song);
        Ok((resp.album.album, songs))
    }

    /// `getPlaylists`.
    pub fn playlists(&self, agent: &Agent) -> Result<Vec<Playlist>> {
        #[derive(Deserialize, Default)]
        #[serde(default)]
        struct Resp {
            playlists: Playlists,
        }
        #[derive(Deserialize, Default)]
        #[serde(default)]
        struct Playlists {
            #[serde(deserialize_with = "de_one_or_many")]
            playlist: Vec<Playlist>,
        }
        let resp: Resp = self.call(agent, "getPlaylists", &[])?;
        Ok(resp.playlists.playlist)
    }

    /// `getPlaylist`: the playlist's songs.
    pub fn playlist(&self, agent: &Agent, playlist_id: &str) -> Result<Vec<MediaItem>> {
        #[derive(Deserialize, Default)]
        #[serde(default)]
        struct Resp {
            playlist: Entries,
        }
        #[derive(Deserialize, Default)]
        #[serde(default)]
        struct Entries {
            #[serde(deserialize_with = "de_one_or_many")]
            entry: Vec<Song>,
        }
        let resp: Resp = self.call(agent, "getPlaylist", &[("id", playlist_id)])?;
        Ok(self.songs(resp.playlist.entry))
    }

    /// `getRandomSongs`.
    pub fn random_songs(&self, agent: &Agent, size: u32) -> Result<Vec<MediaItem>> {
        #[derive(Deserialize, Default)]
        #[serde(default)]
        struct Resp {
            #[serde(rename = "randomSongs")]
            random: SongList,
        }
        #[derive(Deserialize, Default)]
        #[serde(default)]
        struct SongList {
            #[serde(deserialize_with = "de_one_or_many")]
            song: Vec<Song>,
        }
        let size = size.clamp(1, 500).to_string();
        let resp: Resp = self.call(agent, "getRandomSongs", &[("size", size.as_str())])?;
        Ok(self.songs(resp.random.song))
    }

    /// `scrobble`: `submission = false` reports "now playing", `true` a
    /// finished play (timestamped now).
    pub fn scrobble(&self, agent: &Agent, song_id: &str, submission: bool) -> Result<()> {
        let time = unix_millis().to_string();
        let mut params = vec![
            ("id", song_id),
            ("submission", if submission { "true" } else { "false" }),
        ];
        if submission {
            params.push(("time", time.as_str()));
        }
        self.call::<IgnoredAny>(agent, "scrobble", &params)
            .map(|_| ())
    }

    fn call<T: DeserializeOwned>(
        &self,
        agent: &Agent,
        endpoint: &str,
        params: &[(&str, &str)],
    ) -> Result<T> {
        let url = self.url(endpoint, params);
        parse_envelope(&get_bytes(agent, &url, "application/json", MAX_BODY)?)
    }

    fn songs(&self, songs: Vec<Song>) -> Vec<MediaItem> {
        songs
            .into_iter()
            .filter(|s| !s.id.is_empty() && s.is_video != Some(true))
            .map(|s| self.song_item(s))
            .collect()
    }

    fn song_item(&self, s: Song) -> MediaItem {
        let artist = non_empty(s.artist);
        let album = non_empty(s.album);
        MediaItem {
            id: self.stream_url(&s.id),
            title: non_empty(Some(s.title)).unwrap_or_else(|| "Untitled".to_string()),
            subtitle: join_dot([artist.as_deref(), album.as_deref()]),
            artist,
            album,
            duration: s.duration.filter(|&d| d > 0),
            source: SourceKind::Url,
        }
    }
}

/// Unwraps `{"subsonic-response": {...}}`, turning `status: failed` into an
/// error (codes 40/41 = bad credentials → [`ProviderError::Auth`]).
pub fn parse_envelope<T: DeserializeOwned>(bytes: &[u8]) -> Result<T> {
    let mut root: Value = parse_json(bytes)?;
    let inner = root
        .get_mut("subsonic-response")
        .map(Value::take)
        .filter(Value::is_object)
        .ok_or_else(|| ProviderError::Parse("not a Subsonic response".into()))?;
    if inner.get("status").and_then(Value::as_str) == Some("failed") {
        let err = inner.get("error");
        let code = err
            .and_then(|e| e.get("code"))
            .and_then(Value::as_i64)
            .unwrap_or(0);
        let message = err
            .and_then(|e| e.get("message"))
            .and_then(Value::as_str)
            .filter(|m| !m.trim().is_empty())
            .unwrap_or("request failed")
            .to_string();
        return Err(match code {
            40 | 41 => ProviderError::Auth(message),
            _ => ProviderError::Api { code, message },
        });
    }
    serde_json::from_value(inner).map_err(|e| ProviderError::Parse(e.to_string()))
}

#[cfg(test)]
mod tests {
    use super::super::testutil::{agent, request_line, MockServer, Reply};
    use super::*;

    fn client() -> SubsonicClient {
        SubsonicClient::new(&SubsonicConfig {
            url: " https://music.example.com/ ".into(),
            user: "alice".into(),
            password: "sesame".into(),
            format: Some("opus".into()),
            max_bitrate: Some(128),
            scrobble: true,
        })
    }

    #[test]
    fn token_is_md5_of_password_and_salt() {
        // Example from the Subsonic API documentation.
        assert_eq!(
            token("sesame", "c19b2d"),
            "26719a1196d2a940705a59634eb18eab"
        );
    }

    #[test]
    fn builds_urls_with_fixed_salt() {
        let c = client();
        assert_eq!(
            c.url_with_salt("search3", &[("query", "a&b c")], "c19b2d"),
            "https://music.example.com/rest/search3?u=alice&t=26719a1196d2a940705a59634eb18eab\
             &s=c19b2d&v=1.13.0&c=rustune&f=json&query=a%26b%20c"
        );
        assert_eq!(
            c.stream_url_with_salt("tr/1", "c19b2d"),
            "https://music.example.com/rest/stream?u=alice&t=26719a1196d2a940705a59634eb18eab\
             &s=c19b2d&v=1.13.0&c=rustune&f=json&id=tr%2F1&format=opus&maxBitRate=128"
        );
        let plain = SubsonicClient::new(&SubsonicConfig {
            url: "http://h".into(),
            user: "u".into(),
            format: Some("  ".into()),
            max_bitrate: Some(0),
            ..SubsonicConfig::default()
        });
        assert!(plain.stream_url("x").ends_with("&f=json&id=x"));
    }

    #[test]
    fn fresh_salt_per_url() {
        let c = client();
        let a = c.url("ping", &[]);
        let b = c.url("ping", &[]);
        assert_ne!(query_param(&a, "s"), query_param(&b, "s"));
        let s = query_param(&a, "s").unwrap();
        assert_eq!(query_param(&a, "t").unwrap(), token("sesame", &s));
    }

    #[test]
    fn song_id_round_trip() {
        let c = client();
        let item = c.song_item(Song {
            id: "abc 1".into(),
            title: "T".into(),
            ..Song::default()
        });
        assert_eq!(c.song_id(&item).as_deref(), Some("abc 1"));
        let foreign = MediaItem {
            id: "https://other.example/rest/stream?id=1".into(),
            ..item
        };
        assert_eq!(c.song_id(&foreign), None);
    }

    #[test]
    fn debug_hides_password() {
        let cfg = SubsonicConfig {
            password: "hunter2".into(),
            ..SubsonicConfig::default()
        };
        assert!(!format!("{cfg:?}").contains("hunter2"));
        assert!(!format!("{:?}", SubsonicClient::new(&cfg)).contains("hunter2"));
    }

    #[test]
    fn envelope_errors() {
        let failed = br#"{"subsonic-response":{"status":"failed","version":"1.16.1",
            "error":{"code":40,"message":"Wrong username or password"}}}"#;
        assert_eq!(
            parse_envelope::<IgnoredAny>(failed).unwrap_err(),
            ProviderError::Auth("Wrong username or password".into())
        );
        let not_found = br#"{"subsonic-response":{"status":"failed","error":{"code":70}}}"#;
        assert_eq!(
            parse_envelope::<IgnoredAny>(not_found).unwrap_err(),
            ProviderError::Api {
                code: 70,
                message: "request failed".into()
            }
        );
        assert!(matches!(
            parse_envelope::<IgnoredAny>(br#"{"error":"proxy"}"#),
            Err(ProviderError::Parse(_))
        ));
        assert!(parse_envelope::<IgnoredAny>(br#"{"subsonic-response":{"status":"ok"}}"#).is_ok());
    }

    #[test]
    fn parses_album_with_songs_and_lenient_shapes() {
        let body = br#"{"subsonic-response":{"status":"ok","version":"1.16.1","type":"navidrome",
          "album":{"id":"al-1","name":"Kind of Blue","artist":"Miles Davis","artistId":"ar-1",
            "year":1959,"songCount":2,"duration":2700,"coverArt":"al-1","genre":"Jazz",
            "song":[{"id":"s1","parent":"al-1","title":"So What","album":"Kind of Blue",
                     "artist":"Miles Davis","track":1,"duration":562,"suffix":"flac","isVideo":false},
                    {"id":"s2","title":"Clip","isVideo":true}]}}}"#;
        #[derive(Deserialize, Default)]
        #[serde(default)]
        struct Resp {
            album: AlbumBody,
        }
        #[derive(Deserialize, Default)]
        #[serde(default)]
        struct AlbumBody {
            #[serde(flatten)]
            album: Album,
            #[serde(deserialize_with = "de_one_or_many")]
            song: Vec<Song>,
        }
        let resp: Resp = parse_envelope(body).unwrap();
        assert_eq!(resp.album.album.name, "Kind of Blue");
        assert_eq!(resp.album.album.year, Some(1959));
        let items = client().songs(resp.album.song);
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].title, "So What");
        assert_eq!(items[0].duration, Some(562));
        assert_eq!(
            items[0].subtitle.as_deref(),
            Some("Miles Davis · Kind of Blue")
        );
        assert!(items[0]
            .id
            .starts_with("https://music.example.com/rest/stream?"));

        // Old servers: numeric IDs, `title` instead of `name`, single object
        // instead of a one-element list.
        let legacy = br#"{"subsonic-response":{"status":"ok","albumList2":
            {"album":{"id":42,"title":"Solo","artist":"X"}}}}"#;
        #[derive(Deserialize, Default)]
        #[serde(default)]
        struct ListResp {
            #[serde(rename = "albumList2")]
            list: List,
        }
        #[derive(Deserialize, Default)]
        #[serde(default)]
        struct List {
            #[serde(deserialize_with = "de_one_or_many")]
            album: Vec<Album>,
        }
        let resp: ListResp = parse_envelope(legacy).unwrap();
        assert_eq!(resp.list.album.len(), 1);
        assert_eq!(resp.list.album[0].id, "42");
        assert_eq!(resp.list.album[0].name, "Solo");
        // Empty list: the key is absent entirely.
        let empty: ListResp =
            parse_envelope(br#"{"subsonic-response":{"status":"ok","albumList2":{}}}"#).unwrap();
        assert!(empty.list.album.is_empty());
    }

    #[test]
    fn album_list_round_trip() {
        let body = r#"{"subsonic-response":{"status":"ok","version":"1.16.1",
          "albumList2":{"album":[{"id":"a1","name":"One","artist":"A","songCount":9},
                                 {"id":"a2","name":"Two"}]}}}"#;
        let server = MockServer::start(vec![Reply::json(body)]);
        let c = SubsonicClient::new(&SubsonicConfig {
            url: server.base.clone(),
            user: "alice".into(),
            password: "sesame".into(),
            ..SubsonicConfig::default()
        });
        let albums = c
            .album_list(&agent(), AlbumListType::AlphabeticalByName, 20, 10)
            .unwrap();
        assert_eq!(albums.len(), 2);
        assert_eq!(albums[0].song_count, Some(9));
        let requests = server.requests();
        let line = request_line(&requests[0]);
        assert!(
            line.starts_with("GET /rest/getAlbumList2?u=alice&t="),
            "{line}"
        );
        assert!(
            line.ends_with(
                "&v=1.13.0&c=rustune&f=json&type=alphabeticalByName&offset=20&size=10 HTTP/1.1"
            ),
            "{line}"
        );
    }

    #[test]
    fn ping_reports_auth_failure() {
        let body = r#"{"subsonic-response":{"status":"failed","version":"1.16.1",
            "error":{"code":40,"message":"Wrong username or password"}}}"#;
        let server = MockServer::start(vec![Reply::json(body)]);
        let c = SubsonicClient::new(&SubsonicConfig {
            url: server.base.clone(),
            user: "alice".into(),
            password: "s3cr3t-pw".into(),
            ..SubsonicConfig::default()
        });
        let err = c.ping(&agent()).unwrap_err();
        assert!(matches!(err, ProviderError::Auth(_)));
        assert!(!err.to_string().contains("s3cr3t-pw"));
        server.requests();
    }

    #[test]
    #[ignore = "network: Navidrome demo server"]
    fn live_demo_server() {
        let c = SubsonicClient::new(&SubsonicConfig {
            url: "https://demo.navidrome.org".into(),
            user: "demo".into(),
            password: "demo".into(),
            ..SubsonicConfig::default()
        });
        let agent = agent();
        c.ping(&agent).unwrap();
        let albums = c.album_list(&agent, AlbumListType::Newest, 0, 5).unwrap();
        let (_, songs) = c.album(&agent, &albums[0].id).unwrap();
        assert!(!songs.is_empty());
    }
}
