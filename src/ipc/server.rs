//! The control server: an accept thread plus one thread per client. It
//! never touches `App`: commands go into the app event channel, status
//! comes from a snapshot the controller publishes through an observer.

use std::io::{self, BufRead, BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use tokio::sync::mpsc::UnboundedSender;

use super::protocol::{error_line, Request, Status, OK};
use super::transport::{self, Conn, Listener, Remover};
use crate::app::App;
use crate::event::AppEvent;

/// Longest accepted request line (a long `enqueue` list fits).
const MAX_LINE: u64 = 1 << 20;
/// How long a command reply waits for the player to apply it.
const APPLY_TIMEOUT: Duration = Duration::from_secs(2);

#[derive(Default)]
struct Published {
    /// Bumped on every change; subscribers wait for it to move.
    version: u64,
    status: Status,
    closed: bool,
}

#[derive(Default)]
struct Shared {
    state: Mutex<Published>,
    changed: Condvar,
}

impl Shared {
    fn lock(&self) -> MutexGuard<'_, Published> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn publish(&self, status: &Status) {
        let mut state = self.lock();
        state.status.clone_from(status);
        state.version += 1;
        self.changed.notify_all();
    }

    fn status_line(&self) -> String {
        serde_json::to_string(&self.lock().status).unwrap_or_default()
    }
}

pub struct IpcServer {
    endpoint: PathBuf,
    shared: Arc<Shared>,
    stop: Arc<AtomicBool>,
    remover: Remover,
}

impl IpcServer {
    /// Bind `endpoint` and start serving; commands are sent on `tx`.
    pub fn start(endpoint: PathBuf, tx: UnboundedSender<AppEvent>) -> io::Result<Self> {
        let listener = Listener::bind(&endpoint)?;
        let remover = listener.remover();
        let shared = Arc::new(Shared::default());
        let stop = Arc::new(AtomicBool::new(false));
        let (accept_shared, accept_stop) = (shared.clone(), stop.clone());
        std::thread::Builder::new()
            .name("rustune-ipc".into())
            .spawn(move || accept_loop(listener, &tx, &accept_shared, &accept_stop))?;
        Ok(Self {
            endpoint,
            shared,
            stop,
            remover,
        })
    }

    pub fn endpoint(&self) -> &Path {
        &self.endpoint
    }

    /// For `Controller::observers`: publishes the status when it changed.
    pub fn observer(&self) -> Box<dyn FnMut(&App) + Send> {
        let shared = self.shared.clone();
        let mut last = Status::default();
        Box::new(move |app: &App| {
            if last.update(app) {
                shared.publish(&last);
            }
        })
    }
}

impl Drop for IpcServer {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        {
            let mut state = self.shared.lock();
            state.closed = true;
            self.shared.changed.notify_all();
        }
        // Wake the accept thread so it sees `stop` and exits.
        let _ = transport::connect(&self.endpoint);
        self.remover.remove();
    }
}

fn accept_loop(
    mut listener: Listener,
    tx: &UnboundedSender<AppEvent>,
    shared: &Arc<Shared>,
    stop: &AtomicBool,
) {
    loop {
        let conn = listener.accept();
        if stop.load(Ordering::Acquire) {
            return;
        }
        let Ok(conn) = conn else {
            // Transient (e.g. too many open files): don't spin.
            std::thread::sleep(Duration::from_millis(50));
            continue;
        };
        let (tx, shared) = (tx.clone(), shared.clone());
        let _ = std::thread::Builder::new()
            .name("rustune-ipc-client".into())
            .spawn(move || serve(conn, &tx, &shared));
    }
}

fn write_line(conn: &mut Conn, line: &str) -> io::Result<()> {
    let mut buf = Vec::with_capacity(line.len() + 1);
    buf.extend_from_slice(line.as_bytes());
    buf.push(b'\n');
    conn.write_all(&buf)?;
    conn.flush()
}

/// Answer requests from one client until it hangs up or subscribes.
fn serve(conn: Conn, tx: &UnboundedSender<AppEvent>, shared: &Shared) {
    let mut reader = BufReader::new(conn);
    let mut line = String::new();
    loop {
        line.clear();
        match reader.by_ref().take(MAX_LINE).read_line(&mut line) {
            Ok(0) | Err(_) => return,
            Ok(_) => {}
        }
        if !line.ends_with('\n') && line.len() as u64 >= MAX_LINE {
            let _ = write_line(reader.get_mut(), &error_line("request too long"));
            return;
        }
        let request = line.trim();
        if request.is_empty() {
            continue;
        }
        let reply = match Request::parse(request) {
            Ok(Request::Command(cmd)) => apply(tx, cmd),
            Ok(Request::Status) => shared.status_line(),
            Ok(Request::Subscribe) => {
                subscribe(reader.get_mut(), shared);
                return;
            }
            Err(e) => error_line(&e),
        };
        if write_line(reader.get_mut(), &reply).is_err() {
            return;
        }
    }
}

/// Queue `cmd` for the controller and wait (bounded) until it is applied,
/// so the client's next status read already reflects it.
fn apply(tx: &UnboundedSender<AppEvent>, cmd: crate::command::Command) -> String {
    let (done, applied) = std::sync::mpsc::sync_channel(1);
    if tx.send(AppEvent::Command(cmd)).is_err() || tx.send(AppEvent::Flush(done)).is_err() {
        return error_line("rustune is shutting down");
    }
    // A busy player (e.g. a dependency installer in the foreground) still
    // applies the command later; the reply just doesn't wait for it.
    let _ = applied.recv_timeout(APPLY_TIMEOUT);
    OK.to_string()
}

/// Stream the status now and after every change until the client leaves
/// or the server stops.
fn subscribe(conn: &mut Conn, shared: &Shared) {
    // Version 0 is the empty placeholder: wait for the first publish.
    let mut seen = Some(0);
    loop {
        let line = {
            let mut state = shared.lock();
            while !state.closed && seen == Some(state.version) {
                state = shared
                    .changed
                    .wait(state)
                    .unwrap_or_else(PoisonError::into_inner);
            }
            if state.closed {
                return;
            }
            seen = Some(state.version);
            serde_json::to_string(&state.status).unwrap_or_default()
        };
        if write_line(conn, &line).is_err() {
            return;
        }
    }
}

#[cfg(all(test, unix))]
mod tests {
    use std::os::unix::net::{UnixListener, UnixStream};

    use super::*;
    use crate::command::Command;
    use crate::config::Config;
    use crate::ipc::protocol::check_reply;
    use crate::view::RepeatMode;

    fn temp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("rt-ipc-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn request(conn: &mut BufReader<UnixStream>, req: &str) -> String {
        conn.get_mut()
            .write_all(format!("{req}\n").as_bytes())
            .unwrap();
        let mut line = String::new();
        conn.read_line(&mut line).unwrap();
        line
    }

    /// Plays the controller's part: applies commands to an `App` and
    /// answers flushes after notifying the observer.
    fn fake_controller(
        mut rx: tokio::sync::mpsc::UnboundedReceiver<AppEvent>,
        mut observer: Box<dyn FnMut(&App) + Send>,
    ) -> std::thread::JoinHandle<Vec<Command>> {
        std::thread::spawn(move || {
            let mut app = App::new(Config::default());
            let (mut seen, mut quit) = (Vec::new(), false);
            observer(&app);
            while let Some(event) = rx.blocking_recv() {
                match event {
                    AppEvent::Command(Command::SetVolume(v)) => {
                        app.volume = v;
                        seen.push(Command::SetVolume(v));
                    }
                    AppEvent::Command(Command::SetRepeat(r)) => {
                        app.queue.set_repeat(r);
                        seen.push(Command::SetRepeat(r));
                    }
                    AppEvent::Command(Command::Quit) => quit = true,
                    AppEvent::Command(cmd) => seen.push(cmd),
                    AppEvent::Flush(done) => {
                        observer(&app);
                        let _ = done.send(());
                        if quit {
                            return seen;
                        }
                    }
                    _ => {}
                }
            }
            seen
        })
    }

    #[test]
    fn socket_round_trip() {
        let dir = temp_dir("rt");
        let path = dir.join("rustune.sock");
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
        let server = IpcServer::start(path.clone(), tx).unwrap();
        let mode = std::os::unix::fs::PermissionsExt::mode(
            &std::fs::metadata(&path).unwrap().permissions(),
        );
        assert_eq!(mode & 0o777, 0o600);
        let controller = fake_controller(rx, server.observer());

        let mut conn = BufReader::new(UnixStream::connect(&path).unwrap());
        let mut watch = BufReader::new(UnixStream::connect(&path).unwrap());
        watch
            .get_mut()
            .write_all(b"{\"subscribe\":true}\n")
            .unwrap();
        let mut first = String::new();
        watch.read_line(&mut first).unwrap();
        let first: Status = serde_json::from_str(&first).unwrap();
        assert_eq!(first.volume, 100);

        let reply = request(&mut conn, r#"{"cmd":"set_volume","arg":0.4}"#);
        assert_eq!(reply.trim(), OK);
        // The reply waits for the flush: status already shows the change.
        let status: Status = serde_json::from_value(
            check_reply(&request(&mut conn, r#"{"get":"status"}"#)).unwrap(),
        )
        .unwrap();
        assert_eq!(status.volume, 40);

        let mut event = String::new();
        watch.read_line(&mut event).unwrap();
        assert_eq!(serde_json::from_str::<Status>(&event).unwrap().volume, 40);

        let repeat = Request::Command(Command::SetRepeat(RepeatMode::One)).encode();
        let reply = request(&mut conn, &repeat);
        assert_eq!(reply.trim(), OK);
        let reply = request(&mut conn, r#"{"cmd":"bogus"}"#);
        assert!(check_reply(&reply).unwrap_err().contains("bad command"));
        let reply = request(&mut conn, r#"{"cmd":"quit"}"#);
        assert_eq!(reply.trim(), OK);

        let seen = controller.join().unwrap();
        assert_eq!(
            seen,
            vec![Command::SetVolume(0.4), Command::SetRepeat(RepeatMode::One)]
        );
        drop(server);
        assert!(!path.exists(), "socket removed on shutdown");
        // Subscribers are told the server is gone.
        let mut rest = String::new();
        loop {
            rest.clear();
            if watch.read_line(&mut rest).unwrap_or(0) == 0 {
                break;
            }
        }
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn stale_socket_is_replaced_but_live_one_is_not() {
        let dir = temp_dir("stale");
        let path = dir.join("rustune.sock");
        // A socket file nobody listens on: left behind by a crash.
        drop(UnixListener::bind(&path).unwrap());
        assert!(path.exists());
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
        let server = IpcServer::start(path.clone(), tx.clone()).unwrap();
        assert!(UnixStream::connect(&path).is_ok());

        let err = IpcServer::start(path.clone(), tx.clone()).err().unwrap();
        assert_eq!(err.kind(), io::ErrorKind::AddrInUse);
        assert!(path.exists(), "the live socket is kept");
        drop(server);

        // A regular file is never deleted.
        std::fs::write(&path, b"not a socket").unwrap();
        let err = IpcServer::start(path.clone(), tx).err().unwrap();
        assert_eq!(err.kind(), io::ErrorKind::AlreadyExists);
        assert_eq!(std::fs::read(&path).unwrap(), b"not a socket");
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
