//! Jellyfin client (also speaks to Emby for the shared endpoints: it sends
//! both the `MediaBrowser` and `X-Emby-*` auth headers).

use std::fmt;
use std::time::Duration;

use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use ureq::Agent;

use super::{
    de_lenient_string, join_dot, non_empty, normalize_base, parse_json, percent_decode,
    push_encoded, push_query, read_ok, ProviderError, Result, USER_AGENT,
};
use crate::media::{MediaItem, SourceKind};

const MAX_BODY: u64 = 16 << 20;
const TICKS_PER_SECOND: u64 = 10_000_000;
const ITEM_FIELDS: &str = "RunTimeTicks,ProductionYear,ChildCount";
/// Containers our decoder handles natively; anything else is transcoded.
const DIRECT_CONTAINERS: &str = "mp3,aac,m4a|aac,m4b|aac,flac,wav,ogg|vorbis";
const DEFAULT_MAX_BITRATE: u32 = 320;

/// Jellyfin settings for the application config. Either `token` (an API
/// key or a saved session token) or `user` (+ `password`) is required.
#[derive(Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct JellyfinConfig {
    pub url: String,
    pub user: String,
    pub password: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub token: Option<String>,
    /// Skips user discovery when a token is set.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub user_id: Option<String>,
    /// Transcoding cap in kbit/s (default 320).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_bitrate: Option<u32>,
}

impl JellyfinConfig {
    pub fn is_configured(&self) -> bool {
        !self.url.trim().is_empty()
            && (!self.user.trim().is_empty()
                || self.token.as_deref().is_some_and(|t| !t.trim().is_empty()))
    }
}

impl fmt::Debug for JellyfinConfig {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("JellyfinConfig")
            .field("url", &self.url)
            .field("user", &self.user)
            .field("password", &"<redacted>")
            .field("token", &self.token.as_ref().map(|_| "<redacted>"))
            .field("user_id", &self.user_id)
            .field("max_bitrate", &self.max_bitrate)
            .finish()
    }
}

/// Album sort orders for [`JellyfinClient::albums`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum AlbumSort {
    #[default]
    Name,
    Artist,
    Year,
    RecentlyAdded,
    Random,
}

impl AlbumSort {
    pub const ALL: [AlbumSort; 5] = [
        Self::Name,
        Self::Artist,
        Self::Year,
        Self::RecentlyAdded,
        Self::Random,
    ];

    pub fn display_name(self) -> &'static str {
        match self {
            Self::Name => "By name",
            Self::Artist => "By artist",
            Self::Year => "By year",
            Self::RecentlyAdded => "Recently added",
            Self::Random => "Random",
        }
    }

    /// `(sortBy, sortOrder)`.
    fn params(self) -> (&'static str, &'static str) {
        match self {
            Self::Name => ("SortName", "Ascending"),
            Self::Artist => ("AlbumArtist,SortName", "Ascending"),
            Self::Year => ("ProductionYear,SortName", "Descending"),
            Self::RecentlyAdded => ("DateCreated,SortName", "Descending"),
            Self::Random => ("Random", "Ascending"),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Album {
    pub id: String,
    pub name: String,
    pub artist: Option<String>,
    pub year: Option<u32>,
    pub track_count: Option<u32>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Playlist {
    pub id: String,
    pub name: String,
    pub track_count: Option<u32>,
    /// Seconds.
    pub duration: Option<u64>,
}

/// `/System/Info/Public`.
#[cfg(test)]
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "PascalCase", default)]
pub struct ServerInfo {
    pub server_name: String,
    pub version: String,
    pub id: String,
    pub product_name: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "PascalCase", default)]
struct ItemsResponse {
    items: Vec<ItemDto>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "PascalCase", default)]
struct ItemDto {
    #[serde(deserialize_with = "de_lenient_string")]
    id: String,
    name: Option<String>,
    album: Option<String>,
    album_artist: Option<String>,
    artists: Option<Vec<String>>,
    run_time_ticks: Option<i64>,
    production_year: Option<u32>,
    child_count: Option<u32>,
    media_type: Option<String>,
}

impl ItemDto {
    fn seconds(&self) -> Option<u64> {
        self.run_time_ticks
            .and_then(|t| u64::try_from(t).ok())
            .map(|t| t / TICKS_PER_SECOND)
            .filter(|&s| s > 0)
    }

    fn first_artist(&mut self) -> Option<String> {
        let artists = self.artists.take().unwrap_or_default();
        non_empty(artists.into_iter().find(|a| !a.trim().is_empty()))
            .or_else(|| non_empty(self.album_artist.take()))
    }
}

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "PascalCase", default)]
struct AuthResponse {
    access_token: String,
    user: UserDto,
}

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "PascalCase", default)]
struct UserDto {
    id: String,
    name: String,
}

#[derive(Serialize)]
#[serde(rename_all = "PascalCase")]
struct PlaybackReport<'a> {
    item_id: &'a str,
    position_ticks: u64,
    can_seek: bool,
    is_paused: bool,
    is_muted: bool,
    play_method: &'static str,
}

#[derive(Serialize)]
#[serde(rename_all = "PascalCase")]
struct PlaybackStop<'a> {
    item_id: &'a str,
    position_ticks: u64,
}

/// Authenticated session. Build with [`connect`](Self::connect) (network)
/// or [`with_session`](Self::with_session) (saved token).
#[derive(Clone)]
pub struct JellyfinClient {
    base: String,
    token: String,
    user_id: String,
    device_id: String,
    max_bitrate: u32,
}

impl fmt::Debug for JellyfinClient {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("JellyfinClient")
            .field("base", &self.base)
            .field("user_id", &self.user_id)
            .finish_non_exhaustive()
    }
}

/// Stable per-server/user device ID (FNV-1a), so the server shows one
/// session instead of a new device on every start.
fn device_id(base: &str, user: &str) -> String {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for &b in base.as_bytes().iter().chain(b"\0").chain(user.as_bytes()) {
        hash ^= u64::from(b);
        hash = hash.wrapping_mul(0x0100_0000_01b3);
    }
    format!("rustune-{hash:016x}")
}

fn auth_header(device_id: &str, token: Option<&str>) -> String {
    let mut h = format!(
        "MediaBrowser Client=\"rustune\", Device=\"rustune\", DeviceId=\"{device_id}\", Version=\"{}\"",
        env!("CARGO_PKG_VERSION")
    );
    if let Some(token) = token {
        h.push_str(", Token=\"");
        h.push_str(token);
        h.push('"');
    }
    h
}

fn ticks(d: Duration) -> u64 {
    u64::try_from(d.as_nanos() / 100).unwrap_or(u64::MAX)
}

impl JellyfinClient {
    /// Authenticates: uses `token` when set (discovering the user ID unless
    /// configured), otherwise `/Users/AuthenticateByName`.
    pub fn connect(agent: &Agent, cfg: &JellyfinConfig) -> Result<Self> {
        let base = normalize_base(&cfg.url);
        if base.is_empty() {
            return Err(ProviderError::Invalid("missing Jellyfin URL".into()));
        }
        let user = cfg.user.trim();
        let mut client = Self {
            device_id: device_id(&base, user),
            base,
            token: String::new(),
            user_id: String::new(),
            max_bitrate: cfg
                .max_bitrate
                .filter(|&b| b > 0)
                .unwrap_or(DEFAULT_MAX_BITRATE),
        };
        let token = cfg
            .token
            .as_deref()
            .map(str::trim)
            .filter(|t| !t.is_empty());
        if let Some(token) = token {
            client.token = token.to_string();
            client.user_id = match cfg
                .user_id
                .as_deref()
                .map(str::trim)
                .filter(|u| !u.is_empty())
            {
                Some(id) => id.to_string(),
                None => client.discover_user_id(agent, user)?,
            };
            return Ok(client);
        }
        if user.is_empty() {
            return Err(ProviderError::Auth("missing token or user name".into()));
        }
        #[derive(Serialize)]
        #[serde(rename_all = "PascalCase")]
        struct Login<'a> {
            username: &'a str,
            pw: &'a str,
        }
        let url = format!("{}/Users/AuthenticateByName", client.base);
        let result = client.request(agent.post(&url)).send_json(&Login {
            username: user,
            pw: &cfg.password,
        });
        let bytes = match read_ok(result, MAX_BODY) {
            Err(ProviderError::Status(401 | 403)) => {
                return Err(ProviderError::Auth("wrong user name or password".into()))
            }
            other => other?,
        };
        let auth: AuthResponse = parse_json(&bytes)?;
        if auth.access_token.is_empty() || auth.user.id.is_empty() {
            return Err(ProviderError::Parse("login response without token".into()));
        }
        client.token = auth.access_token;
        client.user_id = auth.user.id;
        Ok(client)
    }

    #[cfg(test)]
    /// Session from a saved token and user ID (no network).
    pub fn with_session(
        base_url: &str,
        token: &str,
        user_id: &str,
        max_bitrate: Option<u32>,
    ) -> Self {
        let base = normalize_base(base_url);
        Self {
            device_id: device_id(&base, user_id),
            base,
            token: token.to_string(),
            user_id: user_id.to_string(),
            max_bitrate: max_bitrate
                .filter(|&b| b > 0)
                .unwrap_or(DEFAULT_MAX_BITRATE),
        }
    }

    /// Session token: store it (e.g. as `JellyfinConfig::token`) instead of
    /// the password.
    pub fn token(&self) -> &str {
        &self.token
    }

    pub fn user_id(&self) -> &str {
        &self.user_id
    }

    #[cfg(test)]
    /// Unauthenticated server identity, usable as a reachability check.
    pub fn server_info(&self, agent: &Agent) -> Result<ServerInfo> {
        self.get_json(agent, "/System/Info/Public", &[])
    }

    /// Audio tracks matching `query`.
    pub fn search(&self, agent: &Agent, query: &str, limit: u32) -> Result<Vec<MediaItem>> {
        let limit = limit.clamp(1, 500).to_string();
        let resp: ItemsResponse = self.get_json(
            agent,
            "/Items",
            &[
                ("userId", self.user_id.as_str()),
                ("searchTerm", query),
                ("includeItemTypes", "Audio"),
                ("recursive", "true"),
                ("limit", limit.as_str()),
                ("fields", ITEM_FIELDS),
                ("enableTotalRecordCount", "false"),
            ],
        )?;
        Ok(self.tracks(resp.items))
    }

    /// One page of music albums across all libraries.
    pub fn albums(
        &self,
        agent: &Agent,
        sort: AlbumSort,
        offset: u32,
        limit: u32,
    ) -> Result<Vec<Album>> {
        let (sort_by, sort_order) = sort.params();
        let offset = offset.to_string();
        let limit = limit.clamp(1, 1000).to_string();
        let resp: ItemsResponse = self.get_json(
            agent,
            "/Items",
            &[
                ("userId", self.user_id.as_str()),
                ("includeItemTypes", "MusicAlbum"),
                ("recursive", "true"),
                ("sortBy", sort_by),
                ("sortOrder", sort_order),
                ("startIndex", offset.as_str()),
                ("limit", limit.as_str()),
                ("fields", ITEM_FIELDS),
                ("enableTotalRecordCount", "false"),
            ],
        )?;
        Ok(resp
            .items
            .into_iter()
            .filter(|it| !it.id.is_empty())
            .map(|mut it| Album {
                artist: non_empty(it.album_artist.take()).or_else(|| it.first_artist()),
                name: non_empty(it.name.take()).unwrap_or_else(|| "Untitled album".into()),
                year: it.production_year,
                track_count: it.child_count,
                id: it.id,
            })
            .collect())
    }

    /// Tracks of an album in disc/track order.
    pub fn album_tracks(&self, agent: &Agent, album_id: &str) -> Result<Vec<MediaItem>> {
        let resp: ItemsResponse = self.get_json(
            agent,
            "/Items",
            &[
                ("userId", self.user_id.as_str()),
                ("parentId", album_id),
                ("includeItemTypes", "Audio"),
                ("sortBy", "ParentIndexNumber,IndexNumber,SortName"),
                ("sortOrder", "Ascending"),
                ("fields", ITEM_FIELDS),
                ("enableTotalRecordCount", "false"),
            ],
        )?;
        Ok(self.tracks(resp.items))
    }

    /// The user's audio playlists.
    pub fn playlists(&self, agent: &Agent) -> Result<Vec<Playlist>> {
        let resp: ItemsResponse = self.get_json(
            agent,
            "/Items",
            &[
                ("userId", self.user_id.as_str()),
                ("includeItemTypes", "Playlist"),
                ("recursive", "true"),
                ("sortBy", "SortName"),
                ("sortOrder", "Ascending"),
                ("fields", ITEM_FIELDS),
                ("enableTotalRecordCount", "false"),
            ],
        )?;
        Ok(resp
            .items
            .into_iter()
            .filter(|it| {
                !it.id.is_empty()
                    && it
                        .media_type
                        .as_deref()
                        .is_none_or(|m| m.eq_ignore_ascii_case("audio"))
            })
            .map(|mut it| Playlist {
                name: non_empty(it.name.take()).unwrap_or_else(|| "Untitled playlist".into()),
                track_count: it.child_count,
                duration: it.seconds(),
                id: it.id,
            })
            .collect())
    }

    /// Tracks of a playlist in playlist order.
    pub fn playlist_tracks(&self, agent: &Agent, playlist_id: &str) -> Result<Vec<MediaItem>> {
        let mut path = String::from("/Playlists/");
        push_encoded(&mut path, playlist_id);
        path.push_str("/Items");
        let resp: ItemsResponse = self.get_json(
            agent,
            &path,
            &[
                ("userId", self.user_id.as_str()),
                ("fields", ITEM_FIELDS),
                ("enableTotalRecordCount", "false"),
            ],
        )?;
        Ok(self.tracks(resp.items))
    }

    /// Universal audio endpoint: direct stream for natively decodable
    /// containers, MP3 transcode otherwise. Carries the token (`api_key`),
    /// so treat it as a secret.
    pub fn stream_url(&self, item_id: &str) -> String {
        let bitrate = (u64::from(self.max_bitrate) * 1000).to_string();
        let mut url = format!("{}/Audio/", self.base);
        push_encoded(&mut url, item_id);
        url.push_str("/universal");
        push_query(
            &mut url,
            [
                ("UserId", self.user_id.as_str()),
                ("DeviceId", self.device_id.as_str()),
                ("api_key", self.token.as_str()),
                ("Container", DIRECT_CONTAINERS),
                ("TranscodingContainer", "mp3"),
                ("TranscodingProtocol", "http"),
                ("AudioCodec", "mp3"),
                ("MaxStreamingBitrate", bitrate.as_str()),
            ],
        );
        url
    }

    #[cfg(test)]
    /// Original file download, for servers whose universal endpoint
    /// misbehaves. Same secrecy caveat as [`stream_url`](Self::stream_url).
    pub fn download_url(&self, item_id: &str) -> String {
        let mut url = format!("{}/Items/", self.base);
        push_encoded(&mut url, item_id);
        url.push_str("/Download");
        push_query(&mut url, [("api_key", self.token.as_str())]);
        url
    }

    /// Item ID of an item produced by this client, for playback reports.
    pub fn item_id(&self, item: &MediaItem) -> Option<String> {
        let rest = item.id.strip_prefix(self.base.as_str())?;
        let path = rest.split('?').next().unwrap_or(rest);
        let id = path
            .strip_prefix("/Audio/")
            .and_then(|p| p.strip_suffix("/universal"))
            .or_else(|| {
                path.strip_prefix("/Items/")
                    .and_then(|p| p.strip_suffix("/Download"))
            })?;
        (!id.is_empty() && !id.contains('/')).then(|| percent_decode(id))
    }

    /// `POST /Sessions/Playing`.
    pub fn report_start(&self, agent: &Agent, item_id: &str, position: Duration) -> Result<()> {
        self.post_report(agent, "/Sessions/Playing", item_id, position, false)
    }

    /// `POST /Sessions/Playing/Progress`; call periodically and on pause.
    pub fn report_progress(
        &self,
        agent: &Agent,
        item_id: &str,
        position: Duration,
        paused: bool,
    ) -> Result<()> {
        self.post_report(
            agent,
            "/Sessions/Playing/Progress",
            item_id,
            position,
            paused,
        )
    }

    /// `POST /Sessions/Playing/Stopped`.
    pub fn report_stopped(&self, agent: &Agent, item_id: &str, position: Duration) -> Result<()> {
        let url = format!("{}/Sessions/Playing/Stopped", self.base);
        let result = self.request(agent.post(&url)).send_json(&PlaybackStop {
            item_id,
            position_ticks: ticks(position),
        });
        read_ok(result, 64 << 10).map(|_| ())
    }

    fn post_report(
        &self,
        agent: &Agent,
        path: &str,
        item_id: &str,
        position: Duration,
        paused: bool,
    ) -> Result<()> {
        let url = format!("{}{path}", self.base);
        let result = self.request(agent.post(&url)).send_json(&PlaybackReport {
            item_id,
            position_ticks: ticks(position),
            can_seek: true,
            is_paused: paused,
            is_muted: false,
            play_method: "DirectStream",
        });
        read_ok(result, 64 << 10).map(|_| ())
    }

    /// Resolves the user owning the token: `/Users/Me` for session tokens;
    /// API keys belong to no user (400), so fall back to `/Users`, matching
    /// `user` by name or taking the first account.
    fn discover_user_id(&self, agent: &Agent, user: &str) -> Result<String> {
        match self.get_json::<UserDto>(agent, "/Users/Me", &[]) {
            Ok(me) if !me.id.is_empty() => return Ok(me.id),
            Ok(_) => return Err(ProviderError::Parse("current user without ID".into())),
            Err(ProviderError::Status(400)) => {}
            Err(ProviderError::Status(401 | 403)) => {
                return Err(ProviderError::Auth("token rejected".into()))
            }
            Err(e) => return Err(e),
        }
        let users: Vec<UserDto> = self.get_json(agent, "/Users", &[])?;
        let found = if user.is_empty() {
            users.into_iter().find(|u| !u.id.is_empty())
        } else {
            users
                .into_iter()
                .find(|u| !u.id.is_empty() && u.name.eq_ignore_ascii_case(user))
        };
        found
            .map(|u| u.id)
            .ok_or_else(|| ProviderError::Auth("user not found on server".into()))
    }

    fn request<B>(&self, req: ureq::RequestBuilder<B>) -> ureq::RequestBuilder<B> {
        let token = (!self.token.is_empty()).then_some(self.token.as_str());
        let auth = auth_header(&self.device_id, token);
        let req = req
            .header("User-Agent", USER_AGENT)
            .header("Accept", "application/json")
            .header("Authorization", auth.as_str())
            .header("X-Emby-Authorization", auth.as_str());
        match token {
            Some(token) => req.header("X-Emby-Token", token),
            None => req,
        }
    }

    fn get_json<T: DeserializeOwned>(
        &self,
        agent: &Agent,
        path: &str,
        params: &[(&str, &str)],
    ) -> Result<T> {
        let mut url = String::with_capacity(self.base.len() + path.len() + 128);
        url.push_str(&self.base);
        url.push_str(path);
        push_query(&mut url, params.iter().copied());
        let bytes = read_ok(self.request(agent.get(&url)).call(), MAX_BODY)?;
        parse_json(&bytes)
    }

    fn tracks(&self, items: Vec<ItemDto>) -> Vec<MediaItem> {
        items
            .into_iter()
            .filter(|it| !it.id.is_empty())
            .map(|mut it| {
                let artist = it.first_artist();
                let album = non_empty(it.album.take());
                MediaItem {
                    id: self.stream_url(&it.id),
                    title: non_empty(it.name.take()).unwrap_or_else(|| "Untitled".into()),
                    subtitle: join_dot([artist.as_deref(), album.as_deref()]),
                    duration: it.seconds(),
                    artist,
                    album,
                    source: SourceKind::Url,
                }
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::super::query_param;
    use super::super::testutil::{agent, request_line, MockServer, Reply};
    use super::*;

    fn session() -> JellyfinClient {
        JellyfinClient::with_session("https://jf.example.com/", "tok123", "user1", None)
    }

    #[test]
    fn stream_url_and_item_id_round_trip() {
        let c = session();
        let url = c.stream_url("abc/def");
        assert!(url.starts_with(
            "https://jf.example.com/Audio/abc%2Fdef/universal?UserId=user1&DeviceId=rustune-"
        ));
        assert_eq!(query_param(&url, "api_key").as_deref(), Some("tok123"));
        assert_eq!(
            query_param(&url, "Container").as_deref(),
            Some(DIRECT_CONTAINERS)
        );
        assert_eq!(
            query_param(&url, "MaxStreamingBitrate").as_deref(),
            Some("320000")
        );
        let item = MediaItem {
            id: url,
            title: String::new(),
            artist: None,
            album: None,
            duration: None,
            subtitle: None,
            source: SourceKind::Url,
        };
        assert_eq!(c.item_id(&item).as_deref(), Some("abc/def"));
        let dl = MediaItem {
            id: c.download_url("42"),
            ..item.clone()
        };
        assert_eq!(
            dl.id,
            "https://jf.example.com/Items/42/Download?api_key=tok123"
        );
        assert_eq!(c.item_id(&dl).as_deref(), Some("42"));
        let foreign = MediaItem {
            id: "https://other.example/Audio/1/universal".into(),
            ..item
        };
        assert_eq!(c.item_id(&foreign), None);
    }

    #[test]
    fn auth_header_format() {
        assert_eq!(
            auth_header("dev", Some("t")),
            format!(
                "MediaBrowser Client=\"rustune\", Device=\"rustune\", DeviceId=\"dev\", Version=\"{}\", Token=\"t\"",
                env!("CARGO_PKG_VERSION")
            )
        );
        assert!(!auth_header("dev", None).contains("Token"));
    }

    #[test]
    fn device_id_is_stable_and_distinct() {
        assert_eq!(device_id("https://a", "u"), device_id("https://a", "u"));
        assert_ne!(device_id("https://a", "u"), device_id("https://a", "v"));
        assert_eq!(device_id("x", "y").len(), "rustune-".len() + 16);
    }

    #[test]
    fn ticks_conversion() {
        assert_eq!(ticks(Duration::from_secs(2)), 20_000_000);
        assert_eq!(ticks(Duration::from_millis(1)), 10_000);
    }

    #[test]
    fn parses_items_into_tracks_and_albums() {
        let body = br#"{"Items":[
          {"Name":"So What","Id":"t1","Type":"Audio","RunTimeTicks":5620000000,"Album":"Kind of Blue",
           "AlbumArtist":"Miles Davis","Artists":["Miles Davis","John Coltrane"],"IndexNumber":1,
           "ProductionYear":1959,"MediaType":"Audio"},
          {"Name":null,"Id":"t2","Type":"Audio","Artists":null,"AlbumArtist":"Fallback"},
          {"Name":"ghost","Id":""}],
          "TotalRecordCount":3,"StartIndex":0}"#;
        let resp: ItemsResponse = parse_json(body).unwrap();
        let tracks = session().tracks(resp.items);
        assert_eq!(tracks.len(), 2);
        assert_eq!(tracks[0].title, "So What");
        assert_eq!(tracks[0].artist.as_deref(), Some("Miles Davis"));
        assert_eq!(tracks[0].duration, Some(562));
        assert_eq!(
            tracks[0].subtitle.as_deref(),
            Some("Miles Davis · Kind of Blue")
        );
        assert_eq!(tracks[1].title, "Untitled");
        assert_eq!(tracks[1].artist.as_deref(), Some("Fallback"));
        assert_eq!(tracks[1].duration, None);
    }

    #[test]
    fn debug_hides_secrets() {
        let cfg = JellyfinConfig {
            password: "hunter2".into(),
            token: Some("tok-secret".into()),
            ..JellyfinConfig::default()
        };
        let s = format!("{cfg:?}");
        assert!(!s.contains("hunter2") && !s.contains("tok-secret"));
        assert!(!format!("{:?}", session()).contains("tok123"));
    }

    #[test]
    fn connect_with_password_round_trip() {
        let auth = r#"{"User":{"Name":"alice","Id":"u-42","Policy":{}},"SessionInfo":{},
            "AccessToken":"sess-token","ServerId":"srv"}"#;
        let albums = r#"{"Items":[{"Name":"LP","Id":"al1","Type":"MusicAlbum","AlbumArtist":"Band",
            "ProductionYear":2001,"ChildCount":11}],"TotalRecordCount":1}"#;
        let server = MockServer::start(vec![Reply::json(auth), Reply::json(albums)]);
        let cfg = JellyfinConfig {
            url: server.base.clone(),
            user: "alice".into(),
            password: "pw".into(),
            ..JellyfinConfig::default()
        };
        let agent = agent();
        let client = JellyfinClient::connect(&agent, &cfg).unwrap();
        assert_eq!(client.token(), "sess-token");
        assert_eq!(client.user_id(), "u-42");
        let list = client.albums(&agent, AlbumSort::Year, 0, 50).unwrap();
        assert_eq!(
            list,
            vec![Album {
                id: "al1".into(),
                name: "LP".into(),
                artist: Some("Band".into()),
                year: Some(2001),
                track_count: Some(11),
            }]
        );
        let requests = server.requests();
        assert_eq!(
            request_line(&requests[0]),
            "POST /Users/AuthenticateByName HTTP/1.1"
        );
        // ureq pretty-prints JSON bodies.
        let body: serde_json::Value =
            serde_json::from_str(&requests[0][requests[0].find("\r\n\r\n").unwrap() + 4..])
                .unwrap();
        assert_eq!(body, serde_json::json!({"Username": "alice", "Pw": "pw"}));
        assert!(requests[0].contains("MediaBrowser Client=\"rustune\""));
        assert!(request_line(&requests[1]).starts_with(
            "GET /Items?userId=u-42&includeItemTypes=MusicAlbum&recursive=true&sortBy=ProductionYear%2CSortName&sortOrder=Descending"
        ));
        assert!(requests[1].contains("Token=\"sess-token\""));
    }

    #[test]
    fn api_key_discovers_user_from_list() {
        let users = r#"[{"Name":"bob","Id":"u-1"},{"Name":"Alice","Id":"u-2"}]"#;
        let server = MockServer::start(vec![Reply::with_status(400, ""), Reply::json(users)]);
        let cfg = JellyfinConfig {
            url: server.base.clone(),
            user: "alice".into(),
            token: Some("api-key".into()),
            ..JellyfinConfig::default()
        };
        let client = JellyfinClient::connect(&agent(), &cfg).unwrap();
        assert_eq!(client.user_id(), "u-2");
        let requests = server.requests();
        assert_eq!(request_line(&requests[0]), "GET /Users/Me HTTP/1.1");
        assert_eq!(request_line(&requests[1]), "GET /Users HTTP/1.1");
    }

    #[test]
    fn rejected_login_is_auth_error() {
        let server = MockServer::start(vec![Reply::with_status(401, "")]);
        let cfg = JellyfinConfig {
            url: server.base.clone(),
            user: "alice".into(),
            password: "nope".into(),
            ..JellyfinConfig::default()
        };
        assert!(matches!(
            JellyfinClient::connect(&agent(), &cfg),
            Err(ProviderError::Auth(_))
        ));
        server.requests();
    }

    #[test]
    #[ignore = "network: needs JELLYFIN_URL/JELLYFIN_USER/JELLYFIN_PASSWORD"]
    fn live_server() {
        let (Ok(url), Ok(user)) = (
            std::env::var("JELLYFIN_URL"),
            std::env::var("JELLYFIN_USER"),
        ) else {
            return;
        };
        let cfg = JellyfinConfig {
            url,
            user,
            password: std::env::var("JELLYFIN_PASSWORD").unwrap_or_default(),
            ..JellyfinConfig::default()
        };
        let agent = agent();
        let client = JellyfinClient::connect(&agent, &cfg).unwrap();
        client.server_info(&agent).unwrap();
        client.albums(&agent, AlbumSort::Name, 0, 5).unwrap();
    }
}
