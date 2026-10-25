//! Controller thread behind [`super::Engine`].
//!
//! The controller owns every track (`Arc<Track>`), publishes the current and
//! preloaded track to the audio callback through two `AtomicPtr`s, turns
//! callback flags into events, and frees retired tracks only once the callback
//! can no longer be reading them.

use std::ptr::null_mut;
use std::sync::atomic::{AtomicBool, AtomicPtr, AtomicU32, AtomicU64, AtomicU8, Ordering};
use std::sync::mpsc::{self, Receiver, Sender, TryRecvError};
use std::sync::{Arc, OnceLock};
use std::thread::{self, JoinHandle, Thread};
use std::time::{Duration, Instant};

use anyhow::anyhow;

use super::decode::{self, frames_at, Emit};
use super::dsp::DspParams;
use super::output::{self, OutFormat, Output};
use super::track::{Track, ENDED, FAILED, STARTED, STOP};
use super::{EngineEvent, Source};
use crate::tap::Tap;
use crate::view::PlayStatus;

/// After playback ends, keep the device running this long so its last
/// buffer drains before the stream is paused.
const IDLE_PAUSE: Duration = Duration::from_millis(300);
/// Retry interval for freeing retired tracks while the callback is running.
const REAP_RETRY: Duration = Duration::from_millis(50);

/// State shared by the engine handle, the controller and the audio callback.
pub struct Shared {
    /// Track the callback plays. Owned (kept alive) by the controller.
    pub cur: AtomicPtr<Track>,
    /// Preloaded track the callback switches to when `cur` ends.
    pub next: AtomicPtr<Track>,
    /// Callback in progress / completed callbacks; used to free tracks safely.
    pub busy: AtomicBool,
    pub epoch: AtomicU64,
    pub paused: AtomicBool,
    pub params: DspParams,
    pub tap: Arc<Tap>,
    controller: OnceLock<Thread>,
    /// Seek generations: requested by `Engine::seek`, applied by the callback.
    pub seek_req: AtomicU64,
    pub seek_done: AtomicU64,
    pub seek_target_ms: AtomicU64,
    /// Play position of the current track in output frames.
    pub pos_frames: AtomicU64,
    out_rate: AtomicU32,
    status: AtomicU8,
    pub cur_seekable: AtomicBool,
    pub device_lost: AtomicBool,
}

impl Shared {
    fn new(tap: Arc<Tap>) -> Self {
        Self {
            cur: AtomicPtr::new(null_mut()),
            next: AtomicPtr::new(null_mut()),
            busy: AtomicBool::new(false),
            epoch: AtomicU64::new(0),
            paused: AtomicBool::new(false),
            params: DspParams::default(),
            tap,
            controller: OnceLock::new(),
            seek_req: AtomicU64::new(0),
            seek_done: AtomicU64::new(0),
            seek_target_ms: AtomicU64::new(0),
            pos_frames: AtomicU64::new(0),
            out_rate: AtomicU32::new(48_000),
            status: AtomicU8::new(0),
            cur_seekable: AtomicBool::new(false),
            device_lost: AtomicBool::new(false),
        }
    }

    /// Wake the controller thread (lock-free; safe from the audio callback).
    pub fn wake_controller(&self) {
        if let Some(t) = self.controller.get() {
            t.unpark();
        }
    }

    pub fn set_output_format(&self, fmt: OutFormat) {
        self.out_rate.store(fmt.rate, Ordering::Relaxed);
        self.tap.set_format(fmt.channels as u16, fmt.rate);
    }

    pub fn status(&self) -> PlayStatus {
        match self.status.load(Ordering::Acquire) {
            1 => PlayStatus::Playing,
            2 => PlayStatus::Paused,
            _ => PlayStatus::Stopped,
        }
    }

    pub fn set_status(&self, s: PlayStatus) {
        let v = match s {
            PlayStatus::Stopped => 0,
            PlayStatus::Playing => 1,
            PlayStatus::Paused => 2,
        };
        self.status.store(v, Ordering::Release);
    }

    pub fn out_rate(&self) -> u32 {
        self.out_rate.load(Ordering::Relaxed)
    }
}

pub enum Cmd {
    Play(Source, u64),
    Preload(Source, u64),
    /// Pause/resume flags changed; re-evaluate the output stream.
    Sync,
    Stop,
    Seek {
        ms: u64,
        generation: u64,
    },
    Shutdown,
}

/// Where audio goes.
#[derive(Clone, Copy)]
pub enum Backend {
    Device,
    #[cfg(test)]
    Headless {
        fmt: OutFormat,
        realtime: bool,
    },
}

impl Backend {
    fn open(
        self,
        shared: &Arc<Shared>,
        prefer: Option<OutFormat>,
    ) -> anyhow::Result<(Output, OutFormat)> {
        match self {
            Self::Device => output::open_device(shared, prefer),
            #[cfg(test)]
            Self::Headless { fmt, realtime } => Ok((
                Output::Headless(output::headless::Headless::start(shared, fmt, realtime)),
                fmt,
            )),
        }
    }
}

/// Start the controller thread and open the output. Returns once the output is
/// open (or failed to open).
pub fn spawn(
    tap: Arc<Tap>,
    on_event: Box<dyn Fn(EngineEvent) + Send + Sync>,
    backend: Backend,
) -> anyhow::Result<(Arc<Shared>, Sender<Cmd>, JoinHandle<()>)> {
    let shared = Arc::new(Shared::new(tap));
    let emit: Emit = Arc::from(on_event);
    let (tx, rx) = mpsc::channel();
    let (ready_tx, ready_rx) = mpsc::sync_channel(1);
    let sh = shared.clone();
    let thread = thread::Builder::new()
        .name("rustune-audio".into())
        .spawn(move || {
            let _ = sh.controller.set(thread::current());
            match backend.open(&sh, None) {
                Ok((output, fmt)) => {
                    let _ = ready_tx.send(Ok(()));
                    Controller {
                        shared: sh,
                        emit,
                        backend,
                        output: Some(output),
                        fmt,
                        tracks: Vec::new(),
                        running: false,
                        idle_since: None,
                    }
                    .run(&rx);
                }
                Err(e) => {
                    let _ = ready_tx.send(Err(e));
                }
            }
        })?;
    match ready_rx.recv() {
        Ok(Ok(())) => Ok((shared, tx, thread)),
        Ok(Err(e)) => {
            let _ = thread.join();
            Err(e)
        }
        Err(_) => Err(anyhow!("audio engine thread exited during startup")),
    }
}

struct Owned {
    track: Arc<Track>,
    started_sent: bool,
    ended_sent: bool,
    /// Callback epoch when the track was unpublished.
    retired: Option<u64>,
}

impl Owned {
    fn ptr(&self) -> *mut Track {
        Arc::as_ptr(&self.track).cast_mut()
    }
}

struct Controller {
    shared: Arc<Shared>,
    emit: Emit,
    backend: Backend,
    output: Option<Output>,
    fmt: OutFormat,
    /// Creation order, so events of consecutive tracks come out in order.
    tracks: Vec<Owned>,
    running: bool,
    idle_since: Option<Instant>,
}

impl Controller {
    fn run(mut self, rx: &Receiver<Cmd>) {
        loop {
            loop {
                match rx.try_recv() {
                    Ok(Cmd::Shutdown) | Err(TryRecvError::Disconnected) => return self.shutdown(),
                    Ok(cmd) => self.handle(cmd),
                    Err(TryRecvError::Empty) => break,
                }
            }
            if self.shared.device_lost.swap(false, Ordering::SeqCst) {
                self.recover_device();
            }
            self.process_tracks();
            self.update_output();
            self.reap();
            match self.wait_hint() {
                Some(t) => thread::park_timeout(t),
                None => thread::park(),
            }
        }
    }

    fn handle(&mut self, cmd: Cmd) {
        let sh = self.shared.clone();
        match cmd {
            Cmd::Play(src, token) => self.play(src, token, 0),
            Cmd::Preload(src, token) => {
                if sh.cur.load(Ordering::SeqCst).is_null() {
                    self.play(src, token, 0);
                } else {
                    let t = self.new_track(src, token, 0);
                    let old = sh.next.swap(Arc::as_ptr(&t).cast_mut(), Ordering::SeqCst);
                    self.retire(old);
                }
            }
            Cmd::Sync => {}
            Cmd::Stop => {
                let a = sh.cur.swap(null_mut(), Ordering::SeqCst);
                let b = sh.next.swap(null_mut(), Ordering::SeqCst);
                self.retire(a);
                self.retire(b);
                sh.tap.clear();
            }
            Cmd::Seek { ms, generation } => {
                let cur = sh.cur.load(Ordering::SeqCst);
                match self
                    .tracks
                    .iter()
                    .find(|o| o.ptr() == cur && o.retired.is_none())
                {
                    Some(o) => o.track.request_seek(ms, generation),
                    None => sh.seek_done.store(generation, Ordering::Release),
                }
            }
            Cmd::Shutdown => {}
        }
    }

    fn new_track(&mut self, src: Source, token: u64, start_ms: u64) -> Arc<Track> {
        let t = Arc::new(Track::new(src, token, self.fmt.channels, start_ms));
        decode::spawn(t.clone(), self.fmt, self.emit.clone());
        self.tracks.push(Owned {
            track: t.clone(),
            started_sent: false,
            ended_sent: false,
            retired: None,
        });
        t
    }

    fn play(&mut self, src: Source, token: u64, start_ms: u64) {
        let sh = self.shared.clone();
        let t = self.new_track(src, token, start_ms);
        let old_next = sh.next.swap(null_mut(), Ordering::SeqCst);
        let old_cur = sh.cur.swap(Arc::as_ptr(&t).cast_mut(), Ordering::SeqCst);
        self.retire(old_cur);
        self.retire(old_next);
        sh.pos_frames.store(
            frames_at(Duration::from_millis(start_ms), self.fmt.rate),
            Ordering::Relaxed,
        );
        sh.seek_done
            .store(sh.seek_req.load(Ordering::Acquire), Ordering::Release);
        sh.cur_seekable.store(true, Ordering::Relaxed);
        sh.paused.store(false, Ordering::SeqCst);
        sh.set_status(PlayStatus::Playing);
    }

    /// Unpublished track: stop its decoder; free it later in `reap`.
    fn retire(&mut self, p: *mut Track) {
        if p.is_null() {
            return;
        }
        let epoch = self.shared.epoch.load(Ordering::SeqCst);
        if let Some(o) = self.tracks.iter_mut().find(|o| o.ptr() == p) {
            if o.retired.is_none() {
                o.track.stop();
                o.retired = Some(epoch);
            }
        }
    }

    fn process_tracks(&mut self) {
        let sh = self.shared.clone();
        let cur = sh.cur.load(Ordering::SeqCst);
        let mut failed = false;
        for o in self.tracks.iter_mut() {
            let flags = o.track.flags();
            if flags & STOP != 0 {
                continue;
            }
            if !o.started_sent && flags & STARTED != 0 {
                o.started_sent = true;
                let info = o.track.info();
                if o.ptr() == cur {
                    sh.cur_seekable.store(info.seekable, Ordering::Relaxed);
                }
                (self.emit)(EngineEvent::Started {
                    token: o.track.token,
                    info,
                });
            }
            if !o.ended_sent && flags & ENDED != 0 {
                o.ended_sent = true;
                if flags & FAILED == 0 {
                    (self.emit)(EngineEvent::Ended {
                        token: o.track.token,
                    });
                } else {
                    failed = true;
                }
            }
        }
        if failed && sh.cur.load(Ordering::SeqCst).is_null() {
            // The current track failed: playback stops; drop the preload too.
            let n = sh.next.swap(null_mut(), Ordering::SeqCst);
            self.retire(n);
        }
        // A preload that raced with the end of the current track.
        let next = sh.next.load(Ordering::SeqCst);
        if !next.is_null()
            && sh
                .cur
                .compare_exchange(null_mut(), next, Ordering::SeqCst, Ordering::SeqCst)
                .is_ok()
        {
            let _ = sh
                .next
                .compare_exchange(next, null_mut(), Ordering::SeqCst, Ordering::SeqCst);
        }
        // Retire whatever is no longer published.
        let (cur, next) = (
            sh.cur.load(Ordering::SeqCst),
            sh.next.load(Ordering::SeqCst),
        );
        let unpublished: Vec<*mut Track> = self
            .tracks
            .iter()
            .filter(|o| o.retired.is_none() && o.ptr() != cur && o.ptr() != next)
            .map(Owned::ptr)
            .collect();
        for p in unpublished {
            self.retire(p);
        }
        if cur.is_null() && next.is_null() {
            sh.set_status(PlayStatus::Stopped);
        } else if let Some(o) = self.tracks.iter().find(|o| o.ptr() == cur) {
            if o.started_sent {
                sh.cur_seekable
                    .store(o.track.info().seekable, Ordering::Relaxed);
            }
        }
    }

    /// Free retired tracks the callback can no longer reference.
    fn reap(&mut self) {
        let sh = &self.shared;
        let idle = !sh.busy.load(Ordering::SeqCst);
        let epoch = sh.epoch.load(Ordering::SeqCst);
        self.tracks
            .retain(|o| o.retired.is_none_or(|e| !(idle || epoch != e)));
    }

    fn update_output(&mut self) {
        let sh = self.shared.clone();
        let active =
            !sh.cur.load(Ordering::SeqCst).is_null() || !sh.next.load(Ordering::SeqCst).is_null();
        let paused = sh.paused.load(Ordering::SeqCst);
        let Some(output) = self.output.as_mut() else {
            return;
        };
        if active && !paused {
            self.idle_since = None;
            if !self.running {
                output.set_running(true);
                self.running = true;
            }
        } else if self.running {
            let now = Instant::now();
            let since = *self.idle_since.get_or_insert(now);
            // Pause at once; after the end, let the device drain its buffer first.
            if (active && paused) || now.duration_since(since) >= IDLE_PAUSE {
                output.set_running(false);
                self.running = false;
                self.idle_since = None;
                if !active {
                    sh.tap.clear();
                }
            }
        } else {
            self.idle_since = None;
        }
    }

    fn wait_hint(&self) -> Option<Duration> {
        let idle = self.idle_since.map(|s| {
            IDLE_PAUSE
                .saturating_sub(s.elapsed())
                .max(Duration::from_millis(1))
        });
        let reap = self
            .tracks
            .iter()
            .any(|o| o.retired.is_some())
            .then_some(REAP_RETRY);
        match (idle, reap) {
            (Some(a), Some(b)) => Some(a.min(b)),
            (a, b) => a.or(b),
        }
    }

    /// The device disappeared or changed: reopen the default device. If the new
    /// format differs, restart the current/preloaded tracks in that format.
    fn recover_device(&mut self) {
        self.output = None;
        self.running = false;
        match self.backend.open(&self.shared, Some(self.fmt)) {
            Ok((output, fmt)) => {
                self.output = Some(output);
                if fmt != self.fmt {
                    self.fmt = fmt;
                    self.restart_tracks();
                }
            }
            Err(e) => {
                let sh = self.shared.clone();
                let cur = sh.cur.swap(null_mut(), Ordering::SeqCst);
                let next = sh.next.swap(null_mut(), Ordering::SeqCst);
                if let Some(o) = self.tracks.iter().find(|o| o.ptr() == cur) {
                    (self.emit)(EngineEvent::Error {
                        token: o.track.token,
                        message: format!("audio output lost: {e:#}"),
                    });
                }
                self.retire(cur);
                self.retire(next);
                sh.set_status(PlayStatus::Stopped);
            }
        }
    }

    fn restart_tracks(&mut self) {
        let sh = self.shared.clone();
        let cur = sh.cur.load(Ordering::SeqCst);
        let next = sh.next.load(Ordering::SeqCst);
        let find = |p: *mut Track| {
            self.tracks
                .iter()
                .find(|o| o.ptr() == p)
                .map(|o| (o.track.src.clone(), o.track.token, o.started_sent))
        };
        let (cur_t, next_t) = (find(cur), find(next));
        let paused = sh.paused.load(Ordering::SeqCst);
        if let Some((src, token, started)) = cur_t {
            let pos = sh.pos_frames.load(Ordering::Relaxed) * 1000
                / u64::from(sh.out_rate.load(Ordering::Relaxed).max(1));
            self.play(src, token, pos);
            if let Some(o) = self.tracks.last_mut() {
                o.started_sent = started;
            }
            if paused {
                sh.paused.store(true, Ordering::SeqCst);
                sh.set_status(PlayStatus::Paused);
            }
        }
        if let Some((src, token, _)) = next_t {
            let t = self.new_track(src, token, 0);
            let old = sh.next.swap(Arc::as_ptr(&t).cast_mut(), Ordering::SeqCst);
            self.retire(old);
        }
    }

    fn shutdown(mut self) {
        let sh = self.shared.clone();
        let a = sh.cur.swap(null_mut(), Ordering::SeqCst);
        let b = sh.next.swap(null_mut(), Ordering::SeqCst);
        self.retire(a);
        self.retire(b);
        // Dropping the output stops the callback before tracks are freed.
        self.output = None;
        for o in &self.tracks {
            o.track.stop();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::audio::decode::tests::wav_bytes;
    use crate::audio::http::test_server::{serve, Route};
    use crate::audio::Engine;
    use std::path::PathBuf;
    use std::sync::mpsc::RecvTimeoutError;

    const FMT: OutFormat = OutFormat {
        rate: 48_000,
        channels: 2,
    };

    struct Harness {
        engine: Engine,
        rx: Receiver<(EngineEvent, f32)>,
    }

    /// Engine on a device-free output; each event carries the tap's RMS at
    /// the moment it was emitted.
    fn harness(realtime: bool) -> Harness {
        let tap = Arc::new(Tap::new());
        let (tx, rx) = mpsc::channel();
        let probe = tap.clone();
        let engine = Engine::headless(
            tap,
            Box::new(move |ev| {
                let mut buf = vec![0.0f32; 2048];
                let n = probe.snapshot_mono(&mut buf);
                let rms = (buf[..n].iter().map(|s| s * s).sum::<f32>() / n.max(1) as f32).sqrt();
                let _ = tx.send((ev, rms));
            }),
            FMT,
            realtime,
        )
        .expect("headless engine");
        Harness { engine, rx }
    }

    impl Harness {
        fn next(&self) -> (EngineEvent, f32) {
            match self.rx.recv_timeout(Duration::from_secs(15)) {
                Ok(e) => e,
                Err(RecvTimeoutError::Timeout) => panic!("timed out waiting for an engine event"),
                Err(e) => panic!("{e}"),
            }
        }

        fn wait_status(&self, s: PlayStatus) {
            let deadline = Instant::now() + Duration::from_secs(5);
            while self.engine.status() != s {
                assert!(
                    Instant::now() < deadline,
                    "status stuck at {:?}",
                    self.engine.status()
                );
                thread::sleep(Duration::from_millis(5));
            }
        }
    }

    fn wav_file(name: &str, rate: u32, channels: u16, secs: f32) -> PathBuf {
        let path =
            std::env::temp_dir().join(format!("rustune-audio-{}-{name}.wav", std::process::id()));
        let frames = (rate as f32 * secs) as usize;
        std::fs::write(&path, wav_bytes(rate, channels, frames, 440.0)).expect("write wav");
        path
    }

    #[test]
    fn plays_wav_through_chain_to_tap_and_ends() {
        let path = wav_file("basic", 44_100, 2, 0.5);
        let h = harness(false);
        h.engine.set_volume(0.3); // volume is post-tap: must not affect the tap
        h.engine.play(Source::File(path.clone()), 7);
        let (ev, _) = h.next();
        let EngineEvent::Started { token: 7, info } = ev else {
            panic!("expected Started, got {ev:?}");
        };
        assert_eq!(info.duration, Some(Duration::from_millis(500)));
        assert_eq!(info.format.sample_rate, Some(44_100));
        assert_eq!(info.format.channels, Some(2));
        // File size over duration (≈ 1411 kb/s for 16-bit stereo at 44.1 kHz).
        assert!(
            matches!(info.format.kbps, Some(1405..=1420)),
            "{:?}",
            info.format.kbps
        );
        assert!(info.seekable);
        let (ev, rms) = h.next();
        assert_eq!(ev, EngineEvent::Ended { token: 7 });
        // 0.5-amplitude sine → RMS ≈ 0.354 at the tap.
        assert!((rms - 0.354).abs() < 0.03, "tap rms {rms}");
        h.wait_status(PlayStatus::Stopped);
        assert_eq!(h.engine.position(), Duration::ZERO);
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn preload_is_gapless_and_ordered() {
        let a = wav_file("gap-a", 48_000, 1, 0.3);
        let b = wav_file("gap-b", 22_050, 2, 0.3);
        let h = harness(false);
        h.engine.play(Source::File(a.clone()), 1);
        h.engine.preload(Source::File(b.clone()), 2);
        let events: Vec<EngineEvent> = (0..4).map(|_| h.next().0).collect();
        let tokens: Vec<(&str, u64)> = events
            .iter()
            .map(|e| match e {
                EngineEvent::Started { token, .. } => ("start", *token),
                EngineEvent::Ended { token } => ("end", *token),
                other => panic!("unexpected {other:?}"),
            })
            .collect();
        assert_eq!(tokens, [("start", 1), ("end", 1), ("start", 2), ("end", 2)]);
        h.wait_status(PlayStatus::Stopped);
        let _ = std::fs::remove_file(a);
        let _ = std::fs::remove_file(b);
    }

    #[test]
    fn missing_file_reports_error_without_ended() {
        let h = harness(false);
        h.engine
            .play(Source::File("/nonexistent/rustune.flac".into()), 3);
        let (ev, _) = h.next();
        assert!(matches!(ev, EngineEvent::Error { token: 3, .. }), "{ev:?}");
        h.wait_status(PlayStatus::Stopped);
        assert!(h.rx.recv_timeout(Duration::from_millis(200)).is_err());
        assert!(h.engine.seek(Duration::from_secs(1)).is_err());
    }

    #[test]
    fn pause_seek_resume() {
        let path = wav_file("seek", 48_000, 2, 6.0);
        let h = harness(true);
        h.engine.play(Source::File(path.clone()), 5);
        assert!(matches!(h.next().0, EngineEvent::Started { token: 5, .. }));
        h.engine.pause();
        assert_eq!(h.engine.status(), PlayStatus::Paused);
        thread::sleep(Duration::from_millis(50));
        let p1 = h.engine.position();
        thread::sleep(Duration::from_millis(100));
        assert_eq!(h.engine.position(), p1, "position advanced while paused");
        assert!(p1 < Duration::from_secs(2));

        h.engine.seek(Duration::from_millis(5_500)).unwrap();
        assert_eq!(h.engine.position(), Duration::from_millis(5_500));
        h.engine.resume();
        assert_eq!(h.engine.status(), PlayStatus::Playing);
        let (ev, _) = h.next();
        assert_eq!(ev, EngineEvent::Ended { token: 5 });
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn plays_http_range_and_icy_streams() {
        let wav = wav_bytes(44_100, 2, 22_050, 440.0);
        let metaint = 8_192;
        let mut icy = Vec::new();
        for (i, chunk) in wav.chunks(metaint).enumerate() {
            icy.extend_from_slice(chunk);
            if chunk.len() == metaint {
                if i == 0 {
                    let text = b"StreamTitle='Test - Song';";
                    let blocks = text.len().div_ceil(16);
                    icy.push(blocks as u8);
                    icy.extend_from_slice(text);
                    icy.resize(icy.len() + blocks * 16 - text.len(), 0);
                } else {
                    icy.push(0);
                }
            }
        }
        let srv = serve(vec![
            Route {
                path: "/song.wav",
                body: wav,
                content_type: "audio/wav",
                ranges: true,
                extra_headers: Vec::new(),
            },
            Route {
                path: "/radio",
                body: icy,
                content_type: "audio/wav",
                ranges: false,
                extra_headers: vec![("icy-metaint", metaint.to_string())],
            },
        ]);
        let h = harness(false);

        h.engine
            .play(Source::Url(format!("{}/song.wav", srv.base)), 10);
        let (ev, _) = h.next();
        let EngineEvent::Started { token: 10, info } = ev else {
            panic!("expected Started, got {ev:?}");
        };
        assert!(info.seekable);
        assert_eq!(info.duration, Some(Duration::from_millis(500)));
        assert_eq!(h.next().0, EngineEvent::Ended { token: 10 });

        h.engine
            .play(Source::Url(format!("{}/radio", srv.base)), 11);
        let mut seen = Vec::new();
        loop {
            let (ev, _) = h.next();
            let done = matches!(ev, EngineEvent::Ended { .. } | EngineEvent::Error { .. });
            seen.push(ev);
            if done {
                break;
            }
        }
        assert!(seen.contains(&EngineEvent::StreamTitle {
            token: 11,
            title: "Test - Song".into()
        }));
        assert!(seen
            .iter()
            .any(|e| matches!(e, EngineEvent::Started { token: 11, info } if !info.seekable)));
        assert_eq!(seen.last(), Some(&EngineEvent::Ended { token: 11 }));
    }

    #[test]
    fn replacing_track_suppresses_its_events() {
        let a = wav_file("replace-a", 48_000, 2, 5.0);
        let b = wav_file("replace-b", 48_000, 2, 0.2);
        let h = harness(true);
        h.engine.play(Source::File(a.clone()), 1);
        assert!(matches!(h.next().0, EngineEvent::Started { token: 1, .. }));
        h.engine.play(Source::File(b.clone()), 2);
        assert!(matches!(h.next().0, EngineEvent::Started { token: 2, .. }));
        assert_eq!(h.next().0, EngineEvent::Ended { token: 2 });
        h.engine.stop();
        assert_eq!(h.engine.status(), PlayStatus::Stopped);
        let _ = std::fs::remove_file(a);
        let _ = std::fs::remove_file(b);
    }
}
