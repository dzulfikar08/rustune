//! Single-producer/single-consumer ring of interleaved f32 frames between a
//! decoder thread (producer) and the audio callback (consumer).
//!
//! The consumer side is wait-free and allocation-free. The producer parks when
//! the ring is full and is unparked by the consumer once a quarter of the ring
//! has drained, so a paused or full pipeline costs no CPU.
//!
//! Seeking is a producer-side `flush`: it publishes the current write index as
//! the new read index together with the timeline position of the next frame;
//! the consumer applies it on its next `pop` and drops everything before it.

use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, AtomicUsize, Ordering};
use std::sync::OnceLock;
use std::thread::Thread;

/// Default capacity in samples: ~1.4 s of 48 kHz stereo.
pub const RING_CAPACITY: usize = 1 << 17;

/// A flush observed by the consumer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Flush {
    /// Timeline position (in frames) of the first frame after the flush.
    pub frame: u64,
    /// Opaque tag supplied by the producer (the engine's seek generation).
    pub tag: u64,
}

pub struct Ring {
    buf: Box<[AtomicU32]>,
    mask: usize,
    channels: usize,
    /// Total samples ever written (wrapping).
    write: AtomicUsize,
    /// Total samples ever read (wrapping).
    read: AtomicUsize,
    flush_count: AtomicU64,
    flush_at: AtomicUsize,
    flush_frame: AtomicU64,
    flush_tag: AtomicU64,
    /// Consumer-private: last applied `flush_count`.
    seen_flush: AtomicU64,
    /// Producer is (about to be) parked waiting for space.
    waiting: AtomicBool,
    producer: OnceLock<Thread>,
}

impl Ring {
    pub fn new(capacity: usize, channels: usize) -> Self {
        assert!(
            capacity.is_power_of_two(),
            "ring capacity must be a power of two"
        );
        let channels = channels.max(1);
        assert!(capacity >= channels * 4, "ring too small for channel count");
        Self {
            buf: (0..capacity).map(|_| AtomicU32::new(0)).collect(),
            mask: capacity - 1,
            channels,
            write: AtomicUsize::new(0),
            read: AtomicUsize::new(0),
            flush_count: AtomicU64::new(0),
            flush_at: AtomicUsize::new(0),
            flush_frame: AtomicU64::new(0),
            flush_tag: AtomicU64::new(0),
            seen_flush: AtomicU64::new(0),
            waiting: AtomicBool::new(false),
            producer: OnceLock::new(),
        }
    }

    pub fn capacity(&self) -> usize {
        self.mask + 1
    }

    /// Samples above which the ring counts as "primed" and below which a parked
    /// producer is woken. Frame aligned.
    pub fn threshold(&self) -> usize {
        let t = self.capacity() / 4;
        t - t % self.channels
    }

    /// Buffered samples (approximate when called concurrently).
    pub fn len(&self) -> usize {
        let w = self.write.load(Ordering::Acquire);
        let r = self.read.load(Ordering::Acquire);
        w.wrapping_sub(r).min(self.capacity())
    }

    fn free(&self) -> usize {
        let w = self.write.load(Ordering::Relaxed);
        let r = self.read.load(Ordering::SeqCst);
        self.capacity() - w.wrapping_sub(r).min(self.capacity())
    }

    /// Producer: must be called from the producing thread before it may park.
    pub fn register_producer(&self) {
        let _ = self.producer.set(std::thread::current());
    }

    /// Wake the producer (seek/stop requests, or space became available).
    pub fn wake_producer(&self) {
        if let Some(t) = self.producer.get() {
            t.unpark();
        }
    }

    /// Producer: copy as many whole frames of `data` as fit. Returns samples written.
    pub fn push(&self, data: &[f32]) -> usize {
        let w = self.write.load(Ordering::Relaxed);
        let mut n = data.len().min(self.free());
        n -= n % self.channels;
        for (i, &s) in data[..n].iter().enumerate() {
            self.buf[w.wrapping_add(i) & self.mask].store(s.to_bits(), Ordering::Relaxed);
        }
        self.write.store(w.wrapping_add(n), Ordering::Release);
        n
    }

    /// Producer: push all of `data` (whole frames), parking while the ring is
    /// full. Returns `false` as soon as `abort()` returns true.
    pub fn push_all(&self, mut data: &[f32], abort: &dyn Fn() -> bool) -> bool {
        debug_assert_eq!(data.len() % self.channels, 0);
        loop {
            let n = self.push(data);
            data = &data[n..];
            if data.len() < self.channels {
                return true;
            }
            if abort() {
                return false;
            }
            let need = data.len().min(self.threshold());
            self.waiting.store(true, Ordering::SeqCst);
            if self.free() < need {
                std::thread::park();
            }
            self.waiting.store(false, Ordering::SeqCst);
            if abort() {
                return false;
            }
        }
    }

    /// Producer: discard everything buffered so far. The next frame pushed is at
    /// timeline position `frame`; `tag` is reported back to the consumer.
    pub fn flush(&self, frame: u64, tag: u64) {
        self.flush_frame.store(frame, Ordering::Relaxed);
        self.flush_tag.store(tag, Ordering::Relaxed);
        self.flush_at
            .store(self.write.load(Ordering::Relaxed), Ordering::Relaxed);
        self.flush_count.fetch_add(1, Ordering::Release);
    }

    /// Consumer: pop up to `out.len()` samples (whole frames only). Returns the
    /// number of samples written to `out` and a flush applied by this call.
    pub fn pop(&self, out: &mut [f32]) -> (usize, Option<Flush>) {
        let mut flushed = None;
        let fc = self.flush_count.load(Ordering::Acquire);
        if fc != self.seen_flush.load(Ordering::Relaxed) {
            self.seen_flush.store(fc, Ordering::Relaxed);
            self.read
                .store(self.flush_at.load(Ordering::Relaxed), Ordering::SeqCst);
            flushed = Some(Flush {
                frame: self.flush_frame.load(Ordering::Relaxed),
                tag: self.flush_tag.load(Ordering::Relaxed),
            });
        }
        let r = self.read.load(Ordering::Relaxed);
        let w = self.write.load(Ordering::Acquire);
        let avail = w.wrapping_sub(r);
        let mut n = out.len().min(avail);
        n -= n % self.channels;
        for (i, o) in out[..n].iter_mut().enumerate() {
            *o = f32::from_bits(self.buf[r.wrapping_add(i) & self.mask].load(Ordering::Relaxed));
        }
        self.read.store(r.wrapping_add(n), Ordering::SeqCst);
        if self.waiting.load(Ordering::SeqCst)
            && self.capacity() - (avail - n) >= self.threshold()
            && self.waiting.swap(false, Ordering::SeqCst)
        {
            self.wake_producer();
        }
        (n, flushed)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    #[test]
    fn push_pop_whole_frames_and_wraparound() {
        let ring = Ring::new(16, 2);
        let data: Vec<f32> = (0..12).map(|i| i as f32).collect();
        assert_eq!(ring.push(&data), 12);
        // Only 4 free, and 5 offered: 2 whole frames fit.
        assert_eq!(ring.push(&[1.0; 5]), 4);
        let mut out = [0.0f32; 7];
        let (n, f) = ring.pop(&mut out);
        assert_eq!((n, f), (6, None)); // odd request trimmed to whole frames
        assert_eq!(&out[..6], &data[..6]);
        // Wraps around the end of the buffer.
        let more: Vec<f32> = (100..106).map(|i| i as f32).collect();
        assert_eq!(ring.push(&more), 6);
        let mut all = [0.0f32; 16];
        let (n, _) = ring.pop(&mut all);
        assert_eq!(n, 16);
        assert_eq!(&all[..6], &data[6..]);
        assert_eq!(&all[6..10], &[1.0; 4]);
        assert_eq!(&all[10..], &more[..]);
        assert_eq!(ring.len(), 0);
    }

    #[test]
    fn flush_drops_buffered_samples_and_reports_position() {
        let ring = Ring::new(16, 1);
        ring.push(&[1.0, 2.0, 3.0]);
        ring.flush(4_800, 7);
        ring.push(&[9.0]);
        let mut out = [0.0f32; 4];
        let (n, f) = ring.pop(&mut out);
        assert_eq!(n, 1);
        assert_eq!(out[0], 9.0);
        assert_eq!(
            f,
            Some(Flush {
                frame: 4_800,
                tag: 7
            })
        );
        assert_eq!(ring.pop(&mut out), (0, None));
    }

    #[test]
    fn producer_parks_when_full_and_consumer_wakes_it() {
        let ring = Arc::new(Ring::new(64, 2));
        let total = 64 * 50;
        let producer = {
            let ring = ring.clone();
            std::thread::spawn(move || {
                ring.register_producer();
                let data: Vec<f32> = (0..total).map(|i| i as f32).collect();
                for chunk in data.chunks(10) {
                    assert!(ring.push_all(chunk, &|| false));
                }
            })
        };
        let mut got = Vec::with_capacity(total);
        let mut buf = [0.0f32; 6];
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        while got.len() < total {
            assert!(std::time::Instant::now() < deadline, "producer stalled");
            let (n, _) = ring.pop(&mut buf);
            got.extend_from_slice(&buf[..n]);
            if n == 0 {
                std::thread::yield_now();
            }
        }
        producer.join().unwrap();
        assert!(got.iter().enumerate().all(|(i, &s)| s == i as f32));
    }

    #[test]
    fn push_all_aborts() {
        let ring = Ring::new(8, 1);
        ring.register_producer();
        assert!(!ring.push_all(&[0.0; 20], &|| true));
        assert_eq!(ring.len(), 8);
    }
}
