//! OS media integration: media keys, headset buttons and the system's
//! now-playing widgets control rustune and show what it plays.
//!
//! - Linux: MPRIS2 on the D-Bus session bus (`mpris`, over the small
//!   built-in client in `dbus`). Off when there is no session bus.
//! - macOS: Now Playing + remote commands (`macos`); needs `host`.
//! - Other platforms (Windows): unsupported; `doctor` says so.
//!
//! Commands from the OS reach `Controller::apply` as `AppEvent::Command`;
//! state is read from `App::player_view()` by an observer that publishes
//! only what changed (position is extrapolated by the OS side, so clock
//! ticks publish nothing unless the position jumps).

use tokio::sync::mpsc::UnboundedSender;

use crate::controller::Observer;
use crate::event::AppEvent;

#[cfg(target_os = "linux")]
mod dbus;
#[cfg(target_os = "macos")]
mod macos;
#[cfg(target_os = "linux")]
mod mpris;
#[cfg(any(target_os = "linux", target_os = "macos"))]
mod track;

/// Start the media integration; `None` when disabled or unsupported.
/// Never fails the app: a missing session bus just leaves it off.
pub fn start(enabled: bool, tx: UnboundedSender<AppEvent>) -> Option<Observer> {
    if !enabled {
        return None;
    }
    start_backend(tx)
}

#[cfg(target_os = "linux")]
fn start_backend(tx: UnboundedSender<AppEvent>) -> Option<Observer> {
    let mpris = mpris::Mpris::spawn(tx).ok()?;
    Some(observer(move |state, clock, changes| {
        mpris.publish(state, clock, changes);
    }))
}

#[cfg(target_os = "macos")]
fn start_backend(tx: UnboundedSender<AppEvent>) -> Option<Observer> {
    let now_playing = macos::NowPlaying::start(tx)?;
    Some(observer(move |state, clock, changes| {
        now_playing.publish(state, clock, changes);
    }))
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
fn start_backend(_tx: UnboundedSender<AppEvent>) -> Option<Observer> {
    None
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
fn observer(
    mut publish: impl FnMut(&track::State, &track::Clock, &track::Changes) + 'static,
) -> Observer {
    let mut tracker = track::Tracker::new(std::time::Instant::now());
    Box::new(move |app: &crate::app::App| {
        let changes = tracker.observe(&app.player_view(), std::time::Instant::now());
        if changes.any() {
            publish(&tracker.state, &tracker.clock, &changes);
        }
    })
}

/// Run the player (`app`) the way the platform's media integration needs.
/// macOS delivers remote commands through the main thread's run loop, so
/// the player runs on a second thread while the main thread sleeps in that
/// loop (it does nothing unless `start` enables media controls). Elsewhere
/// `app` simply runs here.
#[cfg(target_os = "macos")]
pub fn host(app: impl FnOnce() -> anyhow::Result<()> + Send + 'static) -> anyhow::Result<()> {
    macos::host(app)?
}

#[cfg(not(target_os = "macos"))]
pub fn host(app: impl FnOnce() -> anyhow::Result<()> + Send + 'static) -> anyhow::Result<()> {
    app()
}

/// One status line for `rustune doctor`.
pub fn doctor(enabled: bool) -> String {
    if enabled {
        platform_doctor()
    } else {
        "[--] media controls: off (media_controls = false)".to_string()
    }
}

#[cfg(target_os = "linux")]
use mpris::doctor as platform_doctor;

#[cfg(target_os = "macos")]
use macos::doctor as platform_doctor;

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
fn platform_doctor() -> String {
    "[--] media controls: not supported on this platform".to_string()
}
