//! Plain-data snapshot of player state handed to renderers (TUI themes and the
//! Winamp skin compositor). Renderers never read `App` directly.

use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::media::MediaItem;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum PlayStatus {
    #[default]
    Stopped,
    Playing,
    Paused,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum RepeatMode {
    #[default]
    Off,
    All,
    One,
}

impl RepeatMode {
    pub fn cycle(self) -> Self {
        match self {
            Self::Off => Self::All,
            Self::All => Self::One,
            Self::One => Self::Off,
        }
    }
}

pub const EQ_BANDS: usize = 10;
/// Winamp 2.x band centre frequencies (Hz).
pub const EQ_FREQS: [f32; EQ_BANDS] = [
    60.0, 170.0, 310.0, 600.0, 1_000.0, 3_000.0, 6_000.0, 12_000.0, 14_000.0, 16_000.0,
];
/// Gain range for preamp and bands, in dB (Winamp: ±12 dB, displayed as ±20).
pub const EQ_MAX_DB: f32 = 12.0;

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct EqSettings {
    pub enabled: bool,
    /// Winamp "AUTO" toggle; kept for skin fidelity and persisted.
    pub auto: bool,
    pub preamp_db: f32,
    pub bands_db: [f32; EQ_BANDS],
}

impl Default for EqSettings {
    fn default() -> Self {
        Self {
            enabled: false,
            auto: false,
            preamp_db: 0.0,
            bands_db: [0.0; EQ_BANDS],
        }
    }
}

/// Technical info about the playing stream, when known.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct StreamFormat {
    pub kbps: Option<u32>,
    pub sample_rate: Option<u32>,
    pub channels: Option<u16>,
}

pub struct PlayerView<'a> {
    /// Marquee text, e.g. "Artist - Title".
    pub title: &'a str,
    pub status: PlayStatus,
    pub elapsed: Duration,
    pub duration: Option<Duration>,
    /// 0.0..=1.0
    pub volume: f32,
    /// -1.0 (left) ..= 1.0 (right)
    pub balance: f32,
    pub shuffle: bool,
    pub repeat: RepeatMode,
    pub eq: EqSettings,
    pub format: StreamFormat,
    /// Spectrum magnitudes 0.0..=1.0, low → high frequency, any length.
    pub spectrum: &'a [f32],
    /// Most recent waveform, -1.0..=1.0, oldest first, any length.
    pub scope: &'a [f32],
    /// The play queue in display order.
    pub playlist: &'a [MediaItem],
    /// Index into `playlist` of the playing track.
    pub current: Option<usize>,
    /// Index into `playlist` of the cursor.
    pub selected: usize,
}
