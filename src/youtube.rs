use anyhow::{Context, Result};
use serde::Deserialize;
use std::time::Duration;
use tokio::process::Command;

const YTDLP_TIMEOUT_SECS: u64 = 30;

#[derive(Debug, Clone, Deserialize)]
pub struct SearchResult {
    pub id: String,
    pub title: String,
    pub duration: Option<u64>,
    pub channel: Option<String>,
}

/// Search YouTube using yt-dlp. Returns up to 10 results per page.
/// yt-dlp's `ytsearchN:` returns the top N results — there's no offset,
/// so for page > 0 we fetch (page+1)*10 results and slice the last 10.
pub async fn search(query: &str, page: usize) -> Result<Vec<SearchResult>> {
    let count = (page + 1) * 10;
    let search_term = format!("ytsearch{count}:{query}");

    let output = tokio::time::timeout(
        Duration::from_secs(YTDLP_TIMEOUT_SECS),
        Command::new("yt-dlp")
            .arg(&search_term)
            .arg("--dump-json")
            .arg("--no-download")
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
        match serde_json::from_str::<SearchResult>(line) {
            Ok(r) => results.push(r),
            Err(e) => eprintln!("Warning: skipping unparseable result: {e}"),
        }
    }

    // For page > 0, return only the slice corresponding to this page
    let skip = page * 10;
    if skip > 0 && results.len() > skip {
        Ok(results.into_iter().skip(skip).take(10).collect())
    } else if skip > 0 {
        Ok(Vec::new()) // no results for this page
    } else {
        Ok(results)
    }
}

/// Extract the best audio stream URL for a video ID.
pub async fn get_stream_url(video_id: &str) -> Result<String> {
    let url = format!("https://www.youtube.com/watch?v={video_id}");

    let output = tokio::time::timeout(
        Duration::from_secs(YTDLP_TIMEOUT_SECS),
        Command::new("yt-dlp")
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

    let url = String::from_utf8_lossy(&output.stdout);
    let url = url.trim().to_string();

    if url.is_empty() {
        anyhow::bail!("No stream URL returned");
    }

    Ok(url)
}

/// Check if yt-dlp is available on the system.
pub async fn check_ytdlp() -> Result<()> {
    let output = Command::new("yt-dlp")
        .arg("--version")
        .output()
        .await
        .context("yt-dlp not found. Install it with: brew install yt-dlp")?;

    if !output.status.success() {
        anyhow::bail!("yt-dlp check failed. Install it with: brew install yt-dlp");
    }

    Ok(())
}
