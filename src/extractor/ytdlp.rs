use anyhow::{Context, Result};
use serde::Deserialize;
use std::path::Path;
use std::process::Stdio;
use std::time::Duration;

use super::{BoxFuture, Extractor};
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
            let url = format!("https://www.youtube.com/watch?v={id}");
            let output = tokio::time::timeout(
                self.timeout,
                command()
                    .args(["-f", NATIVE_FIRST_FORMAT, "--get-url", "--"])
                    .arg(&url)
                    .output(),
            )
            .await
            .context("Stream extraction timed out")?
            .context("Failed to run yt-dlp (is it installed?)")?;

            if !output.status.success() {
                anyhow::bail!("Stream extraction failed: {}", stderr_tail(&output.stderr));
            }
            let stdout = String::from_utf8_lossy(&output.stdout);
            let stream_url = stdout.lines().next().unwrap_or("").trim();
            if stream_url.is_empty() {
                anyhow::bail!("No stream URL returned");
            }
            Ok(StreamInfo {
                url: stream_url.to_string(),
                title: title.to_string(),
            })
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
    let safe_title = if safe_title.is_empty() {
        id
    } else {
        safe_title
    };
    let output_template = music_dir.join(format!("{safe_title}.%(ext)s"));
    let url = format!("https://www.youtube.com/watch?v={id}");

    let output = tokio::time::timeout(
        Duration::from_secs(300),
        command()
            .args(["-x", "--audio-format", "best", "--embed-thumbnail", "-o"])
            .arg(&output_template)
            .arg("--")
            .arg(&url)
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
    fn stderr_tail_takes_last_line() {
        assert_eq!(stderr_tail(b"WARNING: x\nERROR: boom\n\n"), "ERROR: boom");
        assert_eq!(stderr_tail(b""), "unknown error");
    }
}
