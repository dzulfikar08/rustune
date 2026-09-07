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
        }
    }
}

pub use local::LocalHandle;

/// Clean up stale temp audio files from previous sessions.
pub fn clean_temp_dir() {
    let temp_dir = std::env::temp_dir().join("rustune-streams");
    if temp_dir.exists() {
        let _ = std::fs::remove_dir_all(&temp_dir);
    }
    let _ = std::fs::create_dir_all(&temp_dir);
}
