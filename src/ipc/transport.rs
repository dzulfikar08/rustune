//! Platform endpoints: a Unix domain socket (mode 0600) or a Windows named
//! pipe, both std-only apart from the Win32 pipe calls.

use std::io;
use std::path::PathBuf;

/// The default endpoint. `RUSTUNE_SOCKET` overrides it (tests, several
/// instances).
pub fn endpoint() -> PathBuf {
    if let Some(path) = std::env::var_os("RUSTUNE_SOCKET").filter(|p| !p.is_empty()) {
        return PathBuf::from(path);
    }
    default_endpoint()
}

fn already_running() -> io::Error {
    io::Error::new(
        io::ErrorKind::AddrInUse,
        "another rustune instance is running",
    )
}

#[cfg(unix)]
pub use unix::*;
#[cfg(windows)]
pub use windows::*;

#[cfg(unix)]
mod unix {
    use std::fs;
    use std::io;
    use std::os::unix::fs::{DirBuilderExt, FileTypeExt, MetadataExt, PermissionsExt};
    use std::os::unix::net::{UnixListener, UnixStream};
    use std::path::{Path, PathBuf};

    pub type Conn = UnixStream;

    /// `$XDG_RUNTIME_DIR/rustune.sock`, else the config directory.
    pub fn default_endpoint() -> PathBuf {
        std::env::var_os("XDG_RUNTIME_DIR")
            .filter(|d| !d.is_empty())
            .map(PathBuf::from)
            .unwrap_or_else(crate::config::config_dir)
            .join("rustune.sock")
    }

    pub struct Listener {
        inner: UnixListener,
        path: PathBuf,
        /// Inode of our socket file: only that file is removed on drop.
        ino: u64,
    }

    impl Listener {
        /// Bind `path`. A live socket there means another instance runs
        /// (`AddrInUse`); a dead one is stale and replaced. Anything that
        /// is not a socket is left alone.
        pub fn bind(path: &Path) -> io::Result<Self> {
            if let Some(dir) = path.parent().filter(|d| !d.as_os_str().is_empty()) {
                fs::DirBuilder::new()
                    .recursive(true)
                    .mode(0o700)
                    .create(dir)?;
            }
            let inner = match UnixListener::bind(path) {
                Ok(listener) => listener,
                Err(e) if e.kind() == io::ErrorKind::AddrInUse => {
                    if UnixStream::connect(path).is_ok() {
                        return Err(super::already_running());
                    }
                    if !fs::symlink_metadata(path)?.file_type().is_socket() {
                        return Err(io::Error::new(
                            io::ErrorKind::AlreadyExists,
                            format!("{} exists and is not a socket", path.display()),
                        ));
                    }
                    fs::remove_file(path)?;
                    UnixListener::bind(path)?
                }
                Err(e) => return Err(e),
            };
            fs::set_permissions(path, fs::Permissions::from_mode(0o600))?;
            let ino = fs::metadata(path)?.ino();
            Ok(Self {
                inner,
                path: path.to_path_buf(),
                ino,
            })
        }

        pub fn accept(&mut self) -> io::Result<Conn> {
            self.inner.accept().map(|(conn, _)| conn)
        }

        /// What [`Remover::remove`] needs once the listener moved to its thread.
        pub fn remover(&self) -> Remover {
            Remover {
                path: self.path.clone(),
                ino: self.ino,
            }
        }
    }

    pub struct Remover {
        path: PathBuf,
        ino: u64,
    }

    impl Remover {
        /// Delete the socket file unless another instance replaced it.
        pub fn remove(&self) {
            if fs::metadata(&self.path).is_ok_and(|m| m.ino() == self.ino) {
                let _ = fs::remove_file(&self.path);
            }
        }
    }

    pub fn connect(path: &Path) -> io::Result<Conn> {
        UnixStream::connect(path)
    }
}

#[cfg(windows)]
mod windows {
    use std::fs::{File, OpenOptions};
    use std::io;
    use std::os::windows::ffi::OsStrExt;
    use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
    use std::path::{Path, PathBuf};
    use std::time::Duration;

    use windows_sys::Win32::Foundation::{
        GetLastError, ERROR_ACCESS_DENIED, ERROR_PIPE_BUSY, ERROR_PIPE_CONNECTED,
        INVALID_HANDLE_VALUE,
    };
    use windows_sys::Win32::Storage::FileSystem::{
        FILE_FLAG_FIRST_PIPE_INSTANCE, PIPE_ACCESS_DUPLEX,
    };
    use windows_sys::Win32::System::Pipes::{
        ConnectNamedPipe, CreateNamedPipeW, PIPE_READMODE_BYTE, PIPE_REJECT_REMOTE_CLIENTS,
        PIPE_TYPE_BYTE, PIPE_UNLIMITED_INSTANCES, PIPE_WAIT,
    };

    pub type Conn = File;

    pub fn default_endpoint() -> PathBuf {
        PathBuf::from(r"\\.\pipe\rustune")
    }

    pub struct Listener {
        name: Vec<u16>,
        /// The instance the next client connects to.
        next: Option<OwnedHandle>,
    }

    fn wide(path: &Path) -> Vec<u16> {
        path.as_os_str().encode_wide().chain(Some(0)).collect()
    }

    fn create(name: &[u16], first: bool) -> io::Result<OwnedHandle> {
        let flags = PIPE_ACCESS_DUPLEX
            | if first {
                FILE_FLAG_FIRST_PIPE_INSTANCE
            } else {
                0
            };
        // SAFETY: `name` is NUL-terminated and outlives the call; null
        // security attributes select the default DACL (owner/admins full
        // control, no write access for other users).
        let handle = unsafe {
            CreateNamedPipeW(
                name.as_ptr(),
                flags,
                PIPE_TYPE_BYTE | PIPE_READMODE_BYTE | PIPE_WAIT | PIPE_REJECT_REMOTE_CLIENTS,
                PIPE_UNLIMITED_INSTANCES,
                4096,
                4096,
                0,
                std::ptr::null(),
            )
        };
        if handle == INVALID_HANDLE_VALUE {
            // SAFETY: plain thread-local error read.
            if first && unsafe { GetLastError() } == ERROR_ACCESS_DENIED {
                return Err(super::already_running());
            }
            return Err(io::Error::last_os_error());
        }
        // SAFETY: a fresh, valid handle we now own.
        Ok(unsafe { OwnedHandle::from_raw_handle(handle) })
    }

    impl Listener {
        /// Create the first pipe instance; fails with `AddrInUse` while
        /// another instance owns the name.
        pub fn bind(path: &Path) -> io::Result<Self> {
            let name = wide(path);
            let next = Some(create(&name, true)?);
            Ok(Self { name, next })
        }

        pub fn accept(&mut self) -> io::Result<Conn> {
            let pipe = match self.next.take() {
                Some(pipe) => pipe,
                None => create(&self.name, false)?,
            };
            // SAFETY: `pipe` is a valid pipe handle; synchronous call.
            let ok = unsafe { ConnectNamedPipe(pipe.as_raw_handle(), std::ptr::null_mut()) };
            // SAFETY: plain thread-local error read.
            if ok == 0 && unsafe { GetLastError() } != ERROR_PIPE_CONNECTED {
                return Err(io::Error::last_os_error());
            }
            // Keep an instance listening while this client is served.
            self.next = create(&self.name, false).ok();
            Ok(File::from(pipe))
        }

        pub fn remover(&self) -> Remover {
            Remover
        }
    }

    /// Pipes vanish with their last handle; nothing to delete.
    pub struct Remover;

    impl Remover {
        pub fn remove(&self) {}
    }

    pub fn connect(path: &Path) -> io::Result<Conn> {
        // All instances can be briefly busy between two accepts.
        for _ in 0..20 {
            match OpenOptions::new().read(true).write(true).open(path) {
                Err(e) if e.raw_os_error() == Some(ERROR_PIPE_BUSY as i32) => {
                    std::thread::sleep(Duration::from_millis(25));
                }
                other => return other,
            }
        }
        OpenOptions::new().read(true).write(true).open(path)
    }
}
