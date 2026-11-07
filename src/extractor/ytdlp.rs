use anyhow::{Context, Result};
use serde::Deserialize;
use std::path::Path;
use std::process::Stdio;
use std::time::Duration;

use super::{page_url, BoxFuture, Extractor};
use crate::media::{MediaItem, SourceKind, StreamInfo};

#[derive(Debug, Clone, Deserialize)]
struct YtdlpResult {
    id: String,
    title: String,
    #[serde(default, deserialize_with = "deserialize_duration")]
    duration: Option<u64>,
    #[serde(default)]
    channel: Option<String>,
}

fn deserialize_duration<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> Result<Option<u64>, D::Error> {
    let val: Option<serde_json::Value> = Option::deserialize(deserializer)?;
    Ok(match val {
        Some(serde_json::Value::Number(n)) => n.as_f64().filter(|d| *d >= 0.0).map(|d| d as u64),
        Some(serde_json::Value::String(s)) => s.parse::<u64>().ok(),
        _ => None,
    })
}

/// Parse `--dump-json` output (one JSON object per line); unparseable lines
/// (warnings, partial output) are skipped.
fn parse_search_output(stdout: &str) -> Vec<MediaItem> {
    stdout
        .lines()
        .filter(|l| !l.trim().is_empty())
        .filter_map(|l| serde_json::from_str::<YtdlpResult>(l).ok())
        .map(|r| MediaItem {
            id: r.id,
            title: r.title,
            artist: None,
            album: None,
            duration: r.duration,
            subtitle: r.channel,
            source: SourceKind::Extractor("ytdlp".into()),
        })
        .collect()
}

/// yt-dlp format selector for playback. Prefers progressive HTTP AAC/MP3 that
/// the built-in engine decodes and seeks natively (YouTube itag 140). YouTube's
/// plain `bestaudio` is WebM/Opus, which would keep an ffmpeg process decoding
/// for the whole track. HLS/Opus-only sources still fall through to `bestaudio`.
const NATIVE_FIRST_FORMAT: &str = "bestaudio[ext=m4a][protocol^=http]/bestaudio[acodec^=mp4a][protocol^=http]/bestaudio[acodec=mp3][protocol^=http]/bestaudio[protocol^=http]/bestaudio/best";

/// Longest wait for a link YouTube has not released yet. Its TV clients hold
/// links back for the length of a skippable pre-roll ad (about 5 s).
const MAX_AVAILABILITY_WAIT: Duration = Duration::from_secs(15);

/// `--print available_at --print urls --print title` output: when the link
/// starts working (Unix seconds, `NA` when it already does), the stream URL,
/// then the page title (used when `fallback_title` is empty).
fn parse_resolve_output(stdout: &str, fallback_title: &str) -> Result<(StreamInfo, Option<f64>)> {
    let mut lines = stdout.lines().map(str::trim).filter(|l| !l.is_empty());
    let available_at = lines.next().and_then(|l| l.parse::<f64>().ok());
    let url = lines
        .next()
        .filter(|l| l.contains("://"))
        .context("No stream URL returned")?;
    let page_title = lines.collect::<Vec<_>>().join(" ");
    let info = StreamInfo {
        url: url.to_string(),
        title: fallback_title.to_string(),
        page_title: (!page_title.is_empty()).then_some(page_title),
    };
    Ok((info, available_at))
}

/// How long to wait before the link may be fetched: YouTube answers 403 to
/// every request before `available_at`, which would fail the track.
fn availability_wait(available_at: Option<f64>, now: f64) -> Duration {
    match available_at {
        Some(at) if at > now => Duration::from_secs_f64(at - now).min(MAX_AVAILABILITY_WAIT),
        _ => Duration::ZERO,
    }
}

fn unix_now() -> f64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0.0, |d| d.as_secs_f64())
}

fn command() -> tokio::process::Command {
    let mut cmd = tokio::process::Command::new("yt-dlp");
    cmd.stdin(Stdio::null()).kill_on_drop(true);
    cmd
}

fn stderr_tail(stderr: &[u8]) -> String {
    let text = String::from_utf8_lossy(stderr);
    text.lines()
        .rev()
        .find(|l| !l.trim().is_empty())
        .unwrap_or("unknown error")
        .trim()
        .to_string()
}

pub struct YtdlpExtractor {
    timeout: Duration,
}

impl YtdlpExtractor {
    pub fn new(timeout: Duration) -> Self {
        Self { timeout }
    }
}

impl Extractor for YtdlpExtractor {
    fn name(&self) -> &str {
        "ytdlp"
    }

    fn resolve<'a>(&'a self, id: &'a str, title: &'a str) -> BoxFuture<'a, Result<StreamInfo>> {
        Box::pin(async move {
            let url = page_url(id);
            // `available_at`, then `urls` (one line per format, one here),
            // then the title.
            let output = tokio::time::timeout(
                self.timeout,
                command()
                    .args([
                        "-f",
                        NATIVE_FIRST_FORMAT,
                        "--no-playlist",
                        "--playlist-items",
                        "1",
                    ])
                    .args(["--print", "available_at", "--print", "urls"])
                    .args(["--print", "title", "--"])
                    .arg(url.as_ref())
                    .output(),
            )
            .await
            .context("Stream extraction timed out")?
            .context("Failed to run yt-dlp (is it installed?)")?;

            if !output.status.success() {
                anyhow::bail!("Stream extraction failed: {}", stderr_tail(&output.stderr));
            }
            let (info, available_at) =
                parse_resolve_output(&String::from_utf8_lossy(&output.stdout), title)?;
            // Skipping the track aborts this task, so the wait never blocks
            // the next one.
            tokio::time::sleep(availability_wait(available_at, unix_now())).await;
            Ok(info)
        })
    }

    fn search<'a>(
        &'a self,
        query: &'a str,
        offset: usize,
        limit: usize,
    ) -> BoxFuture<'a, Result<Vec<MediaItem>>> {
        Box::pin(async move {
            // Ask for offset+limit hits but only dump the requested window.
            let search_term = format!("ytsearch{}:{query}", offset + limit);
            let window = format!("{}:{}", offset + 1, offset + limit);
            let output = tokio::time::timeout(
                self.timeout,
                command()
                    .args(["--dump-json", "--no-download", "--flat-playlist"])
                    .args(["--playlist-items", window.as_str(), "--"])
                    .arg(&search_term)
                    .output(),
            )
            .await
            .context("Search timed out")?
            .context("Failed to run yt-dlp (is it installed?)")?;

            if !output.status.success() {
                anyhow::bail!("yt-dlp search failed: {}", stderr_tail(&output.stderr));
            }
            Ok(parse_search_output(&String::from_utf8_lossy(
                &output.stdout,
            )))
        })
    }
}

/// Download a video's audio into `music_dir` (needs yt-dlp; ffmpeg for `-x`).
pub async fn download_audio(id: &str, title: &str, music_dir: &Path) -> Result<()> {
    let safe_title: String = title
        .chars()
        .map(|c| match c {
            '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|' => '_',
            c if c.is_control() => '_',
            c => c,
        })
        .collect();
    let safe_title = safe_title.trim().trim_start_matches('.');
    let safe_title = if safe_title.is_empty() || safe_title.contains("://") {
        "download"
    } else {
        safe_title
    };
    let output_template = music_dir.join(format!("{safe_title}.%(ext)s"));
    let url = page_url(id);
    let output = tokio::time::timeout(
        Duration::from_secs(300),
        command()
            .args([
                "-x",
                "--audio-format",
                "best",
                "--embed-thumbnail",
                "--no-playlist",
                "-o",
            ])
            .arg(&output_template)
            .arg("--")
            .arg(url.as_ref())
            .output(),
    )
    .await
    .context("Download timed out")?
    .context("Failed to run yt-dlp (is it installed?)")?;

    if !output.status.success() {
        anyhow::bail!("{}", stderr_tail(&output.stderr));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn search_output_parsing_skips_noise() {
        let out = r#"{"id":"a1","title":"First","duration":61.4,"channel":"Chan"}
WARNING: something
{"id":"b2","title":"Second","duration":"30"}

{"id":"c3","title":"Third","duration":null}"#;
        let items = parse_search_output(out);
        assert_eq!(items.len(), 3);
        assert_eq!(items[0].duration, Some(61));
        assert_eq!(items[0].subtitle.as_deref(), Some("Chan"));
        assert_eq!(items[1].duration, Some(30));
        assert_eq!(items[2].duration, None);
        assert!(items
            .iter()
            .all(|i| i.source == SourceKind::Extractor("ytdlp".into())));
    }

    #[test]
    fn resolve_output_gives_the_stream_and_page_title() {
        let (info, at) =
            parse_resolve_output("1794021615\nhttps://cdn.example/a.m4a\nSome Song\n", "").unwrap();
        assert_eq!(info.url, "https://cdn.example/a.m4a");
        assert_eq!(info.page_title.as_deref(), Some("Some Song"));
        assert_eq!(at, Some(1794021615.0));
        let (kept, at) = parse_resolve_output("NA\nhttps://cdn.example/a.m4a\n", "Known").unwrap();
        assert_eq!(
            (kept.title.as_str(), kept.page_title, at),
            ("Known", None, None)
        );
        assert!(parse_resolve_output("", "x").is_err());
        assert!(parse_resolve_output("NA\nNA\n", "x").is_err());
    }

    #[test]
    fn waits_until_the_link_is_released() {
        assert_eq!(availability_wait(None, 100.0), Duration::ZERO);
        assert_eq!(availability_wait(Some(90.0), 100.0), Duration::ZERO);
        assert_eq!(
            availability_wait(Some(104.5), 100.0),
            Duration::from_millis(4500)
        );
        assert_eq!(availability_wait(Some(1e12), 100.0), MAX_AVAILABILITY_WAIT);
    }

    #[test]
    fn stderr_tail_takes_last_line() {
        assert_eq!(stderr_tail(b"WARNING: x\nERROR: boom\n\n"), "ERROR: boom");
        assert_eq!(stderr_tail(b""), "unknown error");
    }
}
