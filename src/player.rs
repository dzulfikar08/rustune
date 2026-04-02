use anyhow::{Context, Result};
use std::sync::Arc;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixStream;
use tokio::sync::{mpsc, oneshot, Mutex};

use crate::event::AppEvent;

/// Check if mpv is available on the system.
pub async fn check_mpv() -> Result<()> {
    let output = tokio::process::Command::new("mpv")
        .arg("--version")
        .output()
        .await
        .context("mpv not found. Install it with: brew install mpv")?;

    if !output.status.success() {
        anyhow::bail!("mpv check failed. Install it with: brew install mpv");
    }

    Ok(())
}

/// Play a stream URL via mpv. Sends events through `tx`.
/// Stops if `kill_rx` receives a signal.
pub async fn play(
    url: String,
    _title: String,
    tx: mpsc::UnboundedSender<AppEvent>,
    mut kill_rx: oneshot::Receiver<()>,
) {
    let socket_path = format!("/tmp/litetube-mpv-{}.sock", std::process::id());
    let _ = std::fs::remove_file(&socket_path);

    let child = match tokio::process::Command::new("mpv")
        .arg("--no-video")
        .arg("--idle=no")
        .arg(format!("--input-ipc-server={socket_path}"))
        .arg(&url)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
    {
        Ok(c) => c,
        Err(e) => {
            let _ = tx.send(AppEvent::PlaybackError(format!("Failed to start mpv: {e}")));
            return;
        }
    };

    let child = Arc::new(Mutex::new(child));

    // Spawn IPC progress reader task
    let ipc_tx = tx.clone();
    let ipc_socket = socket_path.clone();
    let ipc_handle = tokio::spawn(async move {
        // Wait for socket to appear (up to 5 seconds)
        for _ in 0..50 {
            if std::path::Path::new(&ipc_socket).exists() {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        }

        let stream = match UnixStream::connect(&ipc_socket).await {
            Ok(s) => s,
            Err(_) => return,
        };

        let (reader, mut writer) = stream.into_split();
        let mut reader = BufReader::new(reader);
        let mut request_id: u64 = 0;

        loop {
            // Request time-pos
            request_id += 1;
            let time_id = request_id;
            let req = format!(r#"{{"command":["get_property","time-pos"],"request_id":{time_id}}}"#);
            if writer
                .write_all(format!("{req}\n").as_bytes())
                .await
                .is_err()
            {
                break;
            }

            // Request duration
            request_id += 1;
            let dur_id = request_id;
            let req = format!(r#"{{"command":["get_property","duration"],"request_id":{dur_id}}}"#);
            if writer
                .write_all(format!("{req}\n").as_bytes())
                .await
                .is_err()
            {
                break;
            }
            let _ = writer.flush().await;

            // Read responses, using request_id to distinguish
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
                let _ = ipc_tx.send(AppEvent::PlaybackProgress {
                    elapsed_secs: e as u64,
                    duration_secs: d as u64,
                });
            }

            tokio::time::sleep(std::time::Duration::from_millis(500)).await;
        }
    });

    // Wait for mpv to exit or kill signal
    let child_clone = child.clone();
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

    ipc_handle.abort();
    let _ = std::fs::remove_file(&socket_path);
    let _ = tx.send(AppEvent::PlaybackComplete);
}

/// Set pause state on the mpv IPC socket.
pub async fn set_pause(paused: bool) -> Result<()> {
    let socket_path = format!("/tmp/litetube-mpv-{}.sock", std::process::id());

    let stream = UnixStream::connect(&socket_path)
        .await
        .context("Failed to connect to mpv IPC socket")?;

    let (_, mut writer) = stream.into_split();

    let request = format!(r#"{{"command":["set_property","pause",{paused}]}}"#);
    writer
        .write_all(format!("{request}\n").as_bytes())
        .await?;
    writer.flush().await?;

    Ok(())
}
