//! Wire format: one JSON object per line in each direction.
//!
//! Requests: a [`Command`] (`{"cmd":"next"}`, `{"cmd":"set_volume","arg":0.5}`),
//! `{"get":"status"}` or `{"subscribe":true}`. Replies: `{"ok":true}` for a
//! command, a [`Status`] object for `get`/`subscribe`, `{"error":"..."}` on
//! failure.

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::app::App;
use crate::command::Command;
use crate::view::{PlayStatus, RepeatMode};

pub const OK: &str = r#"{"ok":true}"#;

#[derive(Debug, Clone, PartialEq)]
pub enum Request {
    Command(Command),
    Status,
    Subscribe,
}

impl Request {
    /// The request as one JSON line (without the newline).
    pub fn encode(&self) -> String {
        match self {
            Self::Command(cmd) => serde_json::to_string(cmd).unwrap_or_default(),
            Self::Status => r#"{"get":"status"}"#.to_string(),
            Self::Subscribe => r#"{"subscribe":true}"#.to_string(),
        }
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
        match (map.get("get"), map.get("subscribe")) {
            (Some(Value::String(what)), None) if what == "status" => Ok(Self::Status),
            (Some(other), None) => Err(format!("cannot get {other} (only \"status\")")),
            (None, Some(Value::Bool(true))) => Ok(Self::Subscribe),
            _ => Err(r#"expected {"cmd":...}, {"get":"status"} or {"subscribe":true}"#.to_string()),
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
    use crate::media::{MediaItem, SourceKind};
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
            Request::Status,
            Request::Subscribe,
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
        assert!(err(r#"{"get":"lyrics"}"#).contains("only \"status\""));
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
        let mut app = App::new(Config::default());
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

        let json = serde_json::to_value(&status).unwrap();
        assert_eq!(json["status"], "paused");
        assert_eq!(json["volume"], 80);
        let back: Status = serde_json::from_value(json).unwrap();
        assert_eq!(back, status);
    }
}
