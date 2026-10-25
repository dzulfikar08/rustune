use std::path::PathBuf;

use anyhow::Result;

use crate::app::SkinEntry;
use crate::install::Deps;
use crate::media::{MediaItem, StreamInfo};
use crate::net::SkinPage;
use crate::skin::WinampSkin;

/// Everything the controller reacts to: terminal input (from the input
/// thread), playback backends and background tasks.
pub enum AppEvent {
    Term(crossterm::event::Event),
    /// Online search results; `token` identifies the request.
    ItemsReady {
        token: u64,
        results: Result<Vec<MediaItem>>,
    },
    /// An extractor resolved the stream for play request `token`.
    StreamReady {
        token: u64,
        result: Result<StreamInfo>,
    },
    PlaybackProgress {
        elapsed_secs: u64,
        duration_secs: u64,
    },
    /// The playing track reached its end (any backend).
    PlaybackComplete,
    DepsProbed(Deps),
    ScanComplete(Result<Vec<MediaItem>>),
    /// Items resolved from command-line arguments or the open prompt.
    QueueResolved {
        items: Vec<MediaItem>,
        errors: Vec<String>,
    },
    SkinListFetched {
        token: u64,
        page: Result<SkinPage>,
        requested_offset: usize,
    },
    LocalSkinsListed(Vec<SkinEntry>),
    SkinDownloaded {
        md5: String,
        result: Result<PathBuf>,
    },
    SkinLoaded {
        path: Option<PathBuf>,
        result: Result<Box<WinampSkin>>,
    },
    /// The base skin was fetched in the background (`Some` = newly installed).
    DefaultSkinReady(Result<Option<PathBuf>>),
    DownloadComplete {
        title: String,
        result: Result<()>,
    },
    StateSaved(Result<()>),
}
