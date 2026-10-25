mod ytdlp;

pub use ytdlp::{download_audio, YtdlpExtractor};

use anyhow::Result;
use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use crate::media::{MediaItem, StreamInfo};

pub type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// An extractor resolves media IDs to playable stream URLs and can search.
/// Availability (is the tool installed?) is probed once at startup by
/// `install::probe_deps`, never per call.
pub trait Extractor: Send + Sync {
    fn name(&self) -> &str;
    fn resolve<'a>(&'a self, id: &'a str, title: &'a str) -> BoxFuture<'a, Result<StreamInfo>>;
    fn search<'a>(
        &'a self,
        query: &'a str,
        offset: usize,
        limit: usize,
    ) -> BoxFuture<'a, Result<Vec<MediaItem>>>;
}

/// Registry of extractors, keyed by name.
#[derive(Default)]
pub struct ExtractorRegistry {
    extractors: HashMap<String, Arc<dyn Extractor>>,
}

impl ExtractorRegistry {
    pub fn add(&mut self, extractor: Arc<dyn Extractor>) {
        self.extractors
            .insert(extractor.name().to_string(), extractor);
    }

    pub fn get(&self, name: &str) -> Option<&Arc<dyn Extractor>> {
        self.extractors.get(name)
    }
}

/// Recognise a web URL an extractor can resolve. Returns `(extractor, id)`.
pub fn parse_url(url: &str) -> Option<(&'static str, String)> {
    let rest = url
        .strip_prefix("https://")
        .or_else(|| url.strip_prefix("http://"))?;
    let (host, path) = rest.split_once('/').unwrap_or((rest, ""));
    let host = host.strip_prefix("www.").unwrap_or(host);
    let host = host.strip_prefix("m.").unwrap_or(host);
    let id = match host {
        "youtube.com" | "music.youtube.com" => {
            let query = path.strip_prefix("watch?")?;
            query
                .split('&')
                .find_map(|kv| kv.strip_prefix("v="))?
                .to_string()
        }
        "youtu.be" => path.split(['?', '#']).next()?.to_string(),
        _ => return None,
    };
    let valid = !id.is_empty()
        && id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_');
    valid.then_some(("ytdlp", id))
}

/// Canonical web URL for an extractor item (used when saving playlists).
pub fn item_url(extractor: &str, id: &str) -> Option<String> {
    (extractor == "ytdlp").then(|| format!("https://www.youtube.com/watch?v={id}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn youtube_urls_are_recognised() {
        for url in [
            "https://www.youtube.com/watch?v=dQw4w9WgXcQ",
            "http://youtube.com/watch?feature=x&v=dQw4w9WgXcQ",
            "https://m.youtube.com/watch?v=dQw4w9WgXcQ&t=10",
            "https://music.youtube.com/watch?v=dQw4w9WgXcQ",
            "https://youtu.be/dQw4w9WgXcQ?t=3",
        ] {
            assert_eq!(
                parse_url(url),
                Some(("ytdlp", "dQw4w9WgXcQ".to_string())),
                "{url}"
            );
        }
    }

    #[test]
    fn other_urls_are_not() {
        assert_eq!(parse_url("https://radio.example/stream.mp3"), None);
        assert_eq!(parse_url("https://www.youtube.com/channel/x"), None);
        assert_eq!(parse_url("https://youtu.be/"), None);
        assert_eq!(parse_url("https://youtu.be/../etc"), None);
        assert_eq!(parse_url("/local/file.mp3"), None);
    }

    #[test]
    fn item_url_round_trips() {
        let url = item_url("ytdlp", "abc_-1").unwrap();
        assert_eq!(parse_url(&url), Some(("ytdlp", "abc_-1".to_string())));
        assert_eq!(item_url("other", "x"), None);
    }
}
