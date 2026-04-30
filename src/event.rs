use anyhow::Result;

use crate::extractor::ExtractorStatus;
use crate::media::{MediaItem, SourceKind, StreamInfo};

#[derive(Debug)]
#[allow(dead_code)]
pub enum AppEvent {
    ItemsReady {
        results: Result<Vec<MediaItem>>,
        source: SourceKind,
    },
    StreamReady(Result<StreamInfo>),
    PlaybackProgress {
        elapsed_secs: u64,
        duration_secs: u64,
    },
    PlaybackComplete,
    PlaybackError(String),
    ExtractorStatus {
        name: String,
        status: ExtractorStatus,
    },
    ScanComplete(Result<Vec<MediaItem>>),
    SkinListFetched {
        entries: Result<(Vec<crate::app::SkinEntry>, usize)>,
        requested_offset: usize,
    },
    SkinDownloaded {
        md5: String,
        filename: String,
        result: Result<()>,
    },
    DownloadComplete {
        title: String,
        result: Result<()>,
    },
}
