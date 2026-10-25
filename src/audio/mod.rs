//! Self-contained playback engine (replaces mpv).
//!
//! ```text
//! UI thread ──commands──▶ controller thread ──spawns──▶ decoder thread per track
//!                                │                       (file/HTTP/ffmpeg → decode →
//!                                │                        remix → resample → ring)
//!                                ▼
//!                  audio callback (cpal): ring → ReplayGain → preamp+EQ → Tap
//!                                         → balance → volume → device
//! ```
//!
//! * Decoding, network IO and ffmpeg run on per-track decoder threads; the
//!   audio callback only pops from a lock-free ring and runs the DSP chain.
//! * Gapless: `preload` decodes the next track into its own ring; the callback
//!   switches rings at the exact sample where the current track ends.
//! * Idle costs nothing: the output stream is paused when nothing plays and
//!   every helper thread parks instead of polling.

mod decode;
mod dsp;
mod engine;
mod ffmpeg;
mod http;
mod icy;
mod output;
mod replaygain;
mod resample;
mod ring;
mod track;

use std::path::PathBuf;
use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::view::StreamFormat;

pub use engine::Engine;

/// What to play.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Source {
    /// Local file (decoded natively; ffmpeg fallback for other formats).
    File(PathBuf),
    /// `http(s)://` URL: seekable via Range when supported, live streams with
    /// ICY titles, `.pls`/`.m3u` radio playlists, HLS via ffmpeg.
    Url(String),
}

/// Facts about a track, reported with [`EngineEvent::Started`].
#[derive(Debug, Clone, PartialEq, Default)]
pub struct TrackInfo {
    pub duration: Option<Duration>,
    /// Source format (before conversion to the output device format).
    pub format: StreamFormat,
    pub seekable: bool,
    /// Track gain from tags (album gain when only that is present).
    pub replaygain_db: Option<f32>,
}

/// Engine notifications. Every event carries the token passed to
/// [`Engine::play`]/[`Engine::preload`], so events from replaced tracks can be
/// told apart from current ones.
#[derive(Debug, Clone, PartialEq)]
pub enum EngineEvent {
    /// The track produced its first audio (for a preload: it took over gaplessly).
    Started { token: u64, info: TrackInfo },
    /// The track played to its end. Not sent for tracks replaced by `play`,
    /// stopped by `stop`, or that failed (those get `Error`).
    Ended { token: u64 },
    /// Opening or decoding failed; terminal for that token.
    Error { token: u64, message: String },
    /// New live-stream title (ICY `StreamTitle`, chained Ogg tags, ffmpeg metadata).
    StreamTitle { token: u64, title: String },
}

/// ReplayGain normalisation. Missing tags fall back to the other gain; tracks
/// without tags play unchanged. Peaks (when tagged) prevent clipping.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ReplayGainMode {
    #[default]
    Off,
    Track,
    Album,
}
