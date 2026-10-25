//! Wire format: one JSON object per line in each direction.
//!
//! Requests:
//! - a [`Command`] (`{"cmd":"next"}`, `{"cmd":"set_volume","arg":0.5}`),
//!   answered with `{"ok":true}` once applied;
//! - `{"get":"status"}` / `{"subscribe":true}` → [`Status`];
//! - `{"get":"queue"}` → [`QueueReply`], `{"get":"lyrics"}` → [`LyricsReply`],
//!   `{"get":"screen"}` → [`Screen`] (TUI only);
//! - `{"keys":["j","ctrl+k"]}` → `{"ok":true}` once the keys were handled
//!   (TUI only);
//! - `{"search":{"source":"local","query":"x","limit":10,"then":"show"}}` →
//!   [`SearchReply`] (`then`: `"show"`, `"queue"` or `{"play":N}`);
//! - `{"skin":"name|md5|/path/skin.wsz"}` → `{"ok":true,"skin":..,"path":..}`.
//!
//! Positions in replies and in `then.play` are 1-based; `cmd` arguments
//! (`play_index`, `remove_from_queue`, ...) are 0-based queue indexes.
//! Failures are `{"error":"..."}`.

use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::app::{App, Mode, Tab};
use crate::browse::Entry;
use crate::command::Command;
use crate::media::{MediaItem, SourceKind};
use crate::providers::ProviderKind;
use crate::view::{PlayStatus, RepeatMode};

pub const OK: &str = r#"{"ok":true}"#;

/// Where a deferred answer (one JSON line) goes back to the IPC client.
pub type Reply = std::sync::mpsc::SyncSender<String>;

#[derive(Debug, Clone, PartialEq)]
pub enum Request {
    Command(Command),
    Status,
    Subscribe,
    Query(Query),
}

/// Requests the player answers itself, possibly after background work.
#[derive(Debug, Clone, PartialEq)]
pub enum Query {
    Queue,
    Lyrics,
    Screen,
    /// Key specs (see [`super::keys`]), injected as terminal input.
    Keys(Vec<String>),
    Search(Search),
    /// Installed skin name, Winamp Skin Museum md5, or a `.wsz` path.
    Skin(String),
}

impl Query {
    /// How long the server waits for the player's answer.
    pub fn timeout(&self) -> Duration {
        Duration::from_secs(match self {
            Self::Queue | Self::Screen | Self::Keys(_) => 10,
            Self::Lyrics => 30,
            // yt-dlp searches and museum downloads.
            Self::Search(_) | Self::Skin(_) => 180,
        })
    }
}

/// `rustune search SOURCE QUERY`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Search {
    pub source: SearchSource,
    pub query: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limit: Option<usize>,
    #[serde(default)]
    pub then: Then,
}

/// What to do with search results besides returning them.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Then {
    #[default]
    Show,
    /// Append every playable result to the queue.
    Queue,
    /// Replace the queue with the results and play result N (1-based). A
    /// podcast show or an album opens and its first (newest) track plays.
    Play(usize),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SearchSource {
    Local,
    Online,
    Radio,
    Podcasts,
    Subsonic,
    Jellyfin,
}

impl SearchSource {
    pub const ALL: [Self; 6] = [
        Self::Local,
        Self::Online,
        Self::Radio,
        Self::Podcasts,
        Self::Subsonic,
        Self::Jellyfin,
    ];

    pub fn name(self) -> &'static str {
        match self {
            Self::Local => "local",
            Self::Online => "online",
            Self::Radio => "radio",
            Self::Podcasts => "podcasts",
            Self::Subsonic => "subsonic",
            Self::Jellyfin => "jellyfin",
        }
    }

    pub fn from_name(name: &str) -> Option<Self> {
        Self::ALL
            .into_iter()
            .find(|s| s.name().eq_ignore_ascii_case(name.trim()))
    }

    pub fn provider(self) -> Option<ProviderKind> {
        match self {
            Self::Local | Self::Online => None,
            Self::Radio => Some(ProviderKind::Radio),
            Self::Podcasts => Some(ProviderKind::Podcast),
            Self::Subsonic => Some(ProviderKind::Subsonic),
            Self::Jellyfin => Some(ProviderKind::Jellyfin),
        }
    }

    fn of_tab(tab: Tab) -> Self {
        match tab {
            Tab::Local => Self::Local,
            Tab::Online => Self::Online,
            Tab::Provider(ProviderKind::Radio) => Self::Radio,
            Tab::Provider(ProviderKind::Podcast) => Self::Podcasts,
            Tab::Provider(ProviderKind::Subsonic) => Self::Subsonic,
            Tab::Provider(ProviderKind::Jellyfin) => Self::Jellyfin,
        }
    }
}

impl Request {
    /// The request as one JSON line (without the newline).
    pub fn encode(&self) -> String {
        let value = match self {
            Self::Command(cmd) => return serde_json::to_string(cmd).unwrap_or_default(),
            Self::Status => serde_json::json!({ "get": "status" }),
            Self::Subscribe => serde_json::json!({ "subscribe": true }),
            Self::Query(Query::Queue) => serde_json::json!({ "get": "queue" }),
            Self::Query(Query::Lyrics) => serde_json::json!({ "get": "lyrics" }),
            Self::Query(Query::Screen) => serde_json::json!({ "get": "screen" }),
            Self::Query(Query::Keys(keys)) => serde_json::json!({ "keys": keys }),
            Self::Query(Query::Search(search)) => serde_json::json!({ "search": search }),
            Self::Query(Query::Skin(skin)) => serde_json::json!({ "skin": skin }),
        };
        value.to_string()
    }

    pub fn parse(line: &str) -> Result<Self, String> {
        let value: Value = serde_json::from_str(line).map_err(|e| format!("invalid JSON: {e}"))?;
        let Value::Object(map) = &value else {
            return Err("a request must be a JSON object".into());
        };
        if map.contains_key("cmd") {
            return serde_json::from_value(value)
                .map(Self::Command)
                .map_err(|e| format!("bad command: {e}"));
        }
        if let Some(what) = map.get("get") {
            return match what.as_str() {
                Some("status") => Ok(Self::Status),
                Some("queue") => Ok(Self::Query(Query::Queue)),
                Some("lyrics") => Ok(Self::Query(Query::Lyrics)),
                Some("screen") => Ok(Self::Query(Query::Screen)),
                _ => Err(format!(
                    "cannot get {what} (status, queue, lyrics or screen)"
                )),
            };
        }
        if let Some(keys) = map.get("keys") {
            let keys: Vec<String> = serde_json::from_value(keys.clone())
                .map_err(|_| "\"keys\" must be a list of key names".to_string())?;
            if keys.is_empty() {
                return Err("\"keys\" is empty".into());
            }
            for key in &keys {
                super::keys::parse_key(key)?;
            }
            return Ok(Self::Query(Query::Keys(keys)));
        }
        if let Some(search) = map.get("search") {
            let search: Search =
                serde_json::from_value(search.clone()).map_err(|e| format!("bad search: {e}"))?;
            if search.query.trim().is_empty() {
                return Err("bad search: the query is empty".into());
            }
            if search.then == Then::Play(0) || search.limit == Some(0) {
                return Err("bad search: positions and limits start at 1".into());
            }
            return Ok(Self::Query(Query::Search(search)));
        }
        if let Some(skin) = map.get("skin") {
            return match skin.as_str().map(str::trim) {
                Some(name) if !name.is_empty() => Ok(Self::Query(Query::Skin(name.to_string()))),
                _ => Err("\"skin\" must be a skin name, md5 or .wsz path".into()),
            };
        }
        match map.get("subscribe") {
            Some(Value::Bool(true)) => Ok(Self::Subscribe),
            _ => Err(
                r#"expected {"cmd":...}, {"get":...}, {"subscribe":true}, {"keys":[...]}, {"search":{...}} or {"skin":...}"#
                    .to_string(),
            ),
        }
    }
}

/// `{"error":"<message>"}`.
pub fn error_line(message: &str) -> String {
    serde_json::json!({ "error": message }).to_string()
}

/// A reply line: `Err` carries the server's `error` message.
pub fn check_reply(line: &str) -> Result<Value, String> {
    let value: Value =
        serde_json::from_str(line.trim()).map_err(|e| format!("bad reply from rustune: {e}"))?;
    match value.get("error") {
        Some(Value::String(e)) => Err(e.clone()),
        Some(other) => Err(other.to_string()),
        None => Ok(value),
    }
}

/// Player state as reported by `{"get":"status"}` and status events.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Status {
    pub status: PlayStatus,
    /// Track title (or the stream's now-playing title); empty when the
    /// queue has no current item.
    pub title: String,
    pub artist: Option<String>,
    pub album: Option<String>,
    /// Whole seconds.
    pub elapsed: u64,
    /// Whole seconds, when known.
    pub duration: Option<u64>,
    /// 0..=100
    pub volume: u8,
    /// -100 (left) ..= 100 (right)
    pub balance: i8,
    pub shuffle: bool,
    pub repeat: RepeatMode,
    pub eq_enabled: bool,
    pub queue_length: usize,
    /// Index of the current track in the queue.
    pub queue_index: Option<usize>,
    /// Source tab: local, online, radio, podcasts, subsonic or jellyfin.
    pub source: String,
    /// What the TUI shows: browse, palette, input, settings, skin_browser
    /// or onboarding.
    pub view: String,
    pub theme: String,
    /// Visualizer mode name.
    pub vis: String,
    /// A yt-dlp lookup for the current track is in flight.
    pub resolving: bool,
    /// The most recent error message shown to the user.
    pub last_error: Option<String>,
}

/// `{"get":"queue"}`.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct QueueReply {
    pub items: Vec<QueueEntry>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct QueueEntry {
    /// 1-based position.
    pub index: usize,
    pub title: String,
    pub artist: Option<String>,
    pub album: Option<String>,
    pub duration: Option<u64>,
    /// local, url or the extractor name (ytdlp).
    pub source: String,
    pub id: String,
    /// The playing (or paused) track.
    pub current: bool,
    /// The playlist cursor.
    pub cursor: bool,
}

impl QueueReply {
    pub fn of(app: &App) -> Self {
        let queue = &app.queue;
        let items = queue
            .items()
            .iter()
            .enumerate()
            .map(|(i, item)| QueueEntry {
                index: i + 1,
                title: item.title.clone(),
                artist: item.artist.clone(),
                album: item.album.clone(),
                duration: item.duration,
                source: source_name(&item.source).to_string(),
                id: item.id.clone(),
                current: queue.current_index() == Some(i),
                cursor: queue.cursor() == i,
            })
            .collect();
        Self { items }
    }
}

pub fn source_name(source: &SourceKind) -> &str {
    match source {
        SourceKind::Local => "local",
        SourceKind::Url => "url",
        SourceKind::Extractor(name) => name,
    }
}

/// `{"search":{...}}`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SearchReply {
    pub source: SearchSource,
    pub query: String,
    pub results: Vec<Hit>,
    /// `then: "queue"`: how many tracks were queued.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub queued: Option<usize>,
    /// `then: {"play":N}`: the show or album that was opened, if any.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub opened: Option<String>,
    /// `then: {"play":N}`: the track that started.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub playing: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum HitKind {
    /// Playable: song, video, station or episode.
    Track,
    /// A podcast: `then: {"play":N}` plays its newest episode.
    Show,
    /// An album, playlist or category: `then: {"play":N}` plays its first track.
    Folder,
}

/// One search result.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Hit {
    /// 1-based position (what `then: {"play":N}` takes).
    pub index: usize,
    pub kind: HitKind,
    pub title: String,
    pub artist: Option<String>,
    pub album: Option<String>,
    pub duration: Option<u64>,
    /// Channel, station tags, show author, ...
    pub subtitle: Option<String>,
    /// Tracks: local, url or ytdlp.
    pub source: Option<String>,
    /// Tracks: path, URL or video id; shows: the feed URL.
    pub id: Option<String>,
}

impl Hit {
    /// `None` for rows that are not results (menus, the search entry).
    pub fn of(index: usize, entry: &Entry) -> Option<Self> {
        let hit = |kind, title: &str, subtitle: Option<&str>| Self {
            index,
            kind,
            title: title.to_string(),
            artist: None,
            album: None,
            duration: None,
            subtitle: subtitle.map(str::to_string),
            source: None,
            id: None,
        };
        Some(match entry {
            Entry::Track { item, .. } => Self::track(index, item),
            Entry::Podcast {
                title,
                feed_url,
                detail,
            } => Self {
                id: Some(feed_url.clone()),
                ..hit(HitKind::Show, title, detail.as_deref())
            },
            Entry::Folder { label, detail, .. } => hit(HitKind::Folder, label, detail.as_deref()),
            Entry::Menu { .. } | Entry::Search | Entry::Subscriptions => return None,
        })
    }

    pub fn track(index: usize, item: &MediaItem) -> Self {
        Self {
            index,
            kind: HitKind::Track,
            title: item.title.clone(),
            artist: item.artist.clone(),
            album: item.album.clone(),
            duration: item.duration,
            subtitle: item.subtitle.clone(),
            source: Some(source_name(&item.source).to_string()),
            id: Some(item.id.clone()),
        }
    }
}

/// `{"get":"lyrics"}`.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct LyricsReply {
    pub found: bool,
    /// The track the lyrics belong to.
    pub title: String,
    /// Why there are none (`found: false`).
    pub message: Option<String>,
    pub instrumental: bool,
    /// Timed lines, when the lyrics are synced.
    pub synced: Vec<LyricLine>,
    pub plain: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LyricLine {
    /// Seconds from the start of the track.
    pub time: f64,
    pub text: String,
}

impl LyricsReply {
    pub fn none(title: &str, message: impl Into<String>) -> Self {
        Self {
            title: title.to_string(),
            message: Some(message.into()),
            ..Self::default()
        }
    }

    pub fn found(title: &str, lyrics: &crate::providers::lyrics::Lyrics) -> Self {
        Self {
            found: true,
            title: title.to_string(),
            message: None,
            instrumental: lyrics.instrumental,
            synced: lyrics
                .synced
                .iter()
                .map(|(at, text)| LyricLine {
                    time: at.as_secs_f64(),
                    text: text.clone(),
                })
                .collect(),
            plain: lyrics.plain.clone(),
        }
    }
}

/// `{"get":"screen"}`: the last frame the TUI drew.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Screen {
    pub width: u16,
    pub height: u16,
    /// One line per row, trailing spaces trimmed.
    pub text: String,
    /// The same rows with ANSI colours and attributes.
    pub ansi: String,
    /// Text cursor of a focused prompt (0-based cell), if any.
    pub cursor: Option<Cursor>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Cursor {
    pub x: u16,
    pub y: u16,
}

/// A reply value as one JSON line.
pub fn reply_line<T: Serialize>(value: &T) -> String {
    serde_json::to_string(value)
        .unwrap_or_else(|e| error_line(&format!("cannot encode reply: {e}")))
}

fn view_name(app: &App) -> &'static str {
    match app.mode {
        Mode::Browse if app.palette.is_some() => "palette",
        Mode::Browse => "browse",
        Mode::Input => "input",
        Mode::Settings => "settings",
        Mode::SkinBrowser => "skin_browser",
        Mode::Onboarding => "onboarding",
    }
}

fn set_str(dst: &mut String, src: &str) -> bool {
    if dst == src {
        return false;
    }
    dst.clear();
    dst.push_str(src);
    true
}

fn set_opt(dst: &mut Option<String>, src: Option<&str>) -> bool {
    match (dst.as_mut(), src) {
        (Some(d), Some(s)) => set_str(d, s),
        (None, None) => false,
        _ => {
            *dst = src.map(str::to_string);
            true
        }
    }
}

fn set<T: PartialEq>(dst: &mut T, src: T) -> bool {
    if *dst == src {
        return false;
    }
    *dst = src;
    true
}

impl Status {
    /// Refresh from `app` in place (no allocation unless a text field
    /// changed); returns whether anything changed.
    pub fn update(&mut self, app: &App) -> bool {
        let item = app.queue.current();
        let title = match (&app.playback, item) {
            (Some(pb), Some(item)) if shows_item(&pb.title, item) => item.title.as_str(),
            (Some(pb), _) => pb.title.as_str(),
            (None, Some(item)) => item.title.as_str(),
            (None, None) => "",
        };
        let (elapsed, duration) = match &app.playback {
            Some(pb) => (pb.elapsed.as_secs(), pb.duration.map(|d| d.as_secs())),
            None => (0, item.and_then(|i| i.duration)),
        };
        let mut changed = set(&mut self.status, app.play_status());
        changed |= set_str(&mut self.title, title);
        changed |= set_opt(&mut self.artist, item.and_then(|i| i.artist.as_deref()));
        changed |= set_opt(&mut self.album, item.and_then(|i| i.album.as_deref()));
        changed |= set(&mut self.elapsed, elapsed);
        changed |= set(&mut self.duration, duration);
        changed |= set(&mut self.volume, percent(app.volume, 0.0) as u8);
        changed |= set(&mut self.balance, percent(app.balance, -1.0) as i8);
        changed |= set(&mut self.shuffle, app.queue.shuffle());
        changed |= set(&mut self.repeat, app.queue.repeat());
        changed |= set(&mut self.eq_enabled, app.eq.enabled);
        changed |= set(&mut self.queue_length, app.queue.len());
        changed |= set(&mut self.queue_index, app.queue.current_index());
        changed |= set_str(&mut self.source, SearchSource::of_tab(app.tab).name());
        changed |= set_str(&mut self.view, view_name(app));
        changed |= set_str(&mut self.theme, &app.theme.name);
        changed |= set_str(&mut self.vis, app.vis_mode.name());
        changed |= set(&mut self.resolving, app.resolving.is_some());
        changed |= set_opt(&mut self.last_error, app.last_error.as_deref());
        changed
    }
}

/// `v` (clamped to `min..=1.0`) in whole percent.
fn percent(v: f32, min: f32) -> f32 {
    if v.is_finite() {
        (v.clamp(min, 1.0) * 100.0).round()
    } else {
        0.0
    }
}

/// Whether the now-playing title is just the item's "Artist - Title".
fn shows_item(now_playing: &str, item: &crate::media::MediaItem) -> bool {
    match &item.artist {
        Some(artist) => {
            now_playing
                .strip_prefix(artist.as_str())
                .and_then(|rest| rest.strip_prefix(" - "))
                == Some(item.title.as_str())
        }
        None => now_playing == item.title,
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;
    use crate::app::PlaybackState;
    use crate::config::Config;
    use crate::view::StreamFormat;

    fn item(n: usize, artist: Option<&str>) -> MediaItem {
        MediaItem {
            id: format!("/music/{n}.flac"),
            title: format!("Song {n}"),
            artist: artist.map(str::to_string),
            album: Some("Album".into()),
            duration: Some(200),
            subtitle: None,
            source: SourceKind::Local,
        }
    }

    #[test]
    fn requests_round_trip() {
        let cases = [
            Request::Command(Command::TogglePause),
            Request::Command(Command::SetVolume(0.25)),
            Request::Command(Command::SeekBy(-10)),
            Request::Command(Command::SeekTo(Duration::from_secs(90))),
            Request::Command(Command::SetRepeat(RepeatMode::One)),
            Request::Command(Command::EqPreset("Rock".into())),
            Request::Command(Command::SetEqBand { band: 2, db: -3.0 }),
            Request::Command(Command::Enqueue(vec![
                "/a.mp3".into(),
                "https://x/s".into(),
            ])),
            Request::Command(Command::SetVis(crate::vis::VisMode::Scope)),
            Request::Command(Command::RemoveFromQueue(2)),
            Request::Command(Command::MoveInQueue { from: 0, to: 3 }),
            Request::Command(Command::ClearQueue),
            Request::Command(Command::PlayIndex(4)),
            Request::Command(Command::SetTheme("winamp".into())),
            Request::Status,
            Request::Subscribe,
            Request::Query(Query::Queue),
            Request::Query(Query::Lyrics),
            Request::Query(Query::Screen),
            Request::Query(Query::Keys(vec![
                "j".into(),
                "ctrl+k".into(),
                "Enter".into(),
            ])),
            Request::Query(Query::Search(Search {
                source: SearchSource::Podcasts,
                query: "rust news".into(),
                limit: Some(5),
                then: Then::Play(2),
            })),
            Request::Query(Query::Search(Search {
                source: SearchSource::Online,
                query: "x".into(),
                limit: None,
                then: Then::Queue,
            })),
            Request::Query(Query::Skin("base-2.91".into())),
        ];
        for req in cases {
            let line = req.encode();
            assert!(!line.contains('\n'), "{line}");
            assert_eq!(Request::parse(&line), Ok(req), "{line}");
        }
    }

    #[test]
    fn wire_names_are_stable() {
        assert_eq!(
            Request::Command(Command::Next).encode(),
            r#"{"cmd":"next"}"#
        );
        assert_eq!(
            Request::Command(Command::SetVolume(0.5)).encode(),
            r#"{"cmd":"set_volume","arg":0.5}"#
        );
        assert_eq!(
            Request::parse(r#"{"cmd":"set_repeat","arg":"all"}"#),
            Ok(Request::Command(Command::SetRepeat(RepeatMode::All)))
        );
        assert_eq!(
            Request::parse(r#" {"get":"status"} "#.trim()),
            Ok(Request::Status)
        );
    }

    #[test]
    fn bad_requests_explain_themselves() {
        let err = |s: &str| Request::parse(s).unwrap_err();
        assert!(err("not json").contains("invalid JSON"));
        assert!(err("[1]").contains("JSON object"));
        assert!(err(r#"{"cmd":"dance"}"#).contains("bad command"));
        assert!(err(r#"{"cmd":"set_volume"}"#).contains("bad command"));
        assert!(err(r#"{"get":"weather"}"#).contains("status, queue, lyrics or screen"));
        assert!(err(r#"{"keys":[]}"#).contains("empty"));
        assert!(err(r#"{"keys":["Enterr"]}"#).contains("bad key"));
        assert!(err(r#"{"keys":"j"}"#).contains("list"));
        assert!(err(r#"{"search":{"source":"mars","query":"x"}}"#).contains("bad search"));
        assert!(err(r#"{"search":{"source":"local","query":" "}}"#).contains("empty"));
        assert!(
            err(r#"{"search":{"source":"local","query":"x","then":{"play":0}}}"#)
                .contains("start at 1")
        );
        assert!(err(r#"{"skin":""}"#).contains("skin"));
        assert!(err(r#"{"subscribe":false}"#).contains("expected"));
        assert_eq!(
            check_reply(&error_line("nope \"quoted\"")),
            Err("nope \"quoted\"".to_string())
        );
        assert!(check_reply(OK).is_ok());
    }

    #[test]
    fn status_json_shape() {
        let json = serde_json::to_value(Status::default()).unwrap();
        let keys: Vec<&str> = json
            .as_object()
            .unwrap()
            .keys()
            .map(String::as_str)
            .collect();
        let mut expected = vec![
            "status",
            "title",
            "artist",
            "album",
            "elapsed",
            "duration",
            "volume",
            "balance",
            "shuffle",
            "repeat",
            "eq_enabled",
            "queue_length",
            "queue_index",
            "source",
            "view",
            "theme",
            "vis",
            "resolving",
            "last_error",
        ];
        expected.sort_unstable();
        let mut keys = keys;
        keys.sort_unstable();
        assert_eq!(keys, expected);
        assert_eq!(json["status"], "stopped");
        assert_eq!(json["repeat"], "off");
        assert_eq!(json["queue_index"], Value::Null);
    }

    #[test]
    fn status_follows_the_app() {
        let mut app = App::new(Config {
            onboarding_done: true,
            ..Config::default()
        });
        let mut status = Status::default();
        app.volume = 0.8;
        app.balance = -0.25;
        app.queue
            .replace(vec![item(0, Some("Band")), item(1, None)], Some(1));
        assert!(status.update(&app));
        assert!(!status.update(&app), "no change, no event");
        assert_eq!(status.status, PlayStatus::Stopped);
        assert_eq!(status.title, "Song 1");
        assert_eq!(status.artist, None);
        assert_eq!(status.duration, Some(200));
        assert_eq!((status.volume, status.balance), (80, -25));
        assert_eq!((status.queue_length, status.queue_index), (2, Some(1)));

        app.queue.jump(0);
        app.playback = Some(PlaybackState {
            title: "Band - Song 0".into(),
            duration: Some(Duration::from_secs(201)),
            elapsed: Duration::from_millis(61_900),
            paused: true,
            format: StreamFormat::default(),
        });
        assert!(status.update(&app));
        assert_eq!(status.status, PlayStatus::Paused);
        assert_eq!(status.title, "Song 0");
        assert_eq!(status.artist.as_deref(), Some("Band"));
        assert_eq!(status.album.as_deref(), Some("Album"));
        assert_eq!((status.elapsed, status.duration), (61, Some(201)));

        // A stream's now-playing title wins over the item title.
        if let Some(pb) = &mut app.playback {
            pb.title = "Live: Other Song".into();
        }
        assert!(status.update(&app));
        assert_eq!(status.title, "Live: Other Song");

        assert_eq!(status.source, "local");
        assert_eq!(status.view, "browse");
        assert_eq!(status.vis, app.vis_mode.name());
        assert!(!status.resolving);
        app.tab = Tab::Online;
        let _ = app.handle_browse_key(crate::ipc::keys::parse_key("?").unwrap());
        app.error("Search failed: boom");
        app.info("later info");
        assert!(status.update(&app));
        assert_eq!(status.source, "online");
        assert_eq!(status.view, "palette");
        assert_eq!(status.last_error.as_deref(), Some("Search failed: boom"));

        let json = serde_json::to_value(&status).unwrap();
        assert_eq!(json["status"], "paused");
        assert_eq!(json["volume"], 80);
        assert_eq!(json["last_error"], "Search failed: boom");
        let back: Status = serde_json::from_value(json).unwrap();
        assert_eq!(back, status);
    }

    #[test]
    fn queue_reply_marks_current_and_cursor() {
        let mut app = App::new(Config::default());
        app.queue
            .replace(vec![item(0, None), item(1, Some("B"))], Some(1));
        app.queue.set_cursor(0);
        let reply = QueueReply::of(&app);
        assert_eq!(reply.items.len(), 2);
        assert_eq!(reply.items[0].index, 1);
        assert!(reply.items[0].cursor && !reply.items[0].current);
        assert!(reply.items[1].current && !reply.items[1].cursor);
        assert_eq!(reply.items[1].source, "local");
        let json = serde_json::to_value(&reply).unwrap();
        assert_eq!(json["items"][1]["artist"], "B");
        assert_eq!(serde_json::from_value::<QueueReply>(json).unwrap(), reply);
    }

    #[test]
    fn hits_describe_tracks_shows_and_folders() {
        let track = Entry::Track {
            item: item(3, Some("A")),
            radio_uuid: None,
        };
        let show = Entry::Podcast {
            title: "Show".into(),
            feed_url: "https://f.example/rss".into(),
            detail: Some("Host".into()),
        };
        let hit = Hit::of(1, &track).unwrap();
        assert_eq!((hit.kind, hit.index), (HitKind::Track, 1));
        assert_eq!(hit.id.as_deref(), Some("/music/3.flac"));
        let hit = Hit::of(2, &show).unwrap();
        assert_eq!(hit.kind, HitKind::Show);
        assert_eq!(hit.id.as_deref(), Some("https://f.example/rss"));
        assert_eq!(Hit::of(3, &Entry::Search), None);
        let json = serde_json::to_value(&hit).unwrap();
        assert_eq!(json["kind"], "show");
    }
}
