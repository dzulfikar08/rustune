#[allow(dead_code)]
/// Generic media item — replaces YouTube-specific SearchResult.
#[derive(Debug, Clone)]
pub struct MediaItem {
    /// Local: absolute file path. Extractor: native ID (e.g. YouTube video ID).
    pub id: String,
    pub title: String,
    pub duration: Option<u64>,
    /// Channel, artist, or album — whatever the source provides.
    pub subtitle: Option<String>,
    pub source: SourceKind,
}

#[derive(Debug, Clone, PartialEq)]
pub enum SourceKind {
    Local,
    Extractor(String), // e.g. "ytdlp"
}

/// Resolved stream info ready for mpv playback.
#[derive(Debug, Clone)]
pub struct StreamInfo {
    /// `file://` path for local, stream URL for extractor.
    pub url: String,
    pub title: String,
}
