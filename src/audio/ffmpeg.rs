//! Optional ffmpeg fallback for formats symphonia cannot decode (Opus, WebM,
//! WMA, HLS, ...). ffmpeg decodes straight to f32 PCM in the output format, so
//! no further conversion is needed. Seeking restarts the process with `-ss`.

use std::ffi::OsString;
use std::io::{self, BufRead, BufReader, Read};
use std::path::PathBuf;
use std::process::{ChildStderr, ChildStdout, Command, Stdio};
use std::sync::mpsc::{self, RecvTimeoutError, Sender};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use anyhow::{anyhow, bail};
use symphonia::core::meta::{Tag, Value};

use super::decode::{Emit, Pcm};
use super::output::OutFormat;
use super::replaygain::ReplayGainTags;
use super::track::{lock, Track, STOP};
use super::EngineEvent;

/// Longest wait for ffmpeg to report the input format before audio starts.
const HEADER_TIMEOUT: Duration = Duration::from_secs(15);

pub enum FfInput {
    Path(PathBuf),
    Url(String),
    /// Bytes fed through stdin (e.g. an ICY-stripped radio stream). Not seekable.
    Pipe(Box<dyn Read + Send>),
}

pub struct Ffmpeg {
    track: Arc<Track>,
    emit: Emit,
    /// `-i` argument; `None` for stdin input.
    input: Option<OsString>,
    out: OutFormat,
    stdout: Option<ChildStdout>,
    leftover: Vec<u8>,
    got_audio: bool,
    last_line: Arc<Mutex<String>>,
}

impl Ffmpeg {
    /// Spawn ffmpeg. A missing binary yields an `io::Error` of kind `NotFound`.
    pub fn start(
        track: Arc<Track>,
        emit: Emit,
        input: FfInput,
        out: OutFormat,
    ) -> anyhow::Result<Self> {
        let (arg, pipe) = match input {
            FfInput::Path(p) => (Some(p.into_os_string()), None),
            FfInput::Url(u) => (Some(OsString::from(u)), None),
            FfInput::Pipe(r) => (None, Some(r)),
        };
        let mut ff = Self {
            track,
            emit,
            input: arg,
            out,
            stdout: None,
            leftover: Vec::new(),
            got_audio: false,
            last_line: Arc::new(Mutex::new(String::new())),
        };
        ff.spawn(Duration::ZERO, pipe)?;
        Ok(ff)
    }

    fn kill(&mut self) {
        self.stdout = None;
        let child = lock(&self.track.child).take();
        if let Some(mut child) = child {
            let _ = child.kill();
            let _ = child.wait();
        }
    }

    fn spawn(&mut self, start: Duration, pipe: Option<Box<dyn Read + Send>>) -> anyhow::Result<()> {
        self.kill();
        let mut cmd = Command::new("ffmpeg");
        cmd.args(["-hide_banner", "-nostats", "-loglevel", "info"]);
        if pipe.is_none() {
            cmd.arg("-nostdin");
        }
        if !start.is_zero() {
            cmd.arg("-ss").arg(format!("{:.3}", start.as_secs_f64()));
        }
        match &self.input {
            Some(arg) => {
                let lower = arg.to_string_lossy().to_ascii_lowercase();
                if lower.starts_with("http://") || lower.starts_with("https://") {
                    cmd.args([
                        "-reconnect",
                        "1",
                        "-reconnect_streamed",
                        "1",
                        "-reconnect_delay_max",
                        "5",
                    ]);
                }
                cmd.arg("-i").arg(arg);
            }
            None => {
                cmd.args(["-i", "pipe:0"]);
            }
        }
        cmd.args([
            "-map",
            "0:a:0",
            "-vn",
            "-sn",
            "-dn",
            "-f",
            "f32le",
            "-acodec",
            "pcm_f32le",
        ])
        .arg("-ac")
        .arg(self.out.channels.to_string())
        .arg("-ar")
        .arg(self.out.rate.to_string())
        .arg("pipe:1")
        .stdin(if pipe.is_some() {
            Stdio::piped()
        } else {
            Stdio::null()
        })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
        let mut child = cmd.spawn()?;
        let (Some(stdout), Some(stderr)) = (child.stdout.take(), child.stderr.take()) else {
            let _ = child.kill();
            let _ = child.wait();
            bail!("ffmpeg pipes unavailable");
        };
        let stdin = child.stdin.take();
        // Owned by the track from here on: `stop` kills it, drop reaps it.
        *lock(&self.track.child) = Some(child);
        self.stdout = Some(stdout);
        self.leftover.clear();
        if let (Some(mut reader), Some(mut stdin)) = (pipe, stdin) {
            // Ends when ffmpeg exits (broken pipe) or the source ends.
            thread::Builder::new()
                .name("rustune-ffpipe".into())
                .spawn(move || {
                    let _ = io::copy(&mut reader, &mut stdin);
                })?;
        }
        let (ready_tx, ready_rx) = mpsc::channel();
        let ctx = LogCtx {
            track: self.track.clone(),
            emit: self.emit.clone(),
            last_line: self.last_line.clone(),
            seekable: self.input.is_some(),
        };
        thread::Builder::new()
            .name("rustune-fflog".into())
            .spawn(move || ctx.run(stderr, ready_tx))?;
        if self.track.has(STOP) {
            self.kill();
            return Ok(());
        }
        // Let ffmpeg report duration/format first so `Started` carries them.
        let deadline = Instant::now() + HEADER_TIMEOUT;
        loop {
            match ready_rx.recv_timeout(Duration::from_millis(100)) {
                Ok(()) | Err(RecvTimeoutError::Disconnected) => break,
                Err(RecvTimeoutError::Timeout) => {
                    if self.track.has(STOP) || Instant::now() >= deadline {
                        break;
                    }
                }
            }
        }
        Ok(())
    }
}

impl Drop for Ffmpeg {
    fn drop(&mut self) {
        self.kill();
    }
}

impl Pcm for Ffmpeg {
    fn next_chunk(&mut self, out: &mut Vec<f32>) -> anyhow::Result<bool> {
        let frame_bytes = 4 * self.out.channels;
        let mut buf = [0u8; 16 * 1024];
        loop {
            let Some(stdout) = self.stdout.as_mut() else {
                return Ok(false);
            };
            let n = match stdout.read(&mut buf) {
                Ok(n) => n,
                Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
                Err(e) => return Err(e.into()),
            };
            if n == 0 {
                self.stdout = None;
                let child = lock(&self.track.child).take();
                let status = child.and_then(|mut c| c.wait().ok());
                let failed = status.is_some_and(|s| !s.success());
                if failed && !self.got_audio && !self.track.has(STOP) {
                    let last = lock(&self.last_line).clone();
                    let msg = if last.is_empty() {
                        "decoding failed"
                    } else {
                        last.as_str()
                    };
                    bail!("ffmpeg: {msg}");
                }
                return Ok(false);
            }
            self.leftover.extend_from_slice(&buf[..n]);
            let whole = self.leftover.len() / frame_bytes * frame_bytes;
            if whole == 0 {
                continue;
            }
            out.extend(
                self.leftover[..whole]
                    .chunks_exact(4)
                    .map(|b| f32::from_le_bytes([b[0], b[1], b[2], b[3]])),
            );
            self.leftover.drain(..whole);
            self.got_audio = true;
            return Ok(true);
        }
    }

    fn spec(&self) -> (u32, usize) {
        (self.out.rate, self.out.channels)
    }

    fn seek(&mut self, pos: Duration) -> anyhow::Result<()> {
        if self.input.is_none() {
            return Err(anyhow!("stream is not seekable"));
        }
        self.spawn(pos, None)
    }
}

/// Parses ffmpeg's stderr: input duration/bitrate/format, ReplayGain/R128
/// tags, ICY `StreamTitle`, and the last line for error reporting.
struct LogCtx {
    track: Arc<Track>,
    emit: Emit,
    last_line: Arc<Mutex<String>>,
    seekable: bool,
}

impl LogCtx {
    fn run(self, stderr: ChildStderr, ready: Sender<()>) {
        let mut ready = Some(ready);
        let mut header = Header::default();
        let mut last_title = String::new();
        for line in BufReader::new(stderr).split(b'\n') {
            let Ok(line) = line else { break };
            let line = String::from_utf8_lossy(&line);
            let t = line.trim();
            if t.is_empty() {
                continue;
            }
            if let Some(title) = metadata_value(t, "StreamTitle") {
                self.title(title, &mut last_title);
            }
            if ready.is_some() && header.feed(t) {
                self.publish(&header);
                ready = None; // dropping the sender releases the waiting decoder
            }
            *lock(&self.last_line) = t.to_owned();
        }
    }

    fn title(&self, title: &str, last: &mut String) {
        if !title.is_empty() && title != last && !self.track.has(STOP) {
            last.clear();
            last.push_str(title);
            (self.emit)(EngineEvent::StreamTitle {
                token: self.track.token,
                title: title.to_owned(),
            });
        }
    }

    fn publish(&self, h: &Header) {
        self.track.set_replaygain(&h.rg);
        let seekable = self.seekable && h.duration.is_some();
        self.track.update_info(|i| {
            i.duration = h.duration;
            i.format.kbps = h.kbps.or(i.format.kbps);
            i.format.sample_rate = h.rate;
            i.format.channels = h.channels;
            i.seekable = seekable;
            i.replaygain_db = h.rg.preferred_db();
        });
    }
}

/// `"    StreamTitle     : Foo"` → `Some("Foo")` for `key = "StreamTitle"`.
fn metadata_value<'a>(line: &'a str, key: &str) -> Option<&'a str> {
    let (k, v) = line.split_once(':')?;
    k.trim().eq_ignore_ascii_case(key).then_some(v.trim())
}

/// Input description accumulated from ffmpeg's banner.
#[derive(Default, Debug)]
struct Header {
    duration: Option<Duration>,
    kbps: Option<u32>,
    rate: Option<u32>,
    channels: Option<u16>,
    rg: ReplayGainTags,
}

impl Header {
    /// Feed one trimmed line; returns true once the input section is complete.
    fn feed(&mut self, t: &str) -> bool {
        if t.starts_with("Output #")
            || t.starts_with("Stream mapping:")
            || t.starts_with("Press [q]")
        {
            return true;
        }
        if let Some(rest) = t.strip_prefix("Duration:") {
            let mut parts = rest.split(',');
            self.duration = parts.next().and_then(|d| parse_hms(d.trim()));
            for p in parts {
                if let Some(b) = p.trim().strip_prefix("bitrate:") {
                    self.kbps = self.kbps.or_else(|| parse_kbps(b));
                }
            }
        } else if t.starts_with("Stream #") && self.rate.is_none() {
            if let Some((_, audio)) = t.split_once("Audio:") {
                for part in audio.split(',').map(str::trim) {
                    if let Some(hz) = part.strip_suffix(" Hz") {
                        self.rate = hz.trim().parse().ok();
                    } else if let Some(ch) = parse_layout(part) {
                        self.channels.get_or_insert(ch);
                    } else if part.ends_with("kb/s") && self.kbps.is_none() {
                        self.kbps = parse_kbps(part);
                    }
                }
            }
        } else if let Some((k, v)) = t.split_once(':') {
            let tag = Tag::new(None, k.trim(), Value::from(v.trim()));
            self.rg.merge_tags(std::slice::from_ref(&tag));
        }
        false
    }
}

/// "00:03:21.79" → 201.79 s; "N/A" → None.
fn parse_hms(s: &str) -> Option<Duration> {
    let mut it = s.split(':');
    let h: f64 = it.next()?.parse().ok()?;
    let m: f64 = it.next()?.parse().ok()?;
    let sec: f64 = it.next()?.parse().ok()?;
    let total = h * 3600.0 + m * 60.0 + sec;
    (total.is_finite() && total > 0.0)
        .then(|| Duration::from_millis((total * 1000.0).round() as u64))
}

fn parse_kbps(s: &str) -> Option<u32> {
    s.trim().strip_suffix("kb/s")?.trim().parse().ok()
}

fn parse_layout(part: &str) -> Option<u16> {
    let base = part.split('(').next().unwrap_or(part).trim();
    match base {
        "mono" => Some(1),
        "stereo" => Some(2),
        "2.1" | "3.0" => Some(3),
        "quad" | "4.0" | "3.1" => Some(4),
        "5.0" | "4.1" => Some(5),
        "5.1" | "6.0" => Some(6),
        "6.1" | "7.0" => Some(7),
        "7.1" => Some(8),
        _ => base.strip_suffix(" channels")?.trim().parse().ok(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const BANNER: &str = "Input #0, ogg, from 'x.opus':
  Duration: 00:03:21.79, start: 0.007500, bitrate: 97 kb/s
  Stream #0:0: Audio: opus, 48000 Hz, stereo, fltp
    Metadata:
      R128_TRACK_GAIN : -1280
      title           : Song
      StreamTitle     : Artist - Song
Stream mapping:
  Stream #0:0 -> #0:0 (opus (native) -> pcm_f32le (native))
Output #0, f32le, to 'pipe:1':
  Stream #0:0: Audio: pcm_f32le, 44100 Hz, stereo, flt, 2822 kb/s";

    #[test]
    fn parses_ffmpeg_banner() {
        let mut h = Header::default();
        let mut done_at = None;
        for (i, line) in BANNER.lines().enumerate() {
            if h.feed(line.trim()) {
                done_at = Some(i);
                break;
            }
        }
        assert_eq!(done_at, Some(7));
        assert_eq!(h.duration, Some(Duration::from_millis(201_790)));
        assert_eq!(h.kbps, Some(97));
        assert_eq!(h.rate, Some(48_000));
        assert_eq!(h.channels, Some(2));
        assert_eq!(h.rg.track_db, Some(0.0));
    }

    #[test]
    fn parses_live_banner() {
        let mut h = Header::default();
        for line in [
            "Input #0, aac, from 'http://radio/stream':",
            "Duration: N/A, start: 0.000000, bitrate: N/A",
            "Stream #0:0: Audio: aac (HE-AACv2), 44100 Hz, 5.1(side), fltp, 64 kb/s",
        ] {
            assert!(!h.feed(line));
        }
        assert_eq!(h.duration, None);
        assert_eq!(h.kbps, Some(64));
        assert_eq!(h.rate, Some(44_100));
        assert_eq!(h.channels, Some(6));
        assert!(h.feed("Output #0, f32le, to 'pipe:1':"));
    }

    #[test]
    fn small_parsers() {
        assert_eq!(
            parse_hms("01:00:00.50"),
            Some(Duration::from_millis(3_600_500))
        );
        assert_eq!(parse_hms("N/A"), None);
        assert_eq!(parse_layout("3 channels"), Some(3));
        assert_eq!(parse_layout("fltp"), None);
        assert_eq!(
            metadata_value("StreamTitle     : A - B: C", "streamtitle"),
            Some("A - B: C")
        );
    }
}
