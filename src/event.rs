use std::path::PathBuf;

use anyhow::Result;

use crate::app::SkinEntry;
use crate::audio::EngineEvent;
use crate::browse::Entry;
use crate::install::Deps;
use crate::media::{MediaItem, StreamInfo};
use crate::net::SkinPage;
use crate::providers::lyrics::Lyrics;
use crate::providers::ProviderKind;
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
    /// The audio engine reported progress of a track (engine thread).
    Engine(EngineEvent),
    /// LRCLIB answered for the track identified by `key`.
    LyricsReady {
        key: String,
        result: Result<Option<Lyrics>, String>,
    },
    /// A provider browser level finished loading.
    BrowseLoaded {
        kind: ProviderKind,
        token: u64,
        result: crate::providers::Result<Vec<Entry>>,
    },
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
