use std::path::PathBuf;

use anyhow::Result;

use crate::audio::EngineEvent;
use crate::browse::Entry;
use crate::install::Deps;
use crate::media::{MediaItem, StreamInfo};
use crate::net::SkinPage;
use crate::providers::lyrics::Lyrics;
use crate::providers::ProviderKind;
use crate::skin::WinampSkin;
use crate::skin_browser::SkinEntry;

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
    /// Items resolved from command-line arguments, the open prompt or an
    /// `Enqueue` command (`append`: add to a non-empty queue, don't replace).
    QueueResolved {
        items: Vec<MediaItem>,
        errors: Vec<String>,
        append: bool,
    },
    SkinListFetched {
        token: u64,
        page: Result<SkinPage>,
        requested_offset: usize,
    },
    LocalSkinsListed {
        token: u64,
        entries: Vec<SkinEntry>,
    },
    /// A museum skin was installed into the skins directory.
    SkinInstalled {
        md5: String,
        result: Result<PathBuf>,
    },
    /// The skin browser's live preview of the skin `key` was loaded.
    SkinPreviewLoaded {
        key: String,
        result: Result<Box<WinampSkin>>,
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
    /// A player command from outside the key handler (IPC socket, OS media
    /// controls); applied through `Controller::apply`.
    Command(crate::command::Command),
    /// Answer once every earlier event is applied and observers have seen the
    /// result, so an IPC client's next request reads fresh state.
    Flush(std::sync::mpsc::SyncSender<()>),
    /// An IPC request the player answers itself (queue, lyrics, screen,
    /// keys, search, skin); the answer goes to `reply`.
    Query {
        query: crate::ipc::protocol::Query,
        reply: crate::ipc::protocol::Reply,
    },
    /// Send `line` to an IPC client once every earlier event is applied.
    Answer {
        reply: crate::ipc::protocol::Reply,
        line: String,
    },
    /// A search requested over IPC finished (`opened`: the podcast show or
    /// album these tracks were loaded from).
    SearchDone {
        search: crate::ipc::protocol::Search,
        result: Result<Vec<Entry>>,
        reply: crate::ipc::protocol::Reply,
        opened: Option<String>,
    },
}
