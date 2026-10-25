//! Per-track state shared by the controller, the track's decoder thread and the
//! audio callback.

use std::process::Child;
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};
use std::sync::{Mutex, MutexGuard, PoisonError};

use super::replaygain::ReplayGainTags;
use super::ring::{Ring, RING_CAPACITY};
use super::{ReplayGainMode, Source, TrackInfo};

/// Source opened; decoding.
pub const OPENED: u32 = 1;
/// Enough audio buffered to start (or resume after an underrun).
pub const PRIMED: u32 = 1 << 1;
/// Producer finished: everything left is in the ring.
pub const EOF: u32 = 1 << 2;
/// Open/decode failed (implies EOF); `Error` already emitted.
pub const FAILED: u32 = 1 << 3;
/// Callback consumed the first frames.
pub const STARTED: u32 = 1 << 4;
/// Callback drained the ring after EOF and moved on.
pub const ENDED: u32 = 1 << 5;
/// Track retired; decoder must exit, events suppressed.
pub const STOP: u32 = 1 << 6;

pub(crate) fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

pub struct Track {
    pub token: u64,
    pub src: Source,
    pub ring: Ring,
    flags: AtomicU32,
    /// Frames (output rate) consumed by the callback, i.e. the play position.
    pub played: AtomicU64,
    seek_gen: AtomicU64,
    seek_ms: AtomicU64,
    /// Initial position to start decoding from (device-recovery restarts).
    pub start_ms: u64,
    info: Mutex<TrackInfo>,
    rg_track: AtomicU32,
    rg_album: AtomicU32,
    /// Running ffmpeg process, killed on stop to unblock the decoder.
    pub child: Mutex<Option<Child>>,
}

impl Track {
    pub fn new(src: Source, token: u64, channels: usize, start_ms: u64) -> Self {
        Self {
            token,
            src,
            ring: Ring::new(RING_CAPACITY, channels),
            flags: AtomicU32::new(0),
            played: AtomicU64::new(0),
            seek_gen: AtomicU64::new(0),
            seek_ms: AtomicU64::new(0),
            start_ms,
            info: Mutex::new(TrackInfo::default()),
            rg_track: AtomicU32::new(1f32.to_bits()),
            rg_album: AtomicU32::new(1f32.to_bits()),
            child: Mutex::new(None),
        }
    }

    #[inline]
    pub fn flags(&self) -> u32 {
        self.flags.load(Ordering::Acquire)
    }

    #[inline]
    pub fn has(&self, f: u32) -> bool {
        self.flags() & f != 0
    }

    #[inline]
    pub fn set(&self, f: u32) {
        self.flags.fetch_or(f, Ordering::AcqRel);
    }

    #[inline]
    pub fn clear(&self, f: u32) {
        self.flags.fetch_and(!f, Ordering::AcqRel);
    }

    pub fn request_seek(&self, ms: u64, generation: u64) {
        self.seek_ms.store(ms, Ordering::Relaxed);
        self.seek_gen.store(generation, Ordering::Release);
        self.ring.wake_producer();
    }

    /// (generation, target ms) of the latest seek request.
    pub fn seek_request(&self) -> (u64, u64) {
        let g = self.seek_gen.load(Ordering::Acquire);
        (g, self.seek_ms.load(Ordering::Relaxed))
    }

    pub fn seek_generation(&self) -> u64 {
        self.seek_gen.load(Ordering::Acquire)
    }

    /// Retire: the decoder exits at its next check, ffmpeg is killed so a
    /// blocked read returns.
    pub fn stop(&self) {
        self.set(STOP);
        if let Some(child) = lock(&self.child).as_mut() {
            let _ = child.kill();
        }
        self.ring.wake_producer();
    }

    pub fn set_replaygain(&self, rg: &ReplayGainTags) {
        self.rg_track
            .store(rg.gain(ReplayGainMode::Track).to_bits(), Ordering::Relaxed);
        self.rg_album
            .store(rg.gain(ReplayGainMode::Album).to_bits(), Ordering::Relaxed);
    }

    #[inline]
    pub fn rg_gain(&self, mode: ReplayGainMode) -> f32 {
        match mode {
            ReplayGainMode::Off => 1.0,
            ReplayGainMode::Track => f32::from_bits(self.rg_track.load(Ordering::Relaxed)),
            ReplayGainMode::Album => f32::from_bits(self.rg_album.load(Ordering::Relaxed)),
        }
    }

    pub fn info(&self) -> TrackInfo {
        lock(&self.info).clone()
    }

    pub fn update_info(&self, f: impl FnOnce(&mut TrackInfo)) {
        f(&mut lock(&self.info));
    }
}

impl Drop for Track {
    fn drop(&mut self) {
        let child = self
            .child
            .get_mut()
            .unwrap_or_else(PoisonError::into_inner)
            .take();
        if let Some(mut child) = child {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}
