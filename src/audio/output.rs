//! Audio output: the device-independent [`Renderer`] (runs on the audio
//! callback) and the cpal device stream that drives it.

use std::ptr::null_mut;
use std::sync::atomic::Ordering;
use std::sync::Arc;

use anyhow::{anyhow, bail, Context};
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{FromSample, SampleFormat, SizedSample, StreamConfig, StreamError};

use super::dsp::Chain;
use super::engine::Shared;
use super::track::{ENDED, EOF, FAILED, PRIMED, STARTED};

/// Output stream format; decoder threads convert every source to it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OutFormat {
    pub rate: u32,
    pub channels: usize,
}

/// Pulls frames from the current track's ring (switching to the preloaded track
/// at the exact sample where the current one ends) and runs the DSP chain.
/// Never blocks, locks or allocates.
pub struct Renderer {
    shared: Arc<Shared>,
    chain: Chain,
    channels: usize,
}

impl Renderer {
    pub fn new(shared: Arc<Shared>, fmt: OutFormat) -> Self {
        Self {
            shared,
            chain: Chain::new(fmt.rate, fmt.channels),
            channels: fmt.channels.max(1),
        }
    }

    /// Fill `out` (whole frames). Returns the number of samples taken from tracks.
    pub fn render(&mut self, out: &mut [f32]) -> usize {
        self.shared.busy.store(true, Ordering::SeqCst);
        let n = self.fill(out);
        self.shared.busy.store(false, Ordering::SeqCst);
        self.shared.epoch.fetch_add(1, Ordering::SeqCst);
        n
    }

    fn fill(&mut self, out: &mut [f32]) -> usize {
        let sh = &*self.shared;
        self.chain.sync(&sh.params);
        if sh.paused.load(Ordering::Relaxed) {
            out.fill(0.0);
            return 0;
        }
        let ch = self.channels;
        let mode = sh.params.replaygain();
        let mut done = 0;
        let mut cur = sh.cur.load(Ordering::SeqCst);
        while done < out.len() && !cur.is_null() {
            // SAFETY: the controller keeps every track published in `cur`/`next`
            // alive until a callback that could have loaded it has finished
            // (`busy` cleared or `epoch` advanced); see `engine::Controller::reap`.
            let track = unsafe { &*cur };
            let flags = track.flags();
            if flags & (PRIMED | EOF | FAILED) == 0 {
                if track.ring.len() < track.ring.threshold() {
                    break; // still buffering
                }
                track.set(PRIMED);
            }
            let (n, flush) = track.ring.pop(&mut out[done..]);
            if let Some(f) = flush {
                track.played.store(f.frame, Ordering::Relaxed);
                sh.seek_done.store(f.tag, Ordering::Release);
            }
            if n > 0 {
                if flags & STARTED == 0 {
                    track.set(STARTED);
                    sh.wake_controller();
                }
                self.chain
                    .process(&mut out[done..done + n], track.rg_gain(mode), &sh.tap);
                let played = track.played.load(Ordering::Relaxed) + (n / ch) as u64;
                track.played.store(played, Ordering::Relaxed);
                done += n;
            }
            if done == out.len() {
                break;
            }
            if flags & (EOF | FAILED) != 0 {
                // Drained (EOF was observed before the pop): hand over to the
                // preloaded track at this exact sample. Failures do not advance.
                track.set(ENDED);
                let next = if flags & FAILED != 0 {
                    null_mut()
                } else {
                    sh.next.load(Ordering::SeqCst)
                };
                match sh
                    .cur
                    .compare_exchange(cur, next, Ordering::SeqCst, Ordering::SeqCst)
                {
                    Ok(_) => {
                        if !next.is_null() {
                            let _ = sh.next.compare_exchange(
                                next,
                                null_mut(),
                                Ordering::SeqCst,
                                Ordering::SeqCst,
                            );
                        }
                        cur = next;
                    }
                    Err(actual) => cur = actual,
                }
                sh.wake_controller();
            } else {
                // Underrun: wait until the decoder has refilled a quarter ring.
                track.clear(PRIMED);
                break;
            }
        }
        if !cur.is_null() {
            // SAFETY: as above.
            let played = unsafe { &*cur }.played.load(Ordering::Relaxed);
            sh.pos_frames.store(played, Ordering::Relaxed);
        }
        out[done..].fill(0.0);
        self.chain.output(out, &sh.params);
        done
    }
}

/// A running output stream.
pub enum Output {
    Device(cpal::Stream),
    #[cfg(test)]
    Headless(headless::Headless),
}

impl Output {
    /// Start or stop pulling audio. A stopped stream costs no CPU.
    pub fn set_running(&mut self, on: bool) {
        match self {
            // Errors are harmless: the renderer outputs silence when idle.
            Self::Device(s) => {
                let _ = if on {
                    s.play().map_err(|_| ())
                } else {
                    s.pause().map_err(|_| ())
                };
            }
            #[cfg(test)]
            Self::Headless(h) => h.set_running(on),
        }
    }
}

/// Open the default output device. Prefers stereo, and `prefer`'s rate (when
/// re-opening after a device change) if the device supports it.
pub fn open_device(
    shared: &Arc<Shared>,
    prefer: Option<OutFormat>,
) -> anyhow::Result<(Output, OutFormat)> {
    let host = cpal::default_host();
    let device = host
        .default_output_device()
        .ok_or_else(|| anyhow!("no audio output device"))?;
    let default = device
        .default_output_config()
        .context("audio output device has no usable configuration")?;
    let format = default.sample_format();
    let mut config: StreamConfig = default.config();
    let want_ch = prefer.map_or(2, |p| p.channels as u16);
    let want_rate = prefer.map_or(config.sample_rate, |p| p.rate);
    if let Ok(ranges) = device.supported_output_configs() {
        let ranges: Vec<_> = ranges
            .filter(|r| r.sample_format() == format && r.channels() == want_ch)
            .collect();
        let fits = |rate: u32| {
            ranges
                .iter()
                .any(|r| r.min_sample_rate() <= rate && rate <= r.max_sample_rate())
        };
        if fits(want_rate) {
            config.channels = want_ch;
            config.sample_rate = want_rate;
        } else if fits(config.sample_rate) {
            config.channels = want_ch;
        }
    }
    let fmt = OutFormat {
        rate: config.sample_rate,
        channels: usize::from(config.channels),
    };
    let renderer = Renderer::new(shared.clone(), fmt);
    let err_shared = shared.clone();
    let on_error = move |e: StreamError| {
        if matches!(
            e,
            StreamError::DeviceNotAvailable | StreamError::StreamInvalidated
        ) {
            err_shared.device_lost.store(true, Ordering::SeqCst);
            err_shared.wake_controller();
        }
    };
    let stream = match format {
        SampleFormat::F32 => {
            let mut r = renderer;
            device.build_output_stream::<f32, _, _>(
                &config,
                move |data: &mut [f32], _: &cpal::OutputCallbackInfo| {
                    r.render(data);
                },
                on_error,
                None,
            )
        }
        SampleFormat::F64 => build::<f64>(&device, &config, renderer, on_error),
        SampleFormat::I16 => build::<i16>(&device, &config, renderer, on_error),
        SampleFormat::I32 => build::<i32>(&device, &config, renderer, on_error),
        SampleFormat::U16 => build::<u16>(&device, &config, renderer, on_error),
        SampleFormat::I8 => build::<i8>(&device, &config, renderer, on_error),
        SampleFormat::U8 => build::<u8>(&device, &config, renderer, on_error),
        other => bail!("unsupported output sample format {other:?}"),
    }
    .context("cannot open audio output stream")?;
    // Some backends start streams on creation; stay silent (and idle) until needed.
    let _ = stream.pause();
    shared.set_output_format(fmt);
    Ok((Output::Device(stream), fmt))
}

fn build<T>(
    device: &cpal::Device,
    config: &StreamConfig,
    mut renderer: Renderer,
    on_error: impl FnMut(StreamError) + Send + 'static,
) -> Result<cpal::Stream, cpal::BuildStreamError>
where
    T: SizedSample + FromSample<f32> + Send + 'static,
{
    // Conversion scratch, allocated once here, never in the callback.
    let mut scratch = vec![0.0f32; 4096 * usize::from(config.channels.max(1))];
    device.build_output_stream::<T, _, _>(
        config,
        move |data: &mut [T], _: &cpal::OutputCallbackInfo| {
            for chunk in data.chunks_mut(scratch.len()) {
                let s = &mut scratch[..chunk.len()];
                renderer.render(s);
                for (d, &x) in chunk.iter_mut().zip(s.iter()) {
                    *d = <T as FromSample<f32>>::from_sample_(x);
                }
            }
        },
        on_error,
        None,
    )
}

#[cfg(test)]
pub mod headless {
    //! Device-free output for tests: renders on a thread, optionally paced.

    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::Arc;
    use std::thread::{self, JoinHandle};
    use std::time::Duration;

    use super::{OutFormat, Renderer};
    use crate::audio::engine::Shared;

    pub struct Headless {
        running: Arc<AtomicBool>,
        quit: Arc<AtomicBool>,
        thread: Option<JoinHandle<()>>,
    }

    impl Headless {
        /// `realtime`: pace like a device; otherwise render as fast as possible.
        pub fn start(shared: &Arc<Shared>, fmt: OutFormat, realtime: bool) -> Self {
            let running = Arc::new(AtomicBool::new(false));
            let quit = Arc::new(AtomicBool::new(false));
            let mut r = Renderer::new(shared.clone(), fmt);
            let (run, q) = (running.clone(), quit.clone());
            shared.set_output_format(fmt);
            let thread = thread::spawn(move || {
                let frames = 512;
                let mut buf = vec![0.0f32; frames * fmt.channels];
                let period = Duration::from_secs_f64(frames as f64 / f64::from(fmt.rate));
                while !q.load(Ordering::SeqCst) {
                    if !run.load(Ordering::SeqCst) {
                        thread::park();
                        continue;
                    }
                    let n = r.render(&mut buf);
                    if realtime {
                        thread::sleep(period);
                    } else if n == 0 {
                        thread::sleep(Duration::from_millis(1));
                    }
                }
            });
            Self {
                running,
                quit,
                thread: Some(thread),
            }
        }

        pub fn set_running(&mut self, on: bool) {
            self.running.store(on, Ordering::SeqCst);
            if let Some(t) = &self.thread {
                t.thread().unpark();
            }
        }
    }

    impl Drop for Headless {
        fn drop(&mut self) {
            self.quit.store(true, Ordering::SeqCst);
            if let Some(t) = self.thread.take() {
                t.thread().unpark();
                let _ = t.join();
            }
        }
    }
}
