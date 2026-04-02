use anyhow::{Context, Result};
use serde::Deserialize;
use std::time::Duration;

use crate::extractor::{Extractor, ExtractorStatus};
use crate::media::{MediaItem, SourceKind, StreamInfo};

const SEARCH_TIMEOUT_SECS: u64 = 60;

#[derive(Debug, Clone, Deserialize)]
struct YtdlpResult {
    pub id: String,
    pub title: String,
    #[serde(default, deserialize_with = "deserialize_duration")]
    pub duration: Option<u64>,
    #[serde(default)]
    pub channel: Option<String>,
}

fn deserialize_duration<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> Result<Option<u64>, D::Error> {
    let val: Option<serde_json::Value> = Option::deserialize(deserializer)?;
    match val {
        None => Ok(None),
        Some(serde_json::Value::Number(n)) => Ok(n.as_u64()),
        Some(serde_json::Value::String(s)) => Ok(s.parse::<u64>().ok()),
        _ => Ok(None),
    }
}

pub struct YtdlpExtractor;

impl YtdlpExtractor {
    pub fn new() -> Self {
        Self
    }
}

impl Extractor for YtdlpExtractor {
    fn name(&self) -> &str {
        "ytdlp"
    }

    fn status(&self) -> ExtractorStatus {
        match std::process::Command::new("yt-dlp").arg("--version").output() {
            Ok(output) if output.status.success() => {
                let version = String::from_utf8_lossy(&output.stdout).trim().to_string();
                ExtractorStatus::Available(version)
            }
            Ok(_) => ExtractorStatus::Broken("yt-dlp check failed".into()),
            Err(_) => ExtractorStatus::NotFound,
        }
    }

    fn resolve(&self, id: &str, title: &str) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<StreamInfo>> + Send + '_>> {
        let id = id.to_string();
        let title = title.to_string();
        Box::pin(async move {
            let url = format!("https://www.youtube.com/watch?v={id}");
            let output = tokio::time::timeout(
                Duration::from_secs(SEARCH_TIMEOUT_SECS),
                tokio::process::Command::new("yt-dlp")
                    .arg("-f")
                    .arg("bestaudio/best")
                    .arg("--get-url")
                    .arg(&url)
                    .output(),
            )
            .await
            .context("Stream extraction timed out.")?
            .context("Failed to run yt-dlp for stream extraction")?;

            if !output.status.success() {
                let stderr = String::from_utf8_lossy(&output.stderr);
                anyhow::bail!("Stream extraction failed: {}", stderr.trim());
            }

            let stream_url = String::from_utf8_lossy(&output.stdout);
            let stream_url = stream_url.trim().to_string();

            if stream_url.is_empty() {
                anyhow::bail!("No stream URL returned");
            }

            Ok(StreamInfo {
                url: stream_url,
                title,
            })
        })
    }

    fn search(&self, query: &str, offset: usize, limit: usize) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<Vec<MediaItem>>> + Send + '_>> {
        let query = query.to_string();
        Box::pin(async move {
            let count = offset + limit;
            let search_term = format!("ytsearch{count}:{query}");

            let output = tokio::time::timeout(
                Duration::from_secs(SEARCH_TIMEOUT_SECS),
                tokio::process::Command::new("yt-dlp")
                    .arg(&search_term)
                    .arg("--dump-json")
                    .arg("--no-download")
                    .arg("--flat-playlist")
                    .output(),
            )
            .await
            .context("Search timed out.")?
            .context("Failed to run yt-dlp. Is it installed?")?;

            if !output.status.success() {
                let stderr = String::from_utf8_lossy(&output.stderr);
                anyhow::bail!("yt-dlp search failed: {}", stderr.trim());
            }

            let stdout = String::from_utf8_lossy(&output.stdout);
            let mut results = Vec::new();

            for line in stdout.lines() {
                if line.trim().is_empty() {
                    continue;
                }
                match serde_json::from_str::<YtdlpResult>(line) {
                    Ok(r) => results.push(MediaItem {
                        id: r.id,
                        title: r.title,
                        duration: r.duration,
                        subtitle: r.channel,
                        source: SourceKind::Extractor("ytdlp".into()),
                    }),
                    Err(e) => eprintln!("Warning: skipping unparseable result: {e}"),
                }
            }

            // For offset > 0, return only the slice for this page
            if offset > 0 && results.len() > offset {
                Ok(results.into_iter().skip(offset).take(limit).collect())
            } else if offset > 0 {
                Ok(Vec::new())
            } else {
                Ok(results)
            }
        })
    }
}
