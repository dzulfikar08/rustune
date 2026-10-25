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
use std::sync::atomic::Ordering;
use std::sync::mpsc::Sender;
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::Duration;

use anyhow::bail;
use serde::{Deserialize, Serialize};

use crate::tap::Tap;
use crate::view::{EqSettings, PlayStatus, StreamFormat};
use engine::{Backend, Cmd, Shared};

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

/// Handle to the playback engine. All methods are non-blocking: IO, decoding
/// and process management happen on engine threads.
pub struct Engine {
    shared: Arc<Shared>,
    tx: Sender<Cmd>,
    thread: Option<JoinHandle<()>>,
}

impl Engine {
    /// Open the default output device and start the engine threads.
    /// `on_event` is called from engine threads (never the audio callback).
    pub fn new(
        tap: Arc<Tap>,
        on_event: Box<dyn Fn(EngineEvent) + Send + Sync>,
    ) -> anyhow::Result<Self> {
        Self::start(tap, on_event, Backend::Device)
    }

    #[cfg(test)]
    fn headless(
        tap: Arc<Tap>,
        on_event: Box<dyn Fn(EngineEvent) + Send + Sync>,
        fmt: output::OutFormat,
        realtime: bool,
    ) -> anyhow::Result<Self> {
        Self::start(tap, on_event, Backend::Headless { fmt, realtime })
    }

    fn start(
        tap: Arc<Tap>,
        on_event: Box<dyn Fn(EngineEvent) + Send + Sync>,
        backend: Backend,
    ) -> anyhow::Result<Self> {
        let (shared, tx, thread) = engine::spawn(tap, on_event, backend)?;
        Ok(Self {
            shared,
            tx,
            thread: Some(thread),
        })
    }

    fn send(&self, cmd: Cmd) {
        let _ = self.tx.send(cmd);
        self.shared.wake_controller();
    }

    /// Play `src` now, replacing the current track and dropping any preload.
    /// Events for it carry `token`.
    pub fn play(&self, src: Source, token: u64) {
        self.shared.paused.store(false, Ordering::SeqCst);
        self.shared.set_status(PlayStatus::Playing);
        self.send(Cmd::Play(src, token));
    }

    /// Queue `src` to follow the current track gaplessly (replacing an earlier
    /// preload). Produces `Ended{current}` then `Started{token}` at the switch.
    /// With nothing playing, behaves like [`Engine::play`].
    pub fn preload(&self, src: Source, token: u64) {
        self.send(Cmd::Preload(src, token));
    }

    pub fn pause(&self) {
        if self.shared.status() == PlayStatus::Playing {
            self.shared.paused.store(true, Ordering::SeqCst);
            self.shared.set_status(PlayStatus::Paused);
            self.send(Cmd::Sync);
        }
    }

    pub fn resume(&self) {
        if self.shared.status() == PlayStatus::Paused {
            self.shared.paused.store(false, Ordering::SeqCst);
            self.shared.set_status(PlayStatus::Playing);
            self.send(Cmd::Sync);
        }
    }

    pub fn stop(&self) {
        self.shared.paused.store(false, Ordering::SeqCst);
        self.shared.set_status(PlayStatus::Stopped);
        self.send(Cmd::Stop);
    }

    /// Seek the current track. Errors when nothing plays or the stream is live.
    /// The position reads as `pos` immediately; audio follows once decoded.
    pub fn seek(&self, pos: Duration) -> anyhow::Result<()> {
        if self.shared.status() == PlayStatus::Stopped {
            bail!("nothing is playing");
        }
        if !self.shared.cur_seekable.load(Ordering::Relaxed) {
            bail!("this stream is not seekable");
        }
        let ms = u64::try_from(pos.as_millis()).unwrap_or(u64::MAX);
        self.shared.seek_target_ms.store(ms, Ordering::Relaxed);
        let generation = self.shared.seek_req.fetch_add(1, Ordering::AcqRel) + 1;
        self.send(Cmd::Seek { ms, generation });
        Ok(())
    }

    /// Position of the audio being sent to the device.
    pub fn position(&self) -> Duration {
        let sh = &self.shared;
        if sh.status() == PlayStatus::Stopped {
            return Duration::ZERO;
        }
        if sh.seek_req.load(Ordering::Acquire) != sh.seek_done.load(Ordering::Acquire) {
            return Duration::from_millis(sh.seek_target_ms.load(Ordering::Relaxed));
        }
        let rate = u64::from(sh.out_rate().max(1));
        let frames = sh.pos_frames.load(Ordering::Relaxed);
        Duration::from_secs(frames / rate)
            + Duration::from_nanos((frames % rate) * 1_000_000_000 / rate)
    }

    pub fn status(&self) -> PlayStatus {
        self.shared.status()
    }

    /// 0.0..=1.0 slider position; mapped through a perceptual (cubic) curve.
    pub fn set_volume(&self, v: f32) {
        self.shared.params.set_volume(v);
    }

    /// -1.0 (left) ..= 1.0 (right).
    pub fn set_balance(&self, b: f32) {
        self.shared.params.set_balance(b);
    }

    pub fn set_eq(&self, eq: EqSettings) {
        self.shared.params.set_eq(&eq);
    }

    pub fn set_replaygain(&self, m: ReplayGainMode) {
        self.shared.params.set_replaygain(m);
    }
}

impl Drop for Engine {
    fn drop(&mut self) {
        self.send(Cmd::Shutdown);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}
