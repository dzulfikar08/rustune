//! macOS: `MPNowPlayingInfoCenter` (title, artist, album, duration,
//! elapsed time, rate) and `MPRemoteCommandCenter` (play, pause, toggle,
//! stop, next, previous, scrubbing via `changePlaybackPositionCommand`).
//!
//! MediaPlayer delivers remote commands on the main thread's run loop, and
//! a terminal app never runs one. `host` therefore moves the player to a
//! second thread and parks the main thread in `CFRunLoop` (asleep until
//! macOS has something to deliver); all MediaPlayer calls are dispatched
//! onto the main queue. Once media controls start, the process registers
//! as an accessory `NSApplication` (no Dock icon), as Now Playing expects
//! an app; with `media_controls = false` the main thread only sleeps.

use std::io;
use std::ptr::NonNull;
use std::sync::atomic::{AtomicBool, AtomicU8, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use block2::RcBlock;
use dispatch2::DispatchQueue;
use objc2::rc::Retained;
use objc2::runtime::{AnyClass, AnyObject};
use objc2::{msg_send, MainThreadMarker};
use objc2_core_foundation::{kCFRunLoopDefaultMode, CFRunLoop, CFRunLoopRunResult};
use objc2_foundation::{NSDictionary, NSNumber, NSString};
use objc2_media_player::{
    MPChangePlaybackPositionCommandEvent, MPMediaItemPropertyAlbumTitle, MPMediaItemPropertyArtist,
    MPMediaItemPropertyPlaybackDuration, MPMediaItemPropertyTitle, MPNowPlayingInfoCenter,
    MPNowPlayingInfoPropertyElapsedPlaybackTime, MPNowPlayingInfoPropertyPlaybackRate,
    MPNowPlayingPlaybackState, MPRemoteCommand, MPRemoteCommandCenter, MPRemoteCommandEvent,
    MPRemoteCommandHandlerStatus,
};
use tokio::sync::mpsc::UnboundedSender;

use super::track::{Changes, Clock, State};
use crate::command::Command;
use crate::event::AppEvent;
use crate::view::PlayStatus;

/// The main thread runs the Cocoa run loop for the player thread.
static HOSTED: AtomicBool = AtomicBool::new(false);
/// Media controls registered (Now Playing must be cleared on exit).
static STARTED: AtomicBool = AtomicBool::new(false);
/// The player thread finished; the main thread leaves its run loop.
static DONE: AtomicBool = AtomicBool::new(false);

/// `NSApplicationActivationPolicyAccessory`: no Dock icon, no menu bar.
const ACTIVATION_POLICY_ACCESSORY: isize = 1;

fn init_application() {
    // AppKit is loaded on demand: linking it would load it on every launch,
    // including `--help` and runs with media controls off.
    // SAFETY: dlopen of a system framework by constant path; the handle is
    // kept for the life of the process.
    unsafe {
        libc::dlopen(
            c"/System/Library/Frameworks/AppKit.framework/AppKit".as_ptr(),
            libc::RTLD_LAZY,
        );
    }
    let Some(class) = AnyClass::get(c"NSApplication") else {
        return;
    };
    // SAFETY: +[NSApplication sharedApplication] and -setActivationPolicy:
    // are called on the main thread with their documented signatures.
    unsafe {
        let app: Option<Retained<AnyObject>> = msg_send![class, sharedApplication];
        if let Some(app) = app {
            let _: bool = msg_send![&*app, setActivationPolicy: ACTIVATION_POLICY_ACCESSORY];
        }
    }
}

/// Ends the main thread's run loop when the player thread exits (also on
/// panic, so the process never hangs).
struct StopMainLoop;

impl Drop for StopMainLoop {
    fn drop(&mut self) {
        DONE.store(true, Ordering::Release);
        DispatchQueue::main().exec_async(|| {
            if let Some(main) = CFRunLoop::main() {
                main.stop();
            }
        });
    }
}

/// Run `app` on a new thread while this (main) thread serves the run loop.
pub fn host<T: Send + 'static>(app: impl FnOnce() -> T + Send + 'static) -> io::Result<T> {
    if MainThreadMarker::new().is_none() {
        return Ok(app());
    }
    HOSTED.store(true, Ordering::Release);
    let player = std::thread::Builder::new()
        .name("rustune-player".into())
        .spawn(move || {
            let _stop = StopMainLoop;
            app()
        });
    let player = match player {
        Ok(handle) => handle,
        Err(e) => {
            HOSTED.store(false, Ordering::Release);
            return Err(e);
        }
    };
    while !DONE.load(Ordering::Acquire) {
        // SAFETY: reading an immutable CoreFoundation constant.
        let mode = unsafe { kCFRunLoopDefaultMode };
        // The main run loop always has the main dispatch queue as a source;
        // the sleep only guards against a busy loop if it ever had none.
        if CFRunLoop::run_in_mode(mode, 3600.0, false) == CFRunLoopRunResult::Finished {
            std::thread::sleep(Duration::from_millis(100));
        }
    }
    HOSTED.store(false, Ordering::Release);
    if STARTED.load(Ordering::Acquire) {
        clear_now_playing();
    }
    match player.join() {
        Ok(value) => Ok(value),
        Err(panic) => std::panic::resume_unwind(panic),
    }
}

fn clear_now_playing() {
    // SAFETY: MediaPlayer singletons, used on the main thread.
    unsafe {
        let center = MPNowPlayingInfoCenter::defaultCenter();
        center.setNowPlayingInfo(None);
        center.setPlaybackState(MPNowPlayingPlaybackState::Stopped);
    }
}

fn status_code(status: PlayStatus) -> u8 {
    match status {
        PlayStatus::Stopped => 0,
        PlayStatus::Playing => 1,
        PlayStatus::Paused => 2,
    }
}

/// A scrub target from `changePlaybackPositionCommand`, in seconds.
pub fn seek_target(seconds: f64) -> Option<Duration> {
    if seconds.is_nan() {
        return None;
    }
    Duration::try_from_secs_f64(seconds.max(0.0)).ok()
}

/// `playCommand` must not restart a playing track (`Command::Play` does).
fn play_command(status: u8) -> Option<Command> {
    (status != status_code(PlayStatus::Playing)).then_some(Command::Play)
}

/// Everything the Now Playing widget shows (owned; built on the player
/// thread, applied on the main thread).
#[derive(Debug, Clone, PartialEq)]
pub struct Info {
    pub status: PlayStatus,
    pub title: String,
    pub artist: Option<String>,
    pub album: Option<String>,
    /// Seconds.
    pub duration: Option<f64>,
    /// Seconds; macOS extrapolates from here using `rate`.
    pub elapsed: f64,
    pub can_next: bool,
    pub can_prev: bool,
    pub can_seek: bool,
}

impl Info {
    pub fn new(state: &State, clock: &Clock, now: Instant) -> Self {
        let t = &state.track;
        Self {
            status: state.status,
            title: t.title.clone(),
            artist: t.artist.clone(),
            album: t.album.clone(),
            duration: t.length.map(|d| d.as_secs_f64()),
            elapsed: clock.position(now).as_secs_f64(),
            can_next: state.can_next,
            can_prev: state.can_prev,
            can_seek: state.can_seek,
        }
    }

    pub fn rate(&self) -> f64 {
        if self.status == PlayStatus::Playing {
            1.0
        } else {
            0.0
        }
    }

    pub fn playback_state(&self) -> MPNowPlayingPlaybackState {
        match self.status {
            PlayStatus::Playing => MPNowPlayingPlaybackState::Playing,
            PlayStatus::Paused => MPNowPlayingPlaybackState::Paused,
            PlayStatus::Stopped => MPNowPlayingPlaybackState::Stopped,
        }
    }

    /// The `nowPlayingInfo` dictionary; `None` clears it (stopped).
    pub fn dictionary(&self) -> Option<Retained<NSDictionary<NSString, AnyObject>>> {
        if self.status == PlayStatus::Stopped {
            return None;
        }
        let title = NSString::from_str(&self.title);
        let artist = self.artist.as_deref().map(NSString::from_str);
        let album = self.album.as_deref().map(NSString::from_str);
        let duration = self.duration.map(NSNumber::new_f64);
        let elapsed = NSNumber::new_f64(self.elapsed);
        let rate = NSNumber::new_f64(self.rate());
        let mut keys: Vec<&NSString> = Vec::with_capacity(6);
        let mut values: Vec<&AnyObject> = Vec::with_capacity(6);
        // SAFETY: the keys are immutable framework string constants.
        unsafe {
            keys.push(MPMediaItemPropertyTitle);
            values.push(&title);
            if let Some(artist) = &artist {
                keys.push(MPMediaItemPropertyArtist);
                values.push(artist);
            }
            if let Some(album) = &album {
                keys.push(MPMediaItemPropertyAlbumTitle);
                values.push(album);
            }
            if let Some(duration) = &duration {
                keys.push(MPMediaItemPropertyPlaybackDuration);
                values.push(duration);
            }
            keys.push(MPNowPlayingInfoPropertyElapsedPlaybackTime);
            values.push(&elapsed);
            keys.push(MPNowPlayingInfoPropertyPlaybackRate);
            values.push(&rate);
        }
        Some(NSDictionary::from_slices(&keys, &values))
    }

    /// Push to the Now Playing center (main thread).
    fn apply(&self) {
        let dict = self.dictionary();
        // SAFETY: MediaPlayer singletons, used on the main thread.
        unsafe {
            let commands = MPRemoteCommandCenter::sharedCommandCenter();
            commands.nextTrackCommand().setEnabled(self.can_next);
            commands.previousTrackCommand().setEnabled(self.can_prev);
            commands
                .changePlaybackPositionCommand()
                .setEnabled(self.can_seek);
            let center = MPNowPlayingInfoCenter::defaultCenter();
            center.setNowPlayingInfo(dict.as_deref());
            center.setPlaybackState(self.playback_state());
        }
    }
}

/// Attach a handler that turns `command` into a player `Command`.
fn on_command(
    command: &MPRemoteCommand,
    tx: &UnboundedSender<AppEvent>,
    status: &Arc<AtomicU8>,
    map: fn(u8) -> Option<Command>,
) {
    let tx = tx.clone();
    let status = Arc::clone(status);
    let handler = RcBlock::new(
        move |_event: NonNull<MPRemoteCommandEvent>| -> MPRemoteCommandHandlerStatus {
            if let Some(cmd) = map(status.load(Ordering::Acquire)) {
                let _ = tx.send(AppEvent::Command(cmd));
            }
            MPRemoteCommandHandlerStatus::Success
        },
    );
    // SAFETY: main thread; the command center copies the block and keeps
    // the target for the life of the process.
    unsafe {
        command.setEnabled(true);
        let _ = command.addTargetWithHandler(&handler);
    }
}

/// Become an accessory app and register the remote command handlers
/// (main thread).
fn register(tx: &UnboundedSender<AppEvent>, status: &Arc<AtomicU8>) {
    init_application();
    STARTED.store(true, Ordering::Release);
    // SAFETY: MediaPlayer singletons, used on the main thread.
    let commands = unsafe { MPRemoteCommandCenter::sharedCommandCenter() };
    unsafe {
        on_command(&commands.playCommand(), tx, status, play_command);
        on_command(&commands.pauseCommand(), tx, status, |_| {
            Some(Command::Pause)
        });
        on_command(&commands.togglePlayPauseCommand(), tx, status, |_| {
            Some(Command::TogglePause)
        });
        on_command(&commands.stopCommand(), tx, status, |_| Some(Command::Stop));
        on_command(&commands.nextTrackCommand(), tx, status, |_| {
            Some(Command::Next)
        });
        on_command(&commands.previousTrackCommand(), tx, status, |_| {
            Some(Command::Prev)
        });
    }
    let tx = tx.clone();
    let scrub = RcBlock::new(
        move |event: NonNull<MPRemoteCommandEvent>| -> MPRemoteCommandHandlerStatus {
            // SAFETY: MediaPlayer passes a valid event for the call's duration.
            let event = unsafe { event.as_ref() };
            let target = event
                .downcast_ref::<MPChangePlaybackPositionCommandEvent>()
                .and_then(|e| seek_target(unsafe { e.positionTime() }));
            match target {
                Some(pos) => {
                    let _ = tx.send(AppEvent::Command(Command::SeekTo(pos)));
                    MPRemoteCommandHandlerStatus::Success
                }
                None => MPRemoteCommandHandlerStatus::CommandFailed,
            }
        },
    );
    // SAFETY: as in `on_command`.
    unsafe {
        let position = commands.changePlaybackPositionCommand();
        position.setEnabled(false);
        let _ = position.addTargetWithHandler(&scrub);
    }
}

pub struct NowPlaying {
    /// Last published status, read by the play handler.
    status: Arc<AtomicU8>,
}

impl NowPlaying {
    /// `None` unless `host` runs the main run loop.
    pub fn start(tx: UnboundedSender<AppEvent>) -> Option<Self> {
        if !HOSTED.load(Ordering::Acquire) {
            return None;
        }
        let status = Arc::new(AtomicU8::new(status_code(PlayStatus::Stopped)));
        let handler_status = Arc::clone(&status);
        DispatchQueue::main().exec_async(move || register(&tx, &handler_status));
        Some(Self { status })
    }

    pub fn publish(&self, state: &State, clock: &Clock, changes: &Changes) {
        self.status
            .store(status_code(state.status), Ordering::Release);
        // Volume, shuffle and repeat aren't shown; elapsed time only needs
        // an update when the clock stops, starts or jumps.
        if !(changes.track || changes.status || changes.caps || changes.seeked.is_some()) {
            return;
        }
        let info = Info::new(state, clock, Instant::now());
        DispatchQueue::main().exec_async(move || info.apply());
    }
}

/// One line for `rustune doctor`.
pub fn doctor() -> String {
    "[ok] media controls: macOS Now Playing and media keys".to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mediactl::track::tests::{item, view};
    use crate::mediactl::track::Tracker;

    fn info(status: PlayStatus) -> Info {
        let now = Instant::now();
        let items = [item("/m/a.flac", "Song", Some("Band"))];
        let mut tracker = Tracker::new(now);
        tracker.observe(
            &view(
                &items,
                Some(0),
                "Band - Song",
                status,
                Duration::from_secs(42),
            ),
            now,
        );
        Info::new(&tracker.state, &tracker.clock, now)
    }

    fn string(dict: &NSDictionary<NSString, AnyObject>, key: &NSString) -> Option<String> {
        let value = dict.objectForKey(key)?;
        Some(value.downcast::<NSString>().ok()?.to_string())
    }

    fn number(dict: &NSDictionary<NSString, AnyObject>, key: &NSString) -> Option<f64> {
        let value = dict.objectForKey(key)?;
        Some(value.downcast::<NSNumber>().ok()?.as_f64())
    }

    #[test]
    fn info_from_player_view() {
        let i = info(PlayStatus::Playing);
        assert_eq!(i.title, "Song");
        assert_eq!(i.artist.as_deref(), Some("Band"));
        assert_eq!(i.album.as_deref(), Some("Album"));
        assert_eq!(i.duration, Some(200.0));
        assert_eq!(i.elapsed, 42.0);
        assert_eq!(i.rate(), 1.0);
        assert_eq!(i.playback_state(), MPNowPlayingPlaybackState::Playing);
        let paused = info(PlayStatus::Paused);
        assert_eq!(paused.rate(), 0.0);
        assert_eq!(paused.playback_state(), MPNowPlayingPlaybackState::Paused);
    }

    #[test]
    fn now_playing_dictionary() {
        let dict = info(PlayStatus::Playing).dictionary().expect("dictionary");
        unsafe {
            assert_eq!(
                string(&dict, MPMediaItemPropertyTitle).as_deref(),
                Some("Song")
            );
            assert_eq!(
                string(&dict, MPMediaItemPropertyArtist).as_deref(),
                Some("Band")
            );
            assert_eq!(
                string(&dict, MPMediaItemPropertyAlbumTitle).as_deref(),
                Some("Album")
            );
            assert_eq!(
                number(&dict, MPMediaItemPropertyPlaybackDuration),
                Some(200.0)
            );
            assert_eq!(
                number(&dict, MPNowPlayingInfoPropertyElapsedPlaybackTime),
                Some(42.0)
            );
            assert_eq!(
                number(&dict, MPNowPlayingInfoPropertyPlaybackRate),
                Some(1.0)
            );
        }
        assert!(info(PlayStatus::Stopped).dictionary().is_none());
    }

    #[test]
    fn remote_command_mapping() {
        assert_eq!(
            play_command(status_code(PlayStatus::Paused)),
            Some(Command::Play)
        );
        assert_eq!(
            play_command(status_code(PlayStatus::Stopped)),
            Some(Command::Play)
        );
        assert_eq!(play_command(status_code(PlayStatus::Playing)), None);
        assert_eq!(seek_target(12.5), Some(Duration::from_millis(12_500)));
        assert_eq!(seek_target(-3.0), Some(Duration::ZERO));
        assert_eq!(seek_target(f64::NAN), None);
        assert_eq!(seek_target(f64::INFINITY), None);
    }

    #[test]
    fn start_requires_the_hosted_run_loop() {
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
        assert!(NowPlaying::start(tx).is_none());
    }
}
