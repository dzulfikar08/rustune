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

/// Hosts whose pages yt-dlp turns into audio (videos, tracks, mixes).
const PAGE_HOSTS: &[&str] = &[
    "youtube.com",
    "music.youtube.com",
    "youtu.be",
    "soundcloud.com",
    "bandcamp.com",
    "vimeo.com",
    "mixcloud.com",
];

/// File extensions of directly playable media: such URLs stream natively
/// even on a page host.
const MEDIA_EXTENSIONS: &[&str] = &[
    "mp3", "m4a", "aac", "ogg", "oga", "opus", "flac", "wav", "aiff", "aif", "wma", "mp4", "webm",
    "m3u", "m3u8", "pls",
];

/// Recognise a web page an extractor can resolve. Returns `(extractor, id)`;
/// the id is the page URL itself, which [`Extractor::resolve`] accepts as is.
/// Direct media URLs (`.../track.mp3`) and other hosts return `None`.
pub fn parse_url(url: &str) -> Option<(&'static str, String)> {
    let url = url.trim();
    let rest = url
        .strip_prefix("https://")
        .or_else(|| url.strip_prefix("http://"))?;
    let end = rest.find(['/', '?', '#']).unwrap_or(rest.len());
    let (authority, path) = rest.split_at(end);
    let host = authority
        .rsplit('@')
        .next()
        .unwrap_or(authority)
        .split(':')
        .next()
        .unwrap_or("")
        .to_ascii_lowercase();
    let host = host.strip_prefix("www.").unwrap_or(&host);
    let host = host.strip_prefix("m.").unwrap_or(host);
    let page_host = PAGE_HOSTS.iter().any(|h| {
        host == *h
            // Artist subdomains: `artist.bandcamp.com`.
            || (*h == "bandcamp.com" && host.ends_with(".bandcamp.com"))
    });
    if !page_host {
        return None;
    }
    let path = path.split(['?', '#']).next().unwrap_or("");
    let media_file = path
        .rsplit('/')
        .next()
        .and_then(|name| name.rsplit_once('.'))
        .is_some_and(|(_, ext)| MEDIA_EXTENSIONS.iter().any(|m| ext.eq_ignore_ascii_case(m)));
    // A bare host (`https://youtu.be/`) is no page to play.
    let has_page = !path.trim_matches('/').is_empty();
    (has_page && !media_file).then(|| ("ytdlp", url.to_string()))
}

/// The page yt-dlp opens for an item id: a full URL as is, a bare id as its
/// YouTube watch page (search results and older sessions store bare ids).
pub fn page_url(id: &str) -> std::borrow::Cow<'_, str> {
    if id.starts_with("https://") || id.starts_with("http://") {
        std::borrow::Cow::Borrowed(id)
    } else {
        std::borrow::Cow::Owned(format!("https://www.youtube.com/watch?v={id}"))
    }
}

/// Canonical web URL for an extractor item (used when saving playlists).
pub fn item_url(extractor: &str, id: &str) -> Option<String> {
    (extractor == "ytdlp").then(|| page_url(id).into_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn page(url: &str) -> Option<(&'static str, String)> {
        Some(("ytdlp", url.to_string()))
    }

    #[test]
    fn page_urls_go_through_ytdlp_with_the_url_as_id() {
        for url in [
            "https://www.youtube.com/watch?v=dQw4w9WgXcQ",
            "http://youtube.com/watch?feature=x&v=dQw4w9WgXcQ",
            "https://m.youtube.com/watch?v=dQw4w9WgXcQ&t=10",
            "https://music.youtube.com/watch?v=dQw4w9WgXcQ",
            "https://youtu.be/dQw4w9WgXcQ?t=3",
            "https://www.youtube.com/playlist?list=PL123",
            "https://soundcloud.com/artist/track",
            "https://artist.bandcamp.com/track/song",
            "https://vimeo.com/12345",
            "https://www.mixcloud.com/dj/mix/",
        ] {
            assert_eq!(parse_url(url), page(url), "{url}");
        }
    }

    #[test]
    fn direct_media_other_hosts_and_paths_are_not_pages() {
        assert_eq!(parse_url("https://radio.example/stream.mp3"), None);
        assert_eq!(parse_url("https://radio.example/live"), None);
        assert_eq!(parse_url("https://soundcloud.com/x/clip.MP3?sig=1"), None);
        assert_eq!(parse_url("https://artist.bandcamp.com/stream.m3u8"), None);
        assert_eq!(parse_url("https://notbandcamp.com/track/x"), None);
        assert_eq!(
            parse_url("https://youtube.com.evil.example/watch?v=x"),
            None
        );
        assert_eq!(parse_url("https://youtu.be/"), None);
        assert_eq!(parse_url("/local/file.mp3"), None);
        assert_eq!(parse_url("youtube.com/watch?v=x"), None);
    }

    #[test]
    fn item_url_round_trips() {
        assert_eq!(
            item_url("ytdlp", "abc_-1").as_deref(),
            Some("https://www.youtube.com/watch?v=abc_-1")
        );
        let url = "https://soundcloud.com/a/b";
        assert_eq!(item_url("ytdlp", url).as_deref(), Some(url));
        assert_eq!(parse_url(url), page(url));
        assert_eq!(item_url("other", "x"), None);
        assert_eq!(page_url("abc"), "https://www.youtube.com/watch?v=abc");
    }
}
