use anyhow::{Context, Result};

use crate::event::AppEvent;
use tokio::sync::mpsc;

/// Check if mpv is available on the system.
pub async fn check_mpv() -> Result<()> {
    let output = tokio::process::Command::new("mpv")
        .arg("--version")
        .output()
        .await
        .context("mpv not found. Install it from https://mpv.io")?;

    if !output.status.success() {
        anyhow::bail!("mpv check failed. Install it from https://mpv.io");
    }

    Ok(())
}

// ── Pure Rust local playback (rodio + symphonia) ────────────────────

mod local {
    use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
    use std::sync::{Arc, Mutex};
    use std::time::Duration;

    use anyhow::Context;
    use rodio::{Decoder, Player, Source};
    use rodio::stream::DeviceSinkBuilder;
    use tokio::sync::mpsc;

    use crate::event::AppEvent;

    pub struct LocalHandle {
        _device_sink: rodio::stream::MixerDeviceSink,
        player: Arc<Mutex<Player>>,
        progress_abort: Option<tokio::task::JoinHandle<()>>,
        duration: Arc<AtomicU64>,
        playing: Arc<AtomicBool>,
    }

    impl LocalHandle {
        pub fn new(tx: mpsc::UnboundedSender<AppEvent>) -> anyhow::Result<Self> {
            let device_sink = DeviceSinkBuilder::open_default_sink()
                .context("No audio output device found")?;
            let mixer = device_sink.mixer();
            let player = Player::connect_new(mixer);
            let player = Arc::new(Mutex::new(player));

            let duration = Arc::new(AtomicU64::new(0));
            let playing = Arc::new(AtomicBool::new(false));

            let player_clone = player.clone();
            let duration_clone = duration.clone();
            let playing_clone = playing.clone();
            let progress_abort = Some(tokio::spawn(async move {
                loop {
                    tokio::time::sleep(Duration::from_millis(500)).await;
                    if playing_clone.load(Ordering::Relaxed) {
                        let e = player_clone.lock().unwrap().get_pos().as_secs();
                        let d = duration_clone.load(Ordering::Relaxed);
                        let _ = tx.send(AppEvent::PlaybackProgress {
                            elapsed_secs: e,
                            duration_secs: d,
                        });
                    }
                }
            }));

            Ok(Self {
                _device_sink: device_sink,
                player,
                progress_abort,
                duration,
                playing,
            })
        }

        pub fn play_file(&self, path: &str) -> anyhow::Result<()> {
            let path = path.strip_prefix("file://").unwrap_or(path);

            let player = self.player.lock().unwrap();
            player.stop();
            self.playing.store(false, Ordering::Relaxed);

            let file = std::fs::File::open(path)
                .with_context(|| format!("Failed to open audio file: {path}"))?;
            let reader = std::io::BufReader::new(file);

            let source = Decoder::new(reader)
                .with_context(|| format!("Failed to decode audio file: {path}"))?;

            let total_duration = source.total_duration()
                .map(|d: Duration| d.as_secs())
                .unwrap_or(0);
            self.duration.store(total_duration, Ordering::Relaxed);

            player.append(source);
            self.playing.store(true, Ordering::Relaxed);

            Ok(())
        }

        pub fn set_pause(&self, paused: bool) {
            let player = self.player.lock().unwrap();
            if paused {
                player.pause();
            } else {
                player.play();
            }
        }

        pub fn seek_to(&self, pos: Duration) -> anyhow::Result<()> {
            self.player.lock().unwrap()
                .try_seek(pos)
                .map_err(|e| anyhow::anyhow!("Seek failed: {e}"))?;
            Ok(())
        }

        pub fn stop(&self) {
            self.player.lock().unwrap().stop();
            self.playing.store(false, Ordering::Relaxed);
        }

        pub fn is_finished(&self) -> bool {
            self.player.lock().unwrap().empty()
        }

        pub fn shutdown(mut self) {
            if let Some(h) = self.progress_abort.take() {
                h.abort();
            }
            self.player.lock().unwrap().stop();
            // Player drops here, stopping all sounds
        }
    }
}

pub use local::LocalHandle;

// ── mpv backend for online/streaming playback (Unix) ────────────────

#[cfg(unix)]
mod mpv_imp {
    use super::*;
    use std::path::PathBuf;
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
    use tokio::net::UnixStream;
    use tokio::process::Child;

    pub struct MpvHandle {
        child: Child,
        socket: PathBuf,
        progress_abort: Option<tokio::task::JoinHandle<()>>,
    }

    impl MpvHandle {
        pub fn new(tx: mpsc::UnboundedSender<AppEvent>) -> Result<Self> {
            let socket = std::env::temp_dir().join(format!(
                "rustune-mpv-{}.sock",
                std::process::id()
            ));
            let _ = std::fs::remove_file(&socket);

            let child = tokio::process::Command::new("mpv")
                .arg("--no-video")
                .arg("--idle=yes")
                .arg("--cache=no")
                .arg("--demuxer-max-bytes=10M")
                .arg("--demuxer-max-back-bytes=5M")
                .arg(format!("--input-ipc-server={}", socket.display()))
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .spawn()
                .context("Failed to start mpv")?;

            let socket_clone = socket.clone();
            let progress_abort = Some(tokio::spawn(async move {
                wait_for_socket(&socket_clone).await;
                let stream = match UnixStream::connect(&socket_clone).await {
                    Ok(s) => s,
                    Err(_) => return,
                };
                let (reader, mut writer) = stream.into_split();
                let mut reader = BufReader::new(reader);
                let mut request_id: u64 = 0;

                loop {
                    request_id += 1;
                    let time_id = request_id;
                    let req = format!(
                        r#"{{"command":["get_property","time-pos"],"request_id":{time_id}}}"#
                    );
                    if writer
                        .write_all(format!("{req}\n").as_bytes())
                        .await
                        .is_err()
                    {
                        break;
                    }

                    request_id += 1;
                    let dur_id = request_id;
                    let req = format!(
                        r#"{{"command":["get_property","duration"],"request_id":{dur_id}}}"#
                    );
                    if writer
                        .write_all(format!("{req}\n").as_bytes())
                        .await
                        .is_err()
                    {
                        break;
                    }
                    let _ = writer.flush().await;

                    let mut elapsed: Option<f64> = None;
                    let mut duration: Option<f64> = None;

                    for _ in 0..4 {
                        let mut line = String::new();
                        match reader.read_line(&mut line).await {
                            Ok(0) | Err(_) => break,
                            _ => {}
                        }

                        if let Ok(val) = serde_json::from_str::<serde_json::Value>(&line) {
                            let rid = val
                                .get("request_id")
                                .and_then(|v| v.as_u64())
                                .unwrap_or(0);

                            if rid == time_id {
                                if let Some(d) = val.get("data").and_then(|v| v.as_f64()) {
                                    elapsed = Some(d);
                                }
                            } else if rid == dur_id {
                                if let Some(d) = val.get("data").and_then(|v| v.as_f64()) {
                                    duration = Some(d);
                                }
                            }
                        }

                        if elapsed.is_some() && duration.is_some() {
                            break;
                        }
                    }

                    if let (Some(e), Some(d)) = (elapsed, duration) {
                        let _ = tx.send(AppEvent::PlaybackProgress {
                            elapsed_secs: e as u64,
                            duration_secs: d as u64,
                        });
                    }

                    tokio::time::sleep(std::time::Duration::from_millis(500)).await;
                }
            }));

            Ok(Self {
                child,
                socket,
                progress_abort,
            })
        }

        pub fn loadfile(&self, url: &str) -> Result<()> {
            let socket = self.socket.clone();
            let url = url.to_string();
            // Wait for socket in background, then send command
            tokio::spawn(async move {
                // Wait up to 5s for mpv socket
                for _ in 0..50 {
                    if socket.exists() {
                        break;
                    }
                    tokio::time::sleep(std::time::Duration::from_millis(100)).await;
                }
                let _ = loadfile_inner(&socket, &url).await;
            });
            Ok(())
        }

        pub fn set_pause(&self, paused: bool) -> Result<()> {
            let socket = self.socket.clone();
            tokio::spawn(async move {
                for _ in 0..50 {
                    if socket.exists() { break; }
                    tokio::time::sleep(std::time::Duration::from_millis(100)).await;
                }
                let _ = set_property(&socket, "pause", paused).await;
            });
            Ok(())
        }

        pub fn seek_to(&self, position_secs: f64) -> Result<()> {
            let socket = self.socket.clone();
            tokio::spawn(async move {
                for _ in 0..50 {
                    if socket.exists() { break; }
                    tokio::time::sleep(std::time::Duration::from_millis(100)).await;
                }
                let _ = set_property(&socket, "time-pos", position_secs).await;
            });
            Ok(())
        }

        pub fn shutdown(mut self) {
            if let Some(h) = self.progress_abort.take() {
                h.abort();
            }
            let _ = self.child.start_kill();
            let socket = self.socket.clone();
            tokio::spawn(async move {
                let _ = std::fs::remove_file(&socket);
            });
        }
    }

    async fn loadfile_inner(socket: &std::path::Path, url: &str) -> Result<()> {
        let stream = UnixStream::connect(socket)
            .await
            .context("Failed to connect to mpv IPC socket")?;
        let (_, mut writer) = stream.into_split();
        let escaped = url.replace('\\', "\\\\").replace('"', "\\\"");
        let req = format!(
            r#"{{"command":["loadfile","{escaped}"],"request_id":1}}"#
        );
        writer
            .write_all(format!("{req}\n").as_bytes())
            .await?;
        writer.flush().await?;
        Ok(())
    }

    async fn set_property<T: std::fmt::Display>(
        socket: &std::path::Path,
        property: &str,
        value: T,
    ) -> Result<()> {
        let stream = UnixStream::connect(socket)
            .await
            .context("Failed to connect to mpv IPC socket")?;
        let (_, mut writer) = stream.into_split();
        let req = format!(
            r#"{{"command":["set_property","{property}",{value}],"request_id":1}}"#
        );
        writer
            .write_all(format!("{req}\n").as_bytes())
            .await?;
        writer.flush().await?;
        Ok(())
    }

    async fn wait_for_socket(path: &std::path::Path) {
        for _ in 0..50 {
            if path.exists() {
                return;
            }
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        }
    }
}

#[cfg(unix)]
pub use mpv_imp::MpvHandle;

// ── mpv backend for online/streaming (non-Unix) ─────────────────────

#[cfg(not(unix))]
mod mpv_imp {
    use super::*;
    use std::sync::Arc;
    use tokio::sync::Mutex;

    pub struct MpvHandle {
        child: Arc<Mutex<tokio::process::Child>>,
        kill_tx: Option<tokio::sync::oneshot::Sender<()>>,
    }

    impl MpvHandle {
        pub fn new(
            url: String,
            tx: mpsc::UnboundedSender<AppEvent>,
        ) -> Result<Self> {
            let child = tokio::process::Command::new("mpv")
                .arg("--no-video")
                .arg("--idle=no")
                .arg("--cache=no")
                .arg("--demuxer-max-bytes=10M")
                .arg("--demuxer-max-back-bytes=5M")
                .arg("--cache-secs=5")
                .arg(&url)
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .spawn()
                .context("Failed to start mpv")?;

            let (kill_tx, mut kill_rx) = tokio::sync::oneshot::channel::<()>();
            let child = Arc::new(Mutex::new(child));
            let child_clone = child.clone();

            tokio::spawn(async move {
                tokio::select! {
                    _ = &mut kill_rx => {
                        let mut guard = child_clone.lock().await;
                        let _ = guard.kill().await;
                    }
                    status = async {
                        let mut guard = child.lock().await;
                        guard.wait().await
                    } => {
                        let _ = status;
                    }
                }
                let _ = tx.send(AppEvent::PlaybackComplete);
            });

            Ok(Self {
                child,
                kill_tx: Some(kill_tx),
            })
        }

        pub fn shutdown(mut self) {
            if let Some(kill) = self.kill_tx.take() {
                let _ = kill.send(());
            }
        }
    }
}

#[cfg(not(unix))]
pub use mpv_imp::MpvHandle;
