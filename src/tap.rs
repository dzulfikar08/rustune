//! Lock-free sample tap: the audio thread pushes post-EQ samples, the UI thread
//! reads the most recent window for visualization. Single writer, many readers.
//! The writer never blocks or allocates; readers may observe a torn window
//! during a concurrent write, which is harmless for visualization.

use std::sync::atomic::{AtomicU16, AtomicU32, AtomicUsize, Ordering};

/// Capacity in samples (interleaved). Power of two; ~0.37 s of 44.1 kHz stereo.
pub const TAP_CAPACITY: usize = 1 << 15;

pub struct Tap {
    buf: Box<[AtomicU32]>,
    /// Total samples ever written (wraps); next write slot = write & MASK.
    write: AtomicUsize,
    channels: AtomicU16,
    sample_rate: AtomicU32,
}

const MASK: usize = TAP_CAPACITY - 1;

impl Tap {
    pub fn new() -> Self {
        Self {
            buf: (0..TAP_CAPACITY).map(|_| AtomicU32::new(0)).collect(),
            write: AtomicUsize::new(0),
            channels: AtomicU16::new(2),
            sample_rate: AtomicU32::new(44_100),
        }
    }

    /// Called by the audio thread when the stream format changes.
    pub fn set_format(&self, channels: u16, sample_rate: u32) {
        self.channels.store(channels.max(1), Ordering::Relaxed);
        self.sample_rate
            .store(sample_rate.max(1), Ordering::Relaxed);
    }

    pub fn channels(&self) -> u16 {
        self.channels.load(Ordering::Relaxed)
    }

    pub fn sample_rate(&self) -> u32 {
        self.sample_rate.load(Ordering::Relaxed)
    }

    /// Audio thread: push one interleaved sample.
    #[inline]
    pub fn push(&self, sample: f32) {
        let w = self.write.load(Ordering::Relaxed);
        self.buf[w & MASK].store(sample.to_bits(), Ordering::Relaxed);
        self.write.store(w.wrapping_add(1), Ordering::Release);
    }

    /// Audio thread: forget buffered samples (e.g. on stop) so visuals decay to silence.
    pub fn clear(&self) {
        for s in self.buf.iter() {
            s.store(0, Ordering::Relaxed);
        }
    }

    /// UI thread: fill `out` with the most recent `out.len()` mono frames
    /// (channels averaged), oldest first. Returns the number of frames written
    /// (less than `out.len()` only before enough audio has been pushed).
    pub fn snapshot_mono(&self, out: &mut [f32]) -> usize {
        let ch = self.channels() as usize;
        let w = self.write.load(Ordering::Acquire);
        let max_frames = (TAP_CAPACITY / ch).min(w / ch);
        let frames = out.len().min(max_frames);
        // Align the end to a frame boundary.
        let end = w - (w % ch);
        let start = end - frames * ch;
        let inv = 1.0 / ch as f32;
        for (i, o) in out[..frames].iter_mut().enumerate() {
            let base = start + i * ch;
            let mut acc = 0.0;
            for c in 0..ch {
                acc += f32::from_bits(self.buf[(base + c) & MASK].load(Ordering::Relaxed));
            }
            *o = acc * inv;
        }
        frames
    }
}

impl Default for Tap {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn snapshot_returns_latest_frames_mixed_to_mono() {
        let tap = Tap::new();
        tap.set_format(2, 48_000);
        for i in 0..10 {
            tap.push(i as f32); // L
            tap.push(-(i as f32)); // R
        }
        let mut out = [9.0f32; 4];
        assert_eq!(tap.snapshot_mono(&mut out), 4);
        assert_eq!(out, [0.0; 4]); // L+R cancel

        let tap = Tap::new();
        tap.set_format(1, 48_000);
        for i in 0..(TAP_CAPACITY + 5) {
            tap.push(i as f32);
        }
        let mut out = [0.0f32; 3];
        tap.snapshot_mono(&mut out);
        let last = (TAP_CAPACITY + 4) as f32;
        assert_eq!(out, [last - 2.0, last - 1.0, last]);
    }

    #[test]
    fn snapshot_before_enough_audio_is_partial() {
        let tap = Tap::new();
        tap.set_format(2, 44_100);
        tap.push(1.0);
        tap.push(1.0);
        tap.push(0.5); // incomplete frame is ignored
        let mut out = [0.0f32; 8];
        assert_eq!(tap.snapshot_mono(&mut out), 1);
        assert_eq!(out[0], 1.0);
    }
}
