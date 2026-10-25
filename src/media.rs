/// Generic media item shared by every source (local files, yt-dlp, providers).
#[derive(Debug, Clone, PartialEq)]
pub struct MediaItem {
    /// Local: absolute file path. Extractor: native ID (e.g. YouTube video ID).
    /// Url: a directly playable http(s) URL.
    pub id: String,
    pub title: String,
    pub artist: Option<String>,
    pub album: Option<String>,
    /// Duration in whole seconds, when known.
    pub duration: Option<u64>,
    /// Secondary line for lists: channel, station tags, album, directory...
    pub subtitle: Option<String>,
    pub source: SourceKind,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SourceKind {
    Local,
    /// Needs resolving through the named extractor (e.g. "ytdlp").
    Extractor(String),
    /// `id` is a directly playable http(s) URL (radio, podcast episode, Subsonic/Jellyfin stream).
    Url,
}

/// Resolved stream info ready for playback.
#[derive(Debug, Clone)]
pub struct StreamInfo {
    /// `file://` path for local, stream URL for extractor.
    pub url: String,
    pub title: String,
}
