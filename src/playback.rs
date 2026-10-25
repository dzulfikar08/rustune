//! Playback backends behind one seam. Local files go to rodio (pure Rust);
//! streams, and local files rodio cannot decode, go to mpv. Every control
//! call is routed to the backend that is *actually* playing, so a decode
//! fallback never sends pause/seek to an idle rodio player.
//!
//! The replacement audio engine plugs in here: `play_local`/`play_url`,
//! `set_paused`, `seek`, `stop`, `finished`, plus whatever volume/EQ setters
//! it offers, called from `Controller::apply`.

use std::path::Path;
use std::time::Duration;

use anyhow::{Context, Result};
use tokio::sync::mpsc::UnboundedSender;

use crate::event::AppEvent;
use crate::player::{LocalHandle, MpvHandle};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Backend {
    Idle,
    Local,
    Mpv,
}

pub struct Playback {
    tx: UnboundedSender<AppEvent>,
    local: Option<LocalHandle>,
    mpv: Option<MpvHandle>,
    active: Backend,
}

fn file_url(path: &Path) -> String {
    format!("file://{}", path.display())
}

impl Playback {
    pub fn new(tx: UnboundedSender<AppEvent>) -> Self {
        Self {
            tx,
            local: None,
            mpv: None,
            active: Backend::Idle,
        }
    }

    pub fn active(&self) -> Backend {
        self.active
    }

    fn stop_mpv(&mut self) {
        if let Some(mpv) = self.mpv.take() {
            mpv.shutdown();
        }
    }

    /// Play a local file with rodio, falling back to mpv for formats rodio
    /// cannot decode. `mpv_missing` short-circuits the fallback with a clear
    /// error instead of a spawn failure.
    pub fn play_local(&mut self, path: &Path, mpv_missing: bool) -> Result<()> {
        if self.local.is_none() {
            // The output device is opened once and kept for the session.
            self.local = LocalHandle::new(self.tx.clone()).ok();
        }
        let attempt = match &self.local {
            Some(local) => {
                let result = local.play_file(&path.to_string_lossy());
                if result.is_err() {
                    local.stop();
                }
                result
            }
            None => Err(anyhow::anyhow!("no audio output device")),
        };
        let rodio_err = match attempt {
            Ok(()) => {
                self.stop_mpv();
                self.active = Backend::Local;
                return Ok(());
            }
            Err(e) => e,
        };
        if mpv_missing {
            self.active = Backend::Idle;
            anyhow::bail!("{rodio_err:#} (mpv fallback not installed)");
        }
        self.play_url(&file_url(path), false)
            .with_context(|| format!("{rodio_err:#}; mpv fallback failed"))
    }

    /// Play a URL (or `file://` URL) through mpv.
    pub fn play_url(&mut self, url: &str, mpv_missing: bool) -> Result<()> {
        if let Some(local) = &self.local {
            local.stop();
        }
        if mpv_missing {
            self.active = Backend::Idle;
            anyhow::bail!("Streaming needs mpv, which is not installed (see `rustune doctor`)");
        }
        if self.mpv.is_none() {
            match MpvHandle::new(self.tx.clone()) {
                Ok(handle) => self.mpv = Some(handle),
                Err(e) => {
                    self.active = Backend::Idle;
                    return Err(e.context("Cannot start mpv (needed for streams)"));
                }
            }
        }
        if let Some(mpv) = &self.mpv {
            mpv.loadfile(url)?;
        }
        self.active = Backend::Mpv;
        Ok(())
    }

    pub fn set_paused(&self, paused: bool) {
        match self.active {
            Backend::Local => {
                if let Some(local) = &self.local {
                    local.set_pause(paused);
                }
            }
            Backend::Mpv => {
                if let Some(mpv) = &self.mpv {
                    let _ = mpv.set_pause(paused);
                }
            }
            Backend::Idle => {}
        }
    }

    pub fn seek(&self, pos: Duration) -> Result<()> {
        match self.active {
            Backend::Local => match &self.local {
                Some(local) => local.seek_to(pos),
                None => Ok(()),
            },
            Backend::Mpv => match &self.mpv {
                Some(mpv) => mpv.seek_to(pos.as_secs_f64()),
                None => Ok(()),
            },
            Backend::Idle => Ok(()),
        }
    }

    pub fn stop(&mut self) {
        if let Some(local) = &self.local {
            local.stop();
        }
        self.stop_mpv();
        self.active = Backend::Idle;
    }

    /// Has the local backend run out of audio? (mpv reports end-of-track
    /// via `AppEvent::PlaybackComplete` where supported.)
    pub fn finished(&self) -> bool {
        self.active == Backend::Local && self.local.as_ref().is_some_and(LocalHandle::is_finished)
    }

    pub fn shutdown(mut self) {
        self.stop_mpv();
        if let Some(local) = self.local.take() {
            local.shutdown();
        }
    }
}
