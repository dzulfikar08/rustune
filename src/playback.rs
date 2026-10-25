//! The playback seam between the controller and the native audio engine
//! (`crate::audio`). Without an output device the engine is unavailable:
//! play requests then fail with the reason and the TUI keeps working.

use std::sync::Arc;
use std::time::Duration;

use anyhow::{anyhow, Result};
use tokio::sync::mpsc::UnboundedSender;

use crate::audio::{Engine, ReplayGainMode, Source};
use crate::event::AppEvent;
use crate::tap::Tap;
use crate::view::EqSettings;

/// What the controller needs from an audio output. Implemented by the real
/// engine and, in tests, by a recorder.
pub trait Output {
    fn play(&self, src: Source, token: u64);
    fn preload(&self, src: Source, token: u64);
    fn cancel_preload(&self);
    fn pause(&self);
    fn resume(&self);
    fn stop(&self);
    fn seek(&self, pos: Duration) -> Result<()>;
    fn position(&self) -> Duration;
    fn set_volume(&self, v: f32);
    fn set_balance(&self, b: f32);
    fn set_eq(&self, eq: EqSettings);
    fn set_replaygain(&self, mode: ReplayGainMode);
}

impl Output for Engine {
    fn play(&self, src: Source, token: u64) {
        Engine::play(self, src, token);
    }
    fn preload(&self, src: Source, token: u64) {
        Engine::preload(self, src, token);
    }
    fn cancel_preload(&self) {
        Engine::cancel_preload(self);
    }
    fn pause(&self) {
        Engine::pause(self);
    }
    fn resume(&self) {
        Engine::resume(self);
    }
    fn stop(&self) {
        Engine::stop(self);
    }
    fn seek(&self, pos: Duration) -> Result<()> {
        Engine::seek(self, pos)
    }
    fn position(&self) -> Duration {
        Engine::position(self)
    }
    fn set_volume(&self, v: f32) {
        Engine::set_volume(self, v);
    }
    fn set_balance(&self, b: f32) {
        Engine::set_balance(self, b);
    }
    fn set_eq(&self, eq: EqSettings) {
        Engine::set_eq(self, eq);
    }
    fn set_replaygain(&self, mode: ReplayGainMode) {
        Engine::set_replaygain(self, mode);
    }
}

pub struct Playback {
    out: std::result::Result<Box<dyn Output>, String>,
    tap: Arc<Tap>,
}

impl Playback {
    /// Open the default output device. Engine events arrive as
    /// `AppEvent::Engine` on `tx`.
    pub fn new(tx: UnboundedSender<AppEvent>) -> Self {
        let tap = Arc::new(Tap::new());
        let on_event = Box::new(move |event| {
            let _ = tx.send(AppEvent::Engine(event));
        });
        let out = Engine::new(tap.clone(), on_event)
            .map(|engine| Box::new(engine) as Box<dyn Output>)
            .map_err(|e| format!("No audio output: {e:#}"));
        Self { out, tap }
    }

    /// A seam without audio (`reason` is reported on every play request).
    #[cfg(test)]
    pub fn unavailable(reason: &str) -> Self {
        Self {
            out: Err(reason.to_string()),
            tap: Arc::new(Tap::new()),
        }
    }

    #[cfg(test)]
    pub fn with_output(out: Box<dyn Output>) -> Self {
        Self {
            out: Ok(out),
            tap: Arc::new(Tap::new()),
        }
    }

    /// Samples after the DSP chain, shared with the visualizer.
    pub fn tap(&self) -> &Arc<Tap> {
        &self.tap
    }

    /// Why audio is unavailable (no output device), if it is.
    pub fn error(&self) -> Option<&str> {
        self.out.as_ref().err().map(String::as_str)
    }

    fn with<T>(&self, f: impl FnOnce(&dyn Output) -> T) -> Option<T> {
        self.out.as_deref().ok().map(f)
    }

    pub fn play(&self, src: Source, token: u64) -> Result<()> {
        match &self.out {
            Ok(out) => {
                out.play(src, token);
                Ok(())
            }
            Err(reason) => Err(anyhow!("{reason}")),
        }
    }

    pub fn preload(&self, src: Source, token: u64) {
        self.with(|o| o.preload(src, token));
    }

    pub fn cancel_preload(&self) {
        self.with(|o| o.cancel_preload());
    }

    pub fn set_paused(&self, paused: bool) {
        self.with(|o| if paused { o.pause() } else { o.resume() });
    }

    pub fn seek(&self, pos: Duration) -> Result<()> {
        self.with(|o| o.seek(pos))
            .unwrap_or_else(|| Err(anyhow!("nothing is playing")))
    }

    pub fn stop(&self) {
        self.with(|o| o.stop());
    }

    pub fn position(&self) -> Duration {
        self.with(|o| o.position()).unwrap_or_default()
    }

    pub fn set_volume(&self, v: f32) {
        self.with(|o| o.set_volume(v));
    }

    pub fn set_balance(&self, b: f32) {
        self.with(|o| o.set_balance(b));
    }

    pub fn set_eq(&self, eq: EqSettings) {
        self.with(|o| o.set_eq(eq));
    }

    pub fn set_replaygain(&self, mode: ReplayGainMode) {
        self.with(|o| o.set_replaygain(mode));
    }
}
