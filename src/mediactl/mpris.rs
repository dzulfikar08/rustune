//! MPRIS2 on the session bus: `org.mpris.MediaPlayer2` and `.Player` with
//! properties, `PropertiesChanged` and `Seeked` signals.
//!
//! Threads: the service thread connects, then reads method calls and
//! answers them from the last published state; a writer thread owns the
//! socket's write side. The app thread only updates the published state
//! and queues signals, so a stalled bus can never block the UI.

use std::io;
use std::net::Shutdown;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::net::UnixStream;
use std::path::Path;
use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant};

use tokio::sync::mpsc::UnboundedSender;

use super::dbus::{self, Bus, Message, Value, METHOD_CALL, NO_REPLY_EXPECTED};
use super::track::{Changes, Clock, Location, State, Track};
use crate::command::Command;
use crate::event::AppEvent;
use crate::view::{PlayStatus, RepeatMode};

pub const BUS_NAME: &str = "org.mpris.MediaPlayer2.rustune";
pub const OBJECT_PATH: &str = "/org/mpris/MediaPlayer2";
const ROOT_IFACE: &str = "org.mpris.MediaPlayer2";
const PLAYER_IFACE: &str = "org.mpris.MediaPlayer2.Player";
const PROPS_IFACE: &str = "org.freedesktop.DBus.Properties";
const INTROSPECT_IFACE: &str = "org.freedesktop.DBus.Introspectable";
const PEER_IFACE: &str = "org.freedesktop.DBus.Peer";
pub const NO_TRACK: &str = "/org/mpris/MediaPlayer2/TrackList/NoTrack";
const TRACK_PREFIX: &str = "/org/mpris/MediaPlayer2/rustune/track/";
/// Setup (connect, auth, Hello, RequestName) must not hang the thread.
const SETUP_TIMEOUT: Duration = Duration::from_secs(5);

const CONNECTING: u8 = 0;
const UP: u8 = 1;
const DOWN: u8 = 2;

const ROOT_PROPS: &[&str] = &[
    "CanQuit",
    "CanRaise",
    "HasTrackList",
    "Identity",
    "SupportedUriSchemes",
    "SupportedMimeTypes",
];
const PLAYER_PROPS: &[&str] = &[
    "PlaybackStatus",
    "LoopStatus",
    "Rate",
    "Shuffle",
    "Metadata",
    "Volume",
    "Position",
    "MinimumRate",
    "MaximumRate",
    "CanGoNext",
    "CanGoPrevious",
    "CanPlay",
    "CanPause",
    "CanSeek",
    "CanControl",
];

const INTROSPECTION: &str = r#"<!DOCTYPE node PUBLIC "-//freedesktop//DTD D-BUS Object Introspection 1.0//EN" "http://www.freedesktop.org/standards/dbus/1.0/introspect.dtd">
<node>
  <interface name="org.freedesktop.DBus.Introspectable">
    <method name="Introspect"><arg name="xml" type="s" direction="out"/></method>
  </interface>
  <interface name="org.freedesktop.DBus.Peer">
    <method name="Ping"/>
    <method name="GetMachineId"><arg name="id" type="s" direction="out"/></method>
  </interface>
  <interface name="org.freedesktop.DBus.Properties">
    <method name="Get"><arg name="interface" type="s" direction="in"/><arg name="name" type="s" direction="in"/><arg name="value" type="v" direction="out"/></method>
    <method name="GetAll"><arg name="interface" type="s" direction="in"/><arg name="props" type="a{sv}" direction="out"/></method>
    <method name="Set"><arg name="interface" type="s" direction="in"/><arg name="name" type="s" direction="in"/><arg name="value" type="v" direction="in"/></method>
    <signal name="PropertiesChanged"><arg name="interface" type="s"/><arg name="changed" type="a{sv}"/><arg name="invalidated" type="as"/></signal>
  </interface>
  <interface name="org.mpris.MediaPlayer2">
    <method name="Raise"/>
    <method name="Quit"/>
    <property name="CanQuit" type="b" access="read"/>
    <property name="CanRaise" type="b" access="read"/>
    <property name="HasTrackList" type="b" access="read"/>
    <property name="Identity" type="s" access="read"/>
    <property name="SupportedUriSchemes" type="as" access="read"/>
    <property name="SupportedMimeTypes" type="as" access="read"/>
  </interface>
  <interface name="org.mpris.MediaPlayer2.Player">
    <method name="Next"/>
    <method name="Previous"/>
    <method name="Pause"/>
    <method name="PlayPause"/>
    <method name="Stop"/>
    <method name="Play"/>
    <method name="Seek"><arg name="Offset" type="x" direction="in"/></method>
    <method name="SetPosition"><arg name="TrackId" type="o" direction="in"/><arg name="Position" type="x" direction="in"/></method>
    <method name="OpenUri"><arg name="Uri" type="s" direction="in"/></method>
    <signal name="Seeked"><arg name="Position" type="x"/></signal>
    <property name="PlaybackStatus" type="s" access="read"/>
    <property name="LoopStatus" type="s" access="readwrite"/>
    <property name="Rate" type="d" access="readwrite"/>
    <property name="Shuffle" type="b" access="readwrite"/>
    <property name="Metadata" type="a{sv}" access="read"/>
    <property name="Volume" type="d" access="readwrite"/>
    <property name="Position" type="x" access="read"/>
    <property name="MinimumRate" type="d" access="read"/>
    <property name="MaximumRate" type="d" access="read"/>
    <property name="CanGoNext" type="b" access="read"/>
    <property name="CanGoPrevious" type="b" access="read"/>
    <property name="CanPlay" type="b" access="read"/>
    <property name="CanPause" type="b" access="read"/>
    <property name="CanSeek" type="b" access="read"/>
    <property name="CanControl" type="b" access="read"/>
  </interface>
</node>
"#;

// ── mappings ─────────────────────────────────────────────────────────────

pub fn playback_status(status: PlayStatus) -> &'static str {
    match status {
        PlayStatus::Playing => "Playing",
        PlayStatus::Paused => "Paused",
        PlayStatus::Stopped => "Stopped",
    }
}

pub fn loop_status(mode: RepeatMode) -> &'static str {
    match mode {
        RepeatMode::Off => "None",
        RepeatMode::One => "Track",
        RepeatMode::All => "Playlist",
    }
}

pub fn parse_loop_status(s: &str) -> Option<RepeatMode> {
    match s {
        "None" => Some(RepeatMode::Off),
        "Track" => Some(RepeatMode::One),
        "Playlist" => Some(RepeatMode::All),
        _ => None,
    }
}

/// MPRIS times are signed microseconds.
pub fn micros(d: Duration) -> i64 {
    i64::try_from(d.as_micros()).unwrap_or(i64::MAX)
}

pub fn from_micros(us: i64) -> Duration {
    Duration::from_micros(u64::try_from(us).unwrap_or(0))
}

/// Stable D-Bus object path for a track (FNV-1a of its queue id).
pub fn track_id(track: &Track) -> String {
    if track.is_empty() {
        return NO_TRACK.to_string();
    }
    let key = if track.id.is_empty() {
        &track.title
    } else {
        &track.id
    };
    let hash = key.bytes().fold(0xcbf2_9ce4_8422_2325_u64, |h, b| {
        (h ^ u64::from(b)).wrapping_mul(0x0000_0100_0000_01b3)
    });
    format!("{TRACK_PREFIX}{hash:016x}")
}

/// `file://` URI with RFC 3986 escaping.
pub fn file_uri(path: &Path) -> String {
    let mut uri = String::from("file://");
    for &b in path.as_os_str().as_bytes() {
        if b.is_ascii_alphanumeric() || b"/-._~".contains(&b) {
            uri.push(char::from(b));
        } else {
            uri.push_str(&format!("%{b:02X}"));
        }
    }
    uri
}

/// A cover image next to a local file, as a `file://` URI.
fn cover_art(track: &Track) -> Option<String> {
    let Some(Location::File(path)) = &track.location else {
        return None;
    };
    let dir = path.parent()?;
    const NAMES: &[&str] = &[
        "cover.jpg",
        "cover.png",
        "Cover.jpg",
        "Cover.png",
        "folder.jpg",
        "folder.png",
        "Folder.jpg",
        "front.jpg",
        "Front.jpg",
        "album.jpg",
    ];
    NAMES
        .iter()
        .map(|name| dir.join(name))
        .find(|p| p.is_file())
        .map(|p| file_uri(&p))
}

/// What `Get`/`GetAll` answer from; written by the app thread on change.
#[derive(Debug, Clone)]
pub struct Published {
    pub state: State,
    pub clock: Clock,
    /// Cover art URI for the current track.
    pub art: Option<String>,
}

pub fn metadata(p: &Published) -> Value {
    let t = &p.state.track;
    let mut entries = vec![("mpris:trackid", Value::Path(track_id(t)))];
    if t.is_empty() {
        return Value::dict(entries);
    }
    if let Some(len) = t.length {
        entries.push(("mpris:length", Value::I64(micros(len))));
    }
    entries.push(("xesam:title", Value::Str(t.title.clone())));
    if let Some(artist) = &t.artist {
        entries.push(("xesam:artist", Value::strings(vec![artist.clone()])));
    }
    if let Some(album) = &t.album {
        entries.push(("xesam:album", Value::Str(album.clone())));
    }
    match &t.location {
        Some(Location::File(path)) => entries.push(("xesam:url", Value::Str(file_uri(path)))),
        Some(Location::Url(url)) => entries.push(("xesam:url", Value::Str(url.clone()))),
        None => {}
    }
    if let Some(art) = &p.art {
        entries.push(("mpris:artUrl", Value::Str(art.clone())));
    }
    Value::dict(entries)
}

fn root_property(name: &str) -> Option<Value> {
    Some(match name {
        "CanQuit" => Value::Bool(true),
        "CanRaise" | "HasTrackList" => Value::Bool(false),
        "Identity" => Value::Str("rustune".into()),
        "SupportedUriSchemes" | "SupportedMimeTypes" => Value::strings(Vec::new()),
        _ => return None,
    })
}

fn player_property(name: &str, p: &Published, now: Instant) -> Option<Value> {
    let s = &p.state;
    Some(match name {
        "PlaybackStatus" => Value::Str(playback_status(s.status).into()),
        "LoopStatus" => Value::Str(loop_status(s.repeat).into()),
        "Rate" | "MinimumRate" | "MaximumRate" => Value::F64(1.0),
        "Shuffle" => Value::Bool(s.shuffle),
        "Metadata" => metadata(p),
        "Volume" => Value::F64(f64::from(s.volume)),
        "Position" => Value::I64(micros(p.clock.position(now))),
        "CanGoNext" => Value::Bool(s.can_next),
        "CanGoPrevious" => Value::Bool(s.can_prev),
        "CanPlay" | "CanPause" => Value::Bool(s.can_play),
        "CanSeek" => Value::Bool(s.can_seek),
        "CanControl" => Value::Bool(true),
        _ => return None,
    })
}

fn property(iface: &str, name: &str, p: &Published, now: Instant) -> Result<Value, &'static str> {
    match iface {
        ROOT_IFACE => root_property(name).ok_or("org.freedesktop.DBus.Error.UnknownProperty"),
        PLAYER_IFACE => {
            player_property(name, p, now).ok_or("org.freedesktop.DBus.Error.UnknownProperty")
        }
        _ => Err("org.freedesktop.DBus.Error.UnknownInterface"),
    }
}

fn all_properties(iface: &str, p: &Published, now: Instant) -> Option<Value> {
    let entries = match iface {
        ROOT_IFACE => ROOT_PROPS
            .iter()
            .filter_map(|&n| Some((n, root_property(n)?)))
            .collect(),
        PLAYER_IFACE => PLAYER_PROPS
            .iter()
            .filter_map(|&n| Some((n, player_property(n, p, now)?)))
            .collect(),
        _ => return None,
    };
    Some(Value::dict(entries))
}

/// The player properties that `changes` touched, for `PropertiesChanged`.
pub fn changed_properties(p: &Published, changes: &Changes) -> Vec<(&'static str, Value)> {
    let s = &p.state;
    let mut out = Vec::new();
    if changes.track {
        out.push(("Metadata", metadata(p)));
    }
    if changes.status {
        out.push((
            "PlaybackStatus",
            Value::Str(playback_status(s.status).into()),
        ));
    }
    if changes.volume {
        out.push(("Volume", Value::F64(f64::from(s.volume))));
    }
    if changes.shuffle {
        out.push(("Shuffle", Value::Bool(s.shuffle)));
    }
    if changes.repeat {
        out.push(("LoopStatus", Value::Str(loop_status(s.repeat).into())));
    }
    if changes.caps {
        out.push(("CanGoNext", Value::Bool(s.can_next)));
        out.push(("CanGoPrevious", Value::Bool(s.can_prev)));
        out.push(("CanPlay", Value::Bool(s.can_play)));
        out.push(("CanPause", Value::Bool(s.can_play)));
        out.push(("CanSeek", Value::Bool(s.can_seek)));
    }
    out
}

/// Signals announcing `changes`.
pub fn signals(p: &Published, changes: &Changes) -> Vec<Message> {
    let mut out = Vec::new();
    let props = changed_properties(p, changes);
    if !props.is_empty() {
        out.push(Message::signal(
            OBJECT_PATH,
            PROPS_IFACE,
            "PropertiesChanged",
            vec![
                Value::Str(PLAYER_IFACE.into()),
                Value::dict(props),
                Value::strings(Vec::new()),
            ],
        ));
    }
    if let Some(pos) = changes.seeked {
        out.push(Message::signal(
            OBJECT_PATH,
            PLAYER_IFACE,
            "Seeked",
            vec![Value::I64(micros(pos))],
        ));
    }
    out
}

// ── method calls ─────────────────────────────────────────────────────────

/// The reply to a method call and the player command it asks for.
#[derive(Debug, Default)]
pub struct Outcome {
    pub reply: Option<Message>,
    pub command: Option<Command>,
}

fn arg_str(msg: &Message, i: usize) -> Option<&str> {
    msg.body.get(i).and_then(Value::as_str)
}

/// Introspection for the parents of the object path (lets tools walk
/// the tree from `/`).
fn parent_node(path: &str) -> Option<&'static str> {
    Some(match path {
        "/" => "<node><node name=\"org\"/></node>",
        "/org" => "<node><node name=\"mpris\"/></node>",
        "/org/mpris" => "<node><node name=\"MediaPlayer2\"/></node>",
        _ => return None,
    })
}

fn machine_id() -> Option<String> {
    ["/etc/machine-id", "/var/lib/dbus/machine-id"]
        .iter()
        .find_map(|p| std::fs::read_to_string(p).ok())
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

fn seek_command(p: &Published, offset: i64, now: Instant) -> Option<Command> {
    if !p.state.can_seek {
        return None;
    }
    let target = micros(p.clock.position(now)).saturating_add(offset).max(0);
    match p.state.track.length {
        // Seeking past the end behaves like Next (MPRIS spec).
        Some(len) if target > micros(len) => Some(Command::Next),
        _ => Some(Command::SeekTo(from_micros(target))),
    }
}

fn set_property(msg: &Message, p: &Published) -> Result<Option<Command>, &'static str> {
    const INVALID: &str = "org.freedesktop.DBus.Error.InvalidArgs";
    let (Some(iface), Some(name), Some(value)) =
        (arg_str(msg, 0), arg_str(msg, 1), msg.body.get(2))
    else {
        return Err(INVALID);
    };
    let value = value.inner();
    if iface != PLAYER_IFACE {
        return Err(match property(iface, name, p, Instant::now()) {
            Ok(_) => "org.freedesktop.DBus.Error.PropertyReadOnly",
            Err(e) => e,
        });
    }
    match (name, value) {
        ("Volume", Value::F64(v)) if v.is_finite() => {
            Ok(Some(Command::SetVolume(v.clamp(0.0, 1.0) as f32)))
        }
        ("Shuffle", Value::Bool(on)) => {
            Ok((*on != p.state.shuffle).then_some(Command::ToggleShuffle))
        }
        ("LoopStatus", Value::Str(s)) => parse_loop_status(s)
            .map(|mode| Some(Command::SetRepeat(mode)))
            .ok_or(INVALID),
        // Only 1.0 is supported; 0.0 means pause (MPRIS spec).
        ("Rate", Value::F64(r)) => Ok((*r == 0.0).then_some(Command::Pause)),
        ("Volume" | "Shuffle" | "LoopStatus" | "Rate", _) => Err(INVALID),
        _ if PLAYER_PROPS.contains(&name) => Err("org.freedesktop.DBus.Error.PropertyReadOnly"),
        _ => Err("org.freedesktop.DBus.Error.UnknownProperty"),
    }
}

/// Answer one method call from the published state.
pub fn handle(msg: &Message, p: &Published, now: Instant) -> Outcome {
    let mut out = Outcome::default();
    if msg.kind != METHOD_CALL {
        return out;
    }
    let ok = |body| Ok(Message::method_return(msg, body));
    let path = msg.path.as_deref().unwrap_or("");
    let member = msg.member.as_deref().unwrap_or("");
    let iface = msg.interface.as_deref();
    let reply: Result<Message, &str> = if path != OBJECT_PATH {
        match (iface, member, parent_node(path)) {
            (Some(INTROSPECT_IFACE) | None, "Introspect", Some(xml)) => {
                ok(vec![Value::Str(xml.into())])
            }
            _ => Err("org.freedesktop.DBus.Error.UnknownObject"),
        }
    } else {
        match (iface, member) {
            (Some(INTROSPECT_IFACE) | None, "Introspect") => {
                ok(vec![Value::Str(INTROSPECTION.into())])
            }
            (Some(PEER_IFACE) | None, "Ping") => ok(Vec::new()),
            (Some(PEER_IFACE) | None, "GetMachineId") => match machine_id() {
                Some(id) => ok(vec![Value::Str(id)]),
                None => Err("org.freedesktop.DBus.Error.FileNotFound"),
            },
            (Some(PROPS_IFACE) | None, "Get") => match (arg_str(msg, 0), arg_str(msg, 1)) {
                (Some(i), Some(n)) => property(i, n, p, now)
                    .map(|v| Message::method_return(msg, vec![Value::variant(v)])),
                _ => Err("org.freedesktop.DBus.Error.InvalidArgs"),
            },
            (Some(PROPS_IFACE) | None, "GetAll") => match arg_str(msg, 0) {
                Some(i) => match all_properties(i, p, now) {
                    Some(all) => ok(vec![all]),
                    // Unknown interfaces have no properties.
                    None => ok(vec![Value::dict(Vec::new())]),
                },
                None => Err("org.freedesktop.DBus.Error.InvalidArgs"),
            },
            (Some(PROPS_IFACE) | None, "Set") => set_property(msg, p).map(|cmd| {
                out.command = cmd;
                Message::method_return(msg, Vec::new())
            }),
            (Some(ROOT_IFACE) | None, "Raise") => ok(Vec::new()),
            (Some(ROOT_IFACE) | None, "Quit") => {
                out.command = Some(Command::Quit);
                ok(Vec::new())
            }
            (Some(PLAYER_IFACE) | None, name) => {
                let command = match name {
                    "Next" => Some(Some(Command::Next)),
                    "Previous" => Some(Some(Command::Prev)),
                    "Pause" => Some(Some(Command::Pause)),
                    "PlayPause" => Some(Some(Command::TogglePause)),
                    "Stop" => Some(Some(Command::Stop)),
                    // `Command::Play` restarts a playing track; MPRIS Play doesn't.
                    "Play" => {
                        Some((p.state.status != PlayStatus::Playing).then_some(Command::Play))
                    }
                    "Seek" => match msg.body.first() {
                        Some(Value::I64(offset)) => Some(seek_command(p, *offset, now)),
                        _ => None,
                    },
                    "SetPosition" => match (arg_str(msg, 0), msg.body.get(1)) {
                        (Some(id), Some(Value::I64(pos))) => {
                            let len = p.state.track.length.map_or(i64::MAX, micros);
                            let valid = p.state.can_seek
                                && id == track_id(&p.state.track)
                                && (0..=len).contains(pos);
                            Some(valid.then(|| Command::SeekTo(from_micros(*pos))))
                        }
                        _ => None,
                    },
                    _ => None,
                };
                match (name, command) {
                    (_, Some(cmd)) => {
                        out.command = cmd;
                        ok(Vec::new())
                    }
                    ("OpenUri", None) => Err("org.freedesktop.DBus.Error.NotSupported"),
                    ("Seek" | "SetPosition", None) => Err("org.freedesktop.DBus.Error.InvalidArgs"),
                    _ => Err("org.freedesktop.DBus.Error.UnknownMethod"),
                }
            }
            _ => Err("org.freedesktop.DBus.Error.UnknownMethod"),
        }
    };
    if msg.flags & NO_REPLY_EXPECTED == 0 {
        out.reply = Some(reply.unwrap_or_else(|name| Message::error(msg, name, member)));
    }
    out
}

// ── service ──────────────────────────────────────────────────────────────

struct Shared {
    link: AtomicU8,
    published: Mutex<Published>,
    /// Kept to shut the socket down when the player goes away.
    stream: Mutex<Option<UnixStream>>,
}

impl Shared {
    fn published(&self) -> MutexGuard<'_, Published> {
        self.published
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
    }
}

pub struct Mpris {
    shared: Arc<Shared>,
    out: Sender<Message>,
}

impl Mpris {
    /// Start the service thread; it connects in the background and the
    /// service just stays off when there is no session bus.
    pub fn spawn(tx: UnboundedSender<AppEvent>) -> io::Result<Self> {
        let shared = Arc::new(Shared {
            link: AtomicU8::new(CONNECTING),
            published: Mutex::new(Published {
                state: State::default(),
                clock: Clock::new(Instant::now()),
                art: None,
            }),
            stream: Mutex::new(None),
        });
        let (out, out_rx) = mpsc::channel();
        let thread_shared = Arc::clone(&shared);
        let thread_out = out.clone();
        std::thread::Builder::new()
            .name("rustune-mpris".into())
            .spawn(move || {
                // Failures leave media controls off; stderr belongs to the TUI.
                let _ = serve(&thread_shared, thread_out, out_rx, &tx);
                thread_shared.link.store(DOWN, Ordering::Release);
            })?;
        Ok(Self { shared, out })
    }

    pub fn publish(&self, state: &State, clock: &Clock, changes: &Changes) {
        let art = changes.track.then(|| cover_art(&state.track));
        let signals = {
            let mut p = self.shared.published();
            p.state.clone_from(state);
            p.clock = *clock;
            if let Some(art) = art {
                p.art = art;
            }
            if self.shared.link.load(Ordering::Acquire) != UP {
                return;
            }
            signals(&p, changes)
        };
        for signal in signals {
            let _ = self.out.send(signal);
        }
    }
}

impl Drop for Mpris {
    fn drop(&mut self) {
        self.shared.link.store(DOWN, Ordering::Release);
        let stream = self
            .shared
            .stream
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .take();
        if let Some(stream) = stream {
            let _ = stream.shutdown(Shutdown::Both);
        }
    }
}

/// Own `BUS_NAME`, or a per-process instance name when another rustune
/// already has it (MPRIS convention).
fn acquire_name(bus: &mut Bus) -> io::Result<()> {
    if bus.request_name(BUS_NAME)? {
        return Ok(());
    }
    let instance = format!("{BUS_NAME}.instance{}", std::process::id());
    if bus.request_name(&instance)? {
        return Ok(());
    }
    Err(io::Error::other(format!("cannot own {BUS_NAME}")))
}

fn serve(
    shared: &Shared,
    out: Sender<Message>,
    out_rx: Receiver<Message>,
    tx: &UnboundedSender<AppEvent>,
) -> io::Result<()> {
    let mut bus = Bus::connect_session(SETUP_TIMEOUT)?;
    acquire_name(&mut bus)?;
    let (stream, mut serial) = bus.into_parts()?;
    let mut reader = stream.try_clone()?;
    *shared.stream.lock().unwrap_or_else(PoisonError::into_inner) = Some(stream.try_clone()?);
    if shared
        .link
        .compare_exchange(CONNECTING, UP, Ordering::AcqRel, Ordering::Acquire)
        .is_err()
    {
        return Ok(()); // the player already went away
    }
    std::thread::Builder::new()
        .name("rustune-mpris-tx".into())
        .spawn(move || {
            let mut stream = stream;
            for msg in out_rx {
                serial = serial.wrapping_add(1).max(1);
                if io::Write::write_all(&mut stream, &msg.encode(serial)).is_err() {
                    break;
                }
            }
        })?;
    loop {
        let raw = dbus::read_message(&mut reader)?;
        // A message we can't parse is skipped; framing stays intact.
        let Ok(msg) = Message::decode(&raw) else {
            continue;
        };
        let outcome = handle(&msg, &shared.published(), Instant::now());
        if let Some(cmd) = outcome.command {
            if tx.send(AppEvent::Command(cmd)).is_err() {
                return Ok(());
            }
        }
        if let Some(reply) = outcome.reply {
            if out.send(reply).is_err() {
                return Ok(());
            }
        }
    }
}

/// One line for `rustune doctor`.
pub fn doctor() -> String {
    match Bus::connect_session(Duration::from_secs(2)) {
        Ok(_) => format!("[ok] media controls: MPRIS ({BUS_NAME} on the D-Bus session bus)"),
        Err(e) if e.kind() == io::ErrorKind::NotFound => {
            "[--] media controls: no D-Bus session bus (MPRIS off)".to_string()
        }
        Err(e) => format!("[!!] media controls: D-Bus session bus unusable: {e}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mediactl::track::tests::{item, view};
    use crate::mediactl::track::Tracker;

    fn published(status: PlayStatus, elapsed: Duration) -> (Published, Instant) {
        let now = Instant::now();
        let items = [
            item("/music/a b.flac", "Song", Some("Band")),
            item("/music/c.flac", "Other", None),
        ];
        let mut tracker = Tracker::new(now);
        tracker.observe(&view(&items, Some(0), "Band - Song", status, elapsed), now);
        let p = Published {
            state: tracker.state.clone(),
            clock: tracker.clock,
            art: None,
        };
        (p, now)
    }

    fn call(iface: &str, member: &str, body: Vec<Value>) -> Message {
        let mut m = Message::method_call(BUS_NAME, OBJECT_PATH, iface, member, body);
        m.serial = 42;
        m.sender = Some(":1.7".into());
        m
    }

    fn entry<'a>(dict: &'a Value, key: &str) -> Option<&'a Value> {
        let Value::Array(_, entries) = dict else {
            return None;
        };
        entries.iter().find_map(|e| match e {
            Value::DictEntry(k, v) if k.as_str() == Some(key) => Some(v.inner()),
            _ => None,
        })
    }

    #[test]
    fn mappings() {
        for mode in [RepeatMode::Off, RepeatMode::All, RepeatMode::One] {
            assert_eq!(parse_loop_status(loop_status(mode)), Some(mode));
        }
        assert_eq!(loop_status(RepeatMode::One), "Track");
        assert_eq!(loop_status(RepeatMode::All), "Playlist");
        assert_eq!(parse_loop_status("bogus"), None);
        assert_eq!(playback_status(PlayStatus::Paused), "Paused");
        assert_eq!(micros(Duration::from_millis(1500)), 1_500_000);
        assert_eq!(from_micros(-5), Duration::ZERO);
        assert_eq!(from_micros(2_000_001), Duration::from_micros(2_000_001));
        assert_eq!(micros(Duration::MAX), i64::MAX);
    }

    #[test]
    fn track_ids_are_valid_stable_object_paths() {
        let (p, _) = published(PlayStatus::Playing, Duration::ZERO);
        let id = track_id(&p.state.track);
        assert_eq!(id, track_id(&p.state.track));
        let tail = id.strip_prefix(TRACK_PREFIX).expect("prefix");
        assert_eq!(tail.len(), 16);
        assert!(tail.bytes().all(|b| b.is_ascii_hexdigit()));
        assert_eq!(track_id(&Track::default()), NO_TRACK);
        let other = Track {
            id: "/music/c.flac".into(),
            ..Track::default()
        };
        assert_ne!(track_id(&other), id);
    }

    #[test]
    fn metadata_from_player_view() {
        let (p, _) = published(PlayStatus::Playing, Duration::ZERO);
        let md = metadata(&p);
        assert_eq!(entry(&md, "xesam:title"), Some(&Value::Str("Song".into())));
        assert_eq!(
            entry(&md, "xesam:artist"),
            Some(&Value::strings(vec!["Band".into()]))
        );
        assert_eq!(entry(&md, "xesam:album"), Some(&Value::Str("Album".into())));
        assert_eq!(entry(&md, "mpris:length"), Some(&Value::I64(200_000_000)));
        assert_eq!(
            entry(&md, "xesam:url"),
            Some(&Value::Str("file:///music/a%20b.flac".into()))
        );
        assert_eq!(
            entry(&md, "mpris:trackid"),
            Some(&Value::Path(track_id(&p.state.track)))
        );
        let empty = Published {
            state: State::default(),
            ..p
        };
        let md = metadata(&empty);
        assert_eq!(
            entry(&md, "mpris:trackid"),
            Some(&Value::Path(NO_TRACK.into()))
        );
        assert_eq!(entry(&md, "xesam:title"), None);
    }

    #[test]
    fn get_and_get_all() {
        let (p, now) = published(PlayStatus::Playing, Duration::from_secs(10));
        let get = |name: &str| {
            let m = call(
                PROPS_IFACE,
                "Get",
                vec![Value::Str(PLAYER_IFACE.into()), Value::Str(name.into())],
            );
            let reply = handle(&m, &p, now + Duration::from_secs(2))
                .reply
                .expect("reply");
            assert_eq!(reply.kind, dbus::METHOD_RETURN);
            assert_eq!(reply.reply_serial, Some(42));
            assert_eq!(reply.destination.as_deref(), Some(":1.7"));
            reply.body[0].inner().clone()
        };
        assert_eq!(get("PlaybackStatus"), Value::Str("Playing".into()));
        assert_eq!(get("Position"), Value::I64(12_000_000));
        assert_eq!(get("Volume"), Value::F64(0.5));
        assert_eq!(get("LoopStatus"), Value::Str("None".into()));
        assert_eq!(get("CanSeek"), Value::Bool(true));

        let m = call(PROPS_IFACE, "GetAll", vec![Value::Str(PLAYER_IFACE.into())]);
        let reply = handle(&m, &p, now).reply.expect("reply");
        for name in PLAYER_PROPS {
            assert!(entry(&reply.body[0], name).is_some(), "{name}");
        }
        let m = call(
            PROPS_IFACE,
            "Get",
            vec![Value::Str(ROOT_IFACE.into()), Value::Str("Identity".into())],
        );
        let reply = handle(&m, &p, now).reply.expect("reply");
        assert_eq!(reply.body[0].inner(), &Value::Str("rustune".into()));

        let m = call(
            PROPS_IFACE,
            "Get",
            vec![Value::Str(PLAYER_IFACE.into()), Value::Str("Nope".into())],
        );
        let reply = handle(&m, &p, now).reply.expect("reply");
        assert_eq!(reply.kind, dbus::ERROR);
        assert_eq!(
            reply.error_name.as_deref(),
            Some("org.freedesktop.DBus.Error.UnknownProperty")
        );
    }

    #[test]
    fn transport_methods_map_to_commands() {
        let (p, now) = published(PlayStatus::Playing, Duration::from_secs(10));
        let cmd = |member: &str, body: Vec<Value>| {
            handle(&call(PLAYER_IFACE, member, body), &p, now).command
        };
        assert_eq!(cmd("PlayPause", vec![]), Some(Command::TogglePause));
        assert_eq!(cmd("Pause", vec![]), Some(Command::Pause));
        assert_eq!(cmd("Stop", vec![]), Some(Command::Stop));
        assert_eq!(cmd("Next", vec![]), Some(Command::Next));
        assert_eq!(cmd("Previous", vec![]), Some(Command::Prev));
        assert_eq!(cmd("Play", vec![]), None, "already playing");
        assert_eq!(
            cmd("Seek", vec![Value::I64(5_000_000)]),
            Some(Command::SeekTo(Duration::from_secs(15)))
        );
        assert_eq!(
            cmd("Seek", vec![Value::I64(-60_000_000)]),
            Some(Command::SeekTo(Duration::ZERO))
        );
        assert_eq!(
            cmd("Seek", vec![Value::I64(500_000_000)]),
            Some(Command::Next)
        );
        let id = track_id(&p.state.track);
        assert_eq!(
            cmd(
                "SetPosition",
                vec![Value::Path(id.clone()), Value::I64(30_000_000)]
            ),
            Some(Command::SeekTo(Duration::from_secs(30)))
        );
        assert_eq!(
            cmd(
                "SetPosition",
                vec![Value::Path(NO_TRACK.into()), Value::I64(1)]
            ),
            None
        );
        assert_eq!(
            cmd("SetPosition", vec![Value::Path(id), Value::I64(-1)]),
            None
        );
        assert_eq!(
            handle(&call(ROOT_IFACE, "Quit", vec![]), &p, now).command,
            Some(Command::Quit)
        );

        let (paused, now) = published(PlayStatus::Paused, Duration::ZERO);
        let m = call(PLAYER_IFACE, "Play", vec![]);
        assert_eq!(handle(&m, &paused, now).command, Some(Command::Play));

        let m = call(
            PLAYER_IFACE,
            "OpenUri",
            vec![Value::Str("file:///x".into())],
        );
        let reply = handle(&m, &p, now).reply.expect("reply");
        assert_eq!(
            reply.error_name.as_deref(),
            Some("org.freedesktop.DBus.Error.NotSupported")
        );
    }

    #[test]
    fn set_properties() {
        let (p, now) = published(PlayStatus::Playing, Duration::ZERO);
        let set = |name: &str, v: Value| {
            let m = call(
                PROPS_IFACE,
                "Set",
                vec![
                    Value::Str(PLAYER_IFACE.into()),
                    Value::Str(name.into()),
                    Value::variant(v),
                ],
            );
            handle(&m, &p, now)
        };
        assert_eq!(
            set("Volume", Value::F64(0.25)).command,
            Some(Command::SetVolume(0.25))
        );
        assert_eq!(
            set("Volume", Value::F64(7.0)).command,
            Some(Command::SetVolume(1.0))
        );
        assert_eq!(
            set("Shuffle", Value::Bool(true)).command,
            Some(Command::ToggleShuffle)
        );
        assert_eq!(
            set("Shuffle", Value::Bool(false)).command,
            None,
            "already off"
        );
        assert_eq!(
            set("LoopStatus", Value::Str("Playlist".into())).command,
            Some(Command::SetRepeat(RepeatMode::All))
        );
        let bad = set("LoopStatus", Value::Str("Sometimes".into()))
            .reply
            .expect("reply");
        assert_eq!(
            bad.error_name.as_deref(),
            Some("org.freedesktop.DBus.Error.InvalidArgs")
        );
        let ro = set("PlaybackStatus", Value::Str("Paused".into()))
            .reply
            .expect("reply");
        assert_eq!(
            ro.error_name.as_deref(),
            Some("org.freedesktop.DBus.Error.PropertyReadOnly")
        );
    }

    #[test]
    fn no_reply_when_not_expected() {
        let (p, now) = published(PlayStatus::Playing, Duration::ZERO);
        let mut m = call(PLAYER_IFACE, "Next", vec![]);
        m.flags = NO_REPLY_EXPECTED;
        let out = handle(&m, &p, now);
        assert!(out.reply.is_none());
        assert_eq!(out.command, Some(Command::Next));
    }

    #[test]
    fn introspection_and_unknown_paths() {
        let (p, now) = published(PlayStatus::Playing, Duration::ZERO);
        let m = call(INTROSPECT_IFACE, "Introspect", vec![]);
        let reply = handle(&m, &p, now).reply.expect("reply");
        let Some(Value::Str(xml)) = reply.body.first() else {
            panic!("xml");
        };
        assert!(xml.contains("org.mpris.MediaPlayer2.Player"));
        let mut root = call(INTROSPECT_IFACE, "Introspect", vec![]);
        root.path = Some("/".into());
        let reply = handle(&root, &p, now).reply.expect("reply");
        assert_eq!(reply.kind, dbus::METHOD_RETURN);
        let mut other = call(PLAYER_IFACE, "Next", vec![]);
        other.path = Some("/elsewhere".into());
        let out = handle(&other, &p, now);
        assert_eq!(out.command, None);
        assert_eq!(
            out.reply.and_then(|r| r.error_name),
            Some("org.freedesktop.DBus.Error.UnknownObject".into())
        );
    }

    #[test]
    fn signals_follow_changes() {
        let (p, _) = published(PlayStatus::Playing, Duration::ZERO);
        let none = signals(&p, &Changes::default());
        assert!(none.is_empty());
        let changes = Changes {
            status: true,
            volume: true,
            seeked: Some(Duration::from_secs(3)),
            ..Changes::default()
        };
        let sigs = signals(&p, &changes);
        assert_eq!(sigs.len(), 2);
        assert_eq!(sigs[0].member.as_deref(), Some("PropertiesChanged"));
        assert_eq!(sigs[0].signature(), "sa{sv}as");
        assert_eq!(
            entry(&sigs[0].body[1], "PlaybackStatus"),
            Some(&Value::Str("Playing".into()))
        );
        assert_eq!(entry(&sigs[0].body[1], "Volume"), Some(&Value::F64(0.5)));
        assert_eq!(entry(&sigs[0].body[1], "Metadata"), None);
        assert_eq!(sigs[1].member.as_deref(), Some("Seeked"));
        assert_eq!(sigs[1].body, vec![Value::I64(3_000_000)]);
        // Encodable as a real message.
        assert!(Message::decode(&sigs[0].encode(1)).is_ok());
    }

    #[test]
    fn file_uris_escape() {
        assert_eq!(
            file_uri(Path::new("/a/b c/ü.mp3")),
            "file:///a/b%20c/%C3%BC.mp3"
        );
    }
}
