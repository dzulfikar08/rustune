//! Session persistence (`<config dir>/rustune/state.toml`): volume, balance,
//! EQ, shuffle/repeat, skin, theme, visualizer mode and the last queue with
//! resume position.

use std::path::Path;

use anyhow::Result;
use serde::{Deserialize, Serialize};

use crate::media::{MediaItem, SourceKind};
use crate::view::{EqSettings, RepeatMode, EQ_BANDS, EQ_MAX_DB};
use crate::vis::VisMode;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct SessionState {
    /// 0.0..=1.0
    pub volume: f32,
    /// -1.0..=1.0
    pub balance: f32,
    pub shuffle: bool,
    pub repeat: RepeatMode,
    /// File name of the selected skin inside the skins directory.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub skin: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub theme: Option<String>,
    /// Index into `queue` of the last current track.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub queue_index: Option<usize>,
    /// Playback position of that track, for resume.
    pub position_secs: u64,
    /// Visualizer mode (cycled with `V`).
    pub vis: VisMode,
    pub eq: EqSettings,
    pub queue: Vec<QueueEntry>,
}

impl Default for SessionState {
    fn default() -> Self {
        Self {
            volume: 1.0,
            balance: 0.0,
            shuffle: false,
            repeat: RepeatMode::Off,
            skin: None,
            theme: None,
            queue_index: None,
            position_secs: 0,
            vis: VisMode::default(),
            eq: EqSettings::default(),
            queue: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct QueueEntry {
    pub id: String,
    pub title: String,
    /// "local", "url" or the extractor name (e.g. "ytdlp").
    pub source: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub artist: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub album: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub duration: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub subtitle: Option<String>,
}

impl From<&MediaItem> for QueueEntry {
    fn from(item: &MediaItem) -> Self {
        let source = match &item.source {
            SourceKind::Local => "local".to_string(),
            SourceKind::Url => "url".to_string(),
            SourceKind::Extractor(name) => name.clone(),
        };
        Self {
            id: item.id.clone(),
            title: item.title.clone(),
            source,
            artist: item.artist.clone(),
            album: item.album.clone(),
            duration: item.duration,
            subtitle: item.subtitle.clone(),
        }
    }
}

impl From<QueueEntry> for MediaItem {
    fn from(entry: QueueEntry) -> Self {
        let source = match entry.source.as_str() {
            "local" => SourceKind::Local,
            "url" => SourceKind::Url,
            _ => SourceKind::Extractor(entry.source),
        };
        Self {
            id: entry.id,
            title: entry.title,
            artist: entry.artist,
            album: entry.album,
            duration: entry.duration,
            subtitle: entry.subtitle,
            source,
        }
    }
}

fn finite_or(v: f32, fallback: f32) -> f32 {
    if v.is_finite() {
        v
    } else {
        fallback
    }
}

impl SessionState {
    /// Clamp values a hand-edited file could put out of range.
    fn sanitized(mut self) -> Self {
        self.volume = finite_or(self.volume, 1.0).clamp(0.0, 1.0);
        self.balance = finite_or(self.balance, 0.0).clamp(-1.0, 1.0);
        self.eq.preamp_db = finite_or(self.eq.preamp_db, 0.0).clamp(-EQ_MAX_DB, EQ_MAX_DB);
        for band in &mut self.eq.bands_db[..EQ_BANDS] {
            *band = finite_or(*band, 0.0).clamp(-EQ_MAX_DB, EQ_MAX_DB);
        }
        if self.queue_index.is_some_and(|i| i >= self.queue.len()) {
            self.queue_index = None;
        }
        self
    }

    pub fn from_toml(text: &str) -> Result<Self> {
        Ok(toml::from_str::<Self>(text)?.sanitized())
    }

    pub fn to_toml(&self) -> Result<String> {
        Ok(toml::to_string(self)?)
    }

    /// Missing or unreadable state yields defaults (and the error, if any).
    pub fn load(path: &Path) -> (Self, Option<String>) {
        match std::fs::read_to_string(path) {
            Ok(text) => match Self::from_toml(&text) {
                Ok(state) => (state, None),
                Err(e) => (
                    Self::default(),
                    Some(format!("Ignoring invalid {}: {e}", path.display())),
                ),
            },
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => (Self::default(), None),
            Err(e) => (
                Self::default(),
                Some(format!("Cannot read {}: {e}", path.display())),
            ),
        }
    }

    pub fn save(&self, path: &Path) -> Result<()> {
        crate::config::write_atomic(path, self.to_toml()?.as_bytes())?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> SessionState {
        let mut eq = EqSettings {
            enabled: true,
            auto: true,
            preamp_db: -3.5,
            ..EqSettings::default()
        };
        eq.bands_db[0] = 6.0;
        eq.bands_db[9] = -12.0;
        SessionState {
            volume: 0.8,
            balance: -0.25,
            shuffle: true,
            repeat: RepeatMode::One,
            skin: Some("base-2.91.wsz".into()),
            theme: Some("Winamp".into()),
            queue_index: Some(1),
            position_secs: 93,
            vis: VisMode::Spectrogram,
            eq,
            queue: vec![
                QueueEntry::from(&MediaItem {
                    id: "/m/a \"quoted\".flac".into(),
                    title: "A".into(),
                    artist: Some("Band".into()),
                    album: None,
                    duration: Some(200),
                    subtitle: None,
                    source: SourceKind::Local,
                }),
                QueueEntry::from(&MediaItem {
                    id: "xyz".into(),
                    title: "Video".into(),
                    artist: None,
                    album: None,
                    duration: None,
                    subtitle: Some("Channel".into()),
                    source: SourceKind::Extractor("ytdlp".into()),
                }),
                QueueEntry::from(&MediaItem {
                    id: "https://radio.example/live".into(),
                    title: "Radio".into(),
                    artist: None,
                    album: None,
                    duration: None,
                    subtitle: None,
                    source: SourceKind::Url,
                }),
            ],
        }
    }

    #[test]
    fn round_trip_through_toml() {
        let state = sample();
        let text = state.to_toml().unwrap();
        let back = SessionState::from_toml(&text).unwrap();
        assert_eq!(back, state);
        let items: Vec<MediaItem> = back.queue.into_iter().map(MediaItem::from).collect();
        assert_eq!(items[0].source, SourceKind::Local);
        assert_eq!(items[1].source, SourceKind::Extractor("ytdlp".into()));
        assert_eq!(items[2].source, SourceKind::Url);
    }

    #[test]
    fn round_trip_through_disk() {
        let dir = std::env::temp_dir().join(format!("rustune-state-{}", std::process::id()));
        let path = dir.join("nested").join("state.toml");
        let state = sample();
        state.save(&path).unwrap();
        let (back, err) = SessionState::load(&path);
        assert_eq!(err, None);
        assert_eq!(back, state);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn missing_and_partial_files_use_defaults() {
        let (s, err) = SessionState::load(Path::new("/definitely/not/here/state.toml"));
        assert_eq!(s, SessionState::default());
        assert_eq!(err, None);
        let s = SessionState::from_toml("volume = 0.5\n").unwrap();
        assert_eq!(s.volume, 0.5);
        assert_eq!(s.repeat, RepeatMode::Off);
        assert!(s.queue.is_empty());
    }

    #[test]
    fn out_of_range_values_are_clamped() {
        let s = SessionState::from_toml(
            "volume = 7.0\nbalance = -3.0\nqueue_index = 4\nrepeat = \"all\"\n[eq]\nenabled = true\nauto = false\npreamp_db = 99.0\nbands_db = [0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, -40.0]\n",
        )
        .unwrap();
        assert_eq!(s.volume, 1.0);
        assert_eq!(s.balance, -1.0);
        assert_eq!(s.queue_index, None);
        assert_eq!(s.repeat, RepeatMode::All);
        assert_eq!(s.eq.preamp_db, EQ_MAX_DB);
        assert_eq!(s.eq.bands_db[9], -EQ_MAX_DB);
    }

    #[test]
    fn garbage_is_an_error_not_a_panic() {
        assert!(SessionState::from_toml("volume = [").is_err());
    }

    #[test]
    fn default_state_round_trips() {
        let text = SessionState::default().to_toml().unwrap();
        assert_eq!(
            SessionState::from_toml(&text).unwrap(),
            SessionState::default()
        );
    }
}
