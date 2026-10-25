//! The controller owns `App`, the audio engine seam, the Winamp skin view
//! state and the graphics painter. It applies every `Command` through one
//! dispatcher (`apply`), reacts to engine and background events, and runs
//! the redraw-on-change event loop (paced by the visualizer when it shows).

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use anyhow::Result;
use crossterm::event::{
    DisableMouseCapture, EnableMouseCapture, Event, KeyEvent, MouseEvent, MouseEventKind,
};
use ratatui::DefaultTerminal;
use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender};
use tokio::time::Instant;

use crate::app::{
    display_title, App, BrowseAction, InputAction, InputPurpose, LyricsStatus, Mode, MouseAction,
    OnboardingAction, PlaybackState, SettingsAction, SkinBrowserAction, SkinBrowserSource,
    SkinEntry, Status, Tab,
};
use crate::audio::{EngineEvent, Source, TrackInfo};
use crate::browse::{self, BrowseOutcome, Services};
use crate::command::Command;
use crate::config::{self, Config};
use crate::eq_presets;
use crate::event::AppEvent;
use crate::extractor::{self, ExtractorRegistry, YtdlpExtractor};
use crate::gfx::GfxPainter;
use crate::install::{self, Dep, DepStatus};
use crate::library;
use crate::media::{MediaItem, SourceKind};
use crate::net;
use crate::playback::Playback;
use crate::playlist_io;
use crate::providers::lyrics::{LrclibClient, Lyrics, TrackQuery};
use crate::providers::ProviderKind;
use crate::report::{self, Call, Tracker};
use crate::skin::WinampSkin;
use crate::state::{QueueEntry, SessionState};
use crate::term_input::InputThread;
use crate::theme::Theme;
use crate::ui;
use crate::view::{PlayStatus, RepeatMode, StreamFormat, EQ_BANDS, EQ_MAX_DB};
use crate::vis::pacing::FramePacer;
use crate::winamp::WinampUiState;

/// Session state is written at most this long after the first unsaved change.
const SAVE_DELAY: Duration = Duration::from_secs(2);
/// "Previous" restarts the current track when it has played longer than this.
const PREV_RESTART_SECS: u64 = 3;
const SKIN_PAGE: usize = 50;
/// Clock resolution while synced lyrics are on screen.
const LYRICS_TICK: Duration = Duration::from_millis(250);

/// The next queue item, decoding ahead in the engine for a gapless switch.
struct Preload {
    token: u64,
    id: String,
}

pub struct Controller {
    pub app: App,
    playback: Playback,
    tx: UnboundedSender<AppEvent>,
    registry: ExtractorRegistry,
    services: Arc<Services>,
    /// Pending resume position: (queue index, seconds).
    resume: Option<(usize, u64)>,
    /// Engine token of the current track (also tags extractor resolution).
    play_token: u64,
    /// Last token handed out (engine tokens are unique per session).
    token_seq: u64,
    /// The engine reported `Started` for the current track.
    started: bool,
    preload: Option<Preload>,
    /// The current track ended and the engine is switching to the preload.
    awaiting_switch: bool,
    /// Tracks that failed in a row; skipping stops after a full queue.
    failures: usize,
    /// Now-playing title from the stream (ICY), when it differs from the item.
    stream_title: Option<String>,
    tracker: Option<Tracker>,
    lyrics_cache: HashMap<String, Option<Lyrics>>,
    lyrics_key: Option<String>,
    search_token: u64,
    skin_fetch_token: u64,
    save_at: Option<Instant>,
    saving: bool,
    pub winamp_ui: WinampUiState,
    pub gfx: GfxPainter,
    pacer: FramePacer,
    /// Last analyzer update.
    vis_at: Instant,
    /// Height of the last drawn frame (decides whether the vis strip shows).
    screen_height: u16,
}

/// Parse a `:command` typed into the prompt into a player `Command`.
fn parse_colon(input: &str) -> Result<Command, String> {
    let words: Vec<&str> = input.split_whitespace().collect();
    let name = words.first().copied().unwrap_or("");
    let arg = words.get(1).copied();
    let num = |s: Option<&str>| -> Result<f32, String> {
        s.and_then(|v| v.parse::<f32>().ok())
            .filter(|v| v.is_finite())
            .ok_or_else(|| format!(":{name} needs a number"))
    };
    Ok(match (name, arg) {
        ("q" | "quit", None) => Command::Quit,
        ("play", None) => Command::Play,
        ("pause", None) => Command::Pause,
        ("toggle", None) => Command::TogglePause,
        ("stop", None) => Command::Stop,
        ("next", None) => Command::Next,
        ("prev", None) => Command::Prev,
        ("open", None) => Command::Eject,
        ("shuffle", None) => Command::ToggleShuffle,
        ("repeat", None) => Command::CycleRepeat,
        ("repeat", Some(mode)) => Command::SetRepeat(match mode {
            "off" => RepeatMode::Off,
            "all" => RepeatMode::All,
            "one" => RepeatMode::One,
            _ => return Err(format!("Unknown repeat mode '{mode}' (off, all, one)")),
        }),
        ("seek", Some(pos)) => parse_seek(pos)?,
        ("vol" | "volume", v) => Command::SetVolume(num(v)? / 100.0),
        ("bal" | "balance", v) => Command::SetBalance(num(v)? / 100.0),
        ("preamp", v) => Command::SetPreamp(num(v)?),
        ("band", Some(b)) => {
            let band = b
                .parse::<usize>()
                .ok()
                .filter(|n| (1..=EQ_BANDS).contains(n))
                .ok_or_else(|| format!("Band must be 1-{EQ_BANDS}"))?;
            Command::SetEqBand {
                band: band - 1,
                db: num(words.get(2).copied())?,
            }
        }
        ("eq", Some("toggle")) => Command::ToggleEq,
        ("eq", Some("auto")) => Command::ToggleEqAuto,
        ("eq", Some(_)) => Command::EqPreset(words[1..].join(" ")),
        _ => return Err(format!("Unknown command: :{input}")),
    })
}

/// `90`, `1:30`, `1:02:03` → absolute; `+10` / `-10` → relative seconds.
fn parse_seek(pos: &str) -> Result<Command, String> {
    let bad = || format!("Bad position '{pos}' (use 90, 1:30, +10 or -10)");
    if let Some(delta) = pos.strip_prefix('+') {
        return delta.parse::<i64>().map(Command::SeekBy).map_err(|_| bad());
    }
    if pos.starts_with('-') {
        return pos.parse::<i64>().map(Command::SeekBy).map_err(|_| bad());
    }
    let mut secs: u64 = 0;
    for part in pos.split(':') {
        let n = part.parse::<u64>().map_err(|_| bad())?;
        secs = secs
            .checked_mul(60)
            .and_then(|s| s.checked_add(n))
            .ok_or_else(bad)?;
    }
    Ok(Command::SeekTo(Duration::from_secs(secs)))
}

/// Only the last of a burst of drags / resizes matters.
fn supersedes(prev: &AppEvent, next: &AppEvent) -> bool {
    match (prev, next) {
        (AppEvent::Term(Event::Resize(..)), AppEvent::Term(Event::Resize(..))) => true,
        (AppEvent::Term(Event::Mouse(a)), AppEvent::Term(Event::Mouse(b))) => {
            matches!(a.kind, MouseEventKind::Drag(_)) && a.kind == b.kind
        }
        _ => false,
    }
}

/// How the engine should open `item`, unless it needs extractor resolution.
fn engine_source(item: &MediaItem) -> Option<Source> {
    match &item.source {
        SourceKind::Local => Some(Source::File(PathBuf::from(&item.id))),
        SourceKind::Url => Some(Source::Url(item.id.clone())),
        SourceKind::Extractor(_) => None,
    }
}

/// Artist and title to look lyrics up by: tags first, else "Artist - Title".
fn lyrics_query(artist: Option<&str>, title: &str) -> (String, String) {
    match artist {
        Some(a) if !a.trim().is_empty() => (a.trim().to_string(), title.trim().to_string()),
        _ => match title.split_once(" - ") {
            Some((a, t)) => (a.trim().to_string(), t.trim().to_string()),
            None => (String::new(), title.trim().to_string()),
        },
    }
}

impl Controller {
    pub fn new(
        config: Config,
        state: SessionState,
        tx: UnboundedSender<AppEvent>,
        playback: Playback,
        gfx: GfxPainter,
    ) -> Self {
        let mut registry = ExtractorRegistry::default();
        registry.add(Arc::new(YtdlpExtractor::new(Duration::from_secs(
            config.search_timeout_secs.max(1),
        ))));
        let services = Arc::new(Services::new(config.providers.clone()));

        let mut app = App::new(config);
        if let Some(theme) = state.theme.as_deref() {
            app.theme = Theme::from_name(theme);
            app.config.theme = app.theme.name.clone();
            app.sync_analyzer();
        }
        app.volume = state.volume;
        app.balance = state.balance;
        app.eq = state.eq;
        app.vis_mode = state.vis;
        app.queue.set_shuffle(state.shuffle);
        app.queue.set_repeat(state.repeat);
        app.skin_path = state
            .skin
            .as_deref()
            .and_then(|s| Path::new(s).file_name())
            .map(|name| config::skins_dir().join(name));
        let resume = state
            .queue_index
            .map(|i| (i, state.position_secs))
            .filter(|&(_, pos)| pos > 0);
        let items: Vec<MediaItem> = state.queue.into_iter().map(MediaItem::from).collect();
        app.queue.replace(items, state.queue_index);

        // Restore the sound settings into the engine.
        playback.set_volume(app.volume);
        playback.set_balance(app.balance);
        playback.set_eq(app.eq);
        playback.set_replaygain(app.config.replaygain);
        if let Some(reason) = playback.error() {
            app.error(format!("{reason} — playback is unavailable"));
        }

        Self {
            app,
            playback,
            tx,
            registry,
            services,
            resume,
            play_token: 0,
            token_seq: 0,
            started: false,
            preload: None,
            awaiting_switch: false,
            failures: 0,
            stream_title: None,
            tracker: None,
            lyrics_cache: HashMap::new(),
            lyrics_key: None,
            search_token: 0,
            skin_fetch_token: 0,
            save_at: None,
            saving: false,
            winamp_ui: WinampUiState::new(),
            gfx,
            pacer: FramePacer::default(),
            vis_at: Instant::now(),
            screen_height: 0,
        }
    }

    /// Kick off background startup work; nothing here blocks.
    pub fn start(&mut self, inputs: Vec<String>) {
        let tx = self.tx.clone();
        tokio::spawn(async move {
            let _ = tx.send(AppEvent::DepsProbed(install::probe_deps().await));
        });

        if self.app.config.onboarding_done {
            self.start_scan();
        }
        if self.app.is_winamp() {
            self.load_skin(self.app.skin_path.clone());
        }
        let tx = self.tx.clone();
        tokio::task::spawn_blocking(move || {
            let result = net::ensure_default_skin(&config::skins_dir());
            let _ = tx.send(AppEvent::DefaultSkinReady(result));
        });

        if !inputs.is_empty() {
            self.resolve_into_queue(inputs);
        }
    }

    fn next_token(&mut self) -> u64 {
        self.token_seq += 1;
        self.token_seq
    }

    // ── background helpers ───────────────────────────────────────────────

    fn start_scan(&mut self) {
        self.app
            .set_status(Status::Scanning("Scanning music directory…".into()));
        let root = self.app.config.music_dir.clone();
        let exts = self.app.config.extensions.clone();
        let tx = self.tx.clone();
        tokio::task::spawn_blocking(move || {
            let cache = config::cache_dir().join("library.json");
            let result = library::scan(&root, &exts, Some(&cache));
            let _ = tx.send(AppEvent::ScanComplete(result));
        });
    }

    fn resolve_into_queue(&mut self, inputs: Vec<String>) {
        self.app.set_status(Status::Loading("Opening…".into()));
        let exts = self.app.config.extensions.clone();
        let tx = self.tx.clone();
        tokio::task::spawn_blocking(move || {
            let (items, errors) = crate::cli::resolve_inputs(&inputs, &exts);
            let _ = tx.send(AppEvent::QueueResolved { items, errors });
        });
    }

    /// Load a skin file in the background; `None` picks the first installed
    /// skin, or the built-in colours when there is none.
    fn load_skin(&mut self, path: Option<PathBuf>) {
        let tx = self.tx.clone();
        tokio::task::spawn_blocking(move || {
            let path = path
                .filter(|p| p.is_file())
                .or_else(|| net::list_skins(&config::skins_dir()).into_iter().next());
            let result = match &path {
                Some(p) => WinampSkin::from_wsz(p).map(Box::new),
                None => Ok(Box::new(WinampSkin::default_skin())),
            };
            let _ = tx.send(AppEvent::SkinLoaded { path, result });
        });
    }

    fn fetch_skins(&mut self, offset: usize) {
        self.skin_fetch_token += 1;
        let token = self.skin_fetch_token;
        let query = self.app.skin_search_query.trim().to_string();
        self.app.skin_browser_loading = true;
        self.app.skin_browser_error = None;
        let tx = self.tx.clone();
        tokio::task::spawn_blocking(move || {
            let dir = config::skins_dir();
            let page = if query.is_empty() {
                net::fetch_skin_list(offset, SKIN_PAGE, &dir)
            } else {
                net::search_skins(&query, offset, SKIN_PAGE, &dir)
            };
            let _ = tx.send(AppEvent::SkinListFetched {
                token,
                page,
                requested_offset: offset,
            });
        });
    }

    fn list_local_skins(&mut self) {
        self.app.skin_browser_loading = true;
        let tx = self.tx.clone();
        tokio::task::spawn_blocking(move || {
            let entries = net::list_skins(&config::skins_dir())
                .into_iter()
                .map(|p| {
                    let filename = p
                        .file_name()
                        .map(|n| n.to_string_lossy().into_owned())
                        .unwrap_or_default();
                    let display_name = WinampSkin::peek_metadata(&p)
                        .ok()
                        .map(|(name, _, _)| name)
                        .filter(|s| !s.trim().is_empty())
                        .unwrap_or_else(|| filename.trim_end_matches(".wsz").to_string());
                    SkinEntry {
                        md5: String::new(),
                        filename,
                        display_name,
                        is_local: true,
                        average_color: None,
                    }
                })
                .collect();
            let _ = tx.send(AppEvent::LocalSkinsListed(entries));
        });
    }

    fn dispatch_search(&mut self) {
        let Some(query) = self.app.input_history.last().cloned() else {
            return;
        };
        let name = self.app.config.extractor.clone();
        let Some(ext) = self.registry.get(&name).cloned() else {
            self.app
                .error(format!("Unknown extractor '{name}' in config"));
            return;
        };
        if name == "ytdlp" && self.app.deps.ytdlp == DepStatus::Missing {
            self.app
                .error("yt-dlp is not installed — online search is unavailable");
            return;
        }
        self.app
            .set_status(Status::Searching(format!("Searching “{query}”…")));
        self.app.results.clear();
        self.app.list_state.select(Some(0));
        self.search_token += 1;
        let token = self.search_token;
        let limit = self.app.config.page_size.max(1);
        let offset = self.app.page * limit;
        let tx = self.tx.clone();
        tokio::spawn(async move {
            let results = ext.search(&query, offset, limit).await;
            let _ = tx.send(AppEvent::ItemsReady { token, results });
        });
    }

    // ── persistence ──────────────────────────────────────────────────────

    fn mark_dirty(&mut self) {
        if self.save_at.is_none() {
            self.save_at = Some(Instant::now() + SAVE_DELAY);
        }
    }

    pub fn save_deadline(&self) -> Option<Instant> {
        self.save_at.filter(|_| !self.saving)
    }

    fn session_state(&self) -> SessionState {
        let queue = &self.app.queue;
        let position_secs = match (&self.app.playback, self.resume) {
            (Some(pb), _) => pb.elapsed.as_secs(),
            (None, Some((idx, pos))) if Some(idx) == queue.current_index() => pos,
            _ => 0,
        };
        SessionState {
            volume: self.app.volume,
            balance: self.app.balance,
            shuffle: queue.shuffle(),
            repeat: queue.repeat(),
            skin: self
                .app
                .skin_path
                .as_deref()
                .and_then(Path::file_name)
                .map(|n| n.to_string_lossy().into_owned()),
            theme: Some(self.app.theme.name.clone()),
            queue_index: queue.current_index(),
            position_secs,
            vis: self.app.vis_mode,
            eq: self.app.eq,
            queue: queue.items().iter().map(QueueEntry::from).collect(),
        }
    }

    /// Write the session in the background (debounced autosave).
    pub fn flush_state(&mut self) {
        self.save_at = None;
        let text = match self.session_state().to_toml() {
            Ok(text) => text,
            Err(e) => {
                self.app.error(format!("Cannot save session: {e:#}"));
                return;
            }
        };
        self.saving = true;
        let tx = self.tx.clone();
        tokio::task::spawn_blocking(move || {
            let result = config::write_atomic(&config::state_path(), text.as_bytes())
                .map_err(anyhow::Error::from);
            let _ = tx.send(AppEvent::StateSaved(result));
        });
    }

    fn save_config(&mut self) {
        if let Err(e) = self.app.config.save() {
            self.app.error(format!("Cannot save config: {e:#}"));
        }
    }

    /// Final synchronous save and engine teardown.
    pub fn shutdown(self) {
        let _ = self.app.config.save();
        let _ = self.session_state().save(&config::state_path());
        self.playback.stop();
    }

    // ── playback reporting ───────────────────────────────────────────────

    fn send_reports(&self, calls: Vec<Call>) {
        if calls.is_empty() {
            return;
        }
        let services = self.services.clone();
        tokio::task::spawn_blocking(move || {
            for call in &calls {
                // Reporting is best effort; failures never interrupt playback.
                let _ = report::send(&services, call);
            }
        });
    }

    fn start_report(&mut self) {
        let Some(item) = self.app.queue.current() else {
            return;
        };
        let uuid = self
            .app
            .browsers
            .iter()
            .find(|b| b.kind == ProviderKind::Radio)
            .and_then(|b| b.radio_uuid(&item.id));
        if let Some(target) = report::classify(item, &self.services, uuid) {
            let (tracker, calls) = Tracker::start(target);
            self.tracker = Some(tracker);
            self.send_reports(calls);
        }
    }

    fn finish_report(&mut self) {
        if let Some(tracker) = self.tracker.take() {
            let elapsed = self
                .app
                .playback
                .as_ref()
                .map_or_else(Duration::default, |p| p.elapsed);
            self.send_reports(tracker.stop(elapsed));
        }
    }

    fn progress_report(&mut self) {
        let (Some(tracker), Some(pb)) = (self.tracker.as_mut(), self.app.playback.as_ref()) else {
            return;
        };
        let calls = tracker.progress(pb.elapsed, pb.duration, pb.paused);
        self.send_reports(calls);
    }

    // ── lyrics ───────────────────────────────────────────────────────────

    /// Show (from cache) or fetch the lyrics of the playing track. Fetches
    /// only while the panel is open.
    fn request_lyrics(&mut self) {
        if !self.app.config.providers.lyrics.enabled {
            return;
        }
        let (Some(item), Some(pb)) = (self.app.queue.current(), self.app.playback.as_ref()) else {
            self.app.lyrics = LyricsStatus::Idle;
            self.lyrics_key = None;
            return;
        };
        let (artist, title) = match &self.stream_title {
            Some(stream) => lyrics_query(None, stream),
            None => lyrics_query(item.artist.as_deref(), &item.title),
        };
        let album = item.album.clone();
        let duration = pb.duration.map(|d| d.as_secs());
        let key = format!("{artist}\u{1f}{title}");
        if self.lyrics_key.as_deref() == Some(key.as_str()) {
            return;
        }
        if let Some(cached) = self.lyrics_cache.get(&key) {
            self.app.lyrics = match cached {
                Some(lyrics) => LyricsStatus::Ready(lyrics.clone()),
                None => LyricsStatus::NotFound,
            };
            self.lyrics_key = Some(key);
            return;
        }
        if !self.app.show_lyrics {
            self.app.lyrics = LyricsStatus::Idle;
            self.lyrics_key = None;
            return;
        }
        self.app.lyrics = LyricsStatus::Loading;
        self.lyrics_key = Some(key.clone());
        let server = self.app.config.providers.lyrics.server.clone();
        let tx = self.tx.clone();
        tokio::task::spawn_blocking(move || {
            let query = TrackQuery {
                artist: &artist,
                title: &title,
                album: album.as_deref(),
                duration,
            };
            let result = LrclibClient::new(&server)
                .fetch(net::agent(), &query)
                .map_err(|e| e.to_string());
            let _ = tx.send(AppEvent::LyricsReady { key, result });
        });
    }

    // ── playback ─────────────────────────────────────────────────────────

    /// Start the queue's current item.
    fn play_current(&mut self) {
        self.finish_report();
        let Some(item) = self.app.queue.current().cloned() else {
            return;
        };
        self.play_token = self.next_token();
        self.started = false;
        self.preload = None;
        self.awaiting_switch = false;
        self.stream_title = None;
        self.app.playback = Some(PlaybackState {
            title: display_title(&item),
            duration: item.duration.map(Duration::from_secs),
            elapsed: Duration::ZERO,
            paused: false,
            format: StreamFormat::default(),
        });
        self.lyrics_key = None;
        self.app.lyrics = LyricsStatus::Idle;
        self.mark_dirty();
        if let Some(src) = engine_source(&item) {
            self.start_engine(src);
            return;
        }
        let SourceKind::Extractor(name) = &item.source else {
            return;
        };
        self.playback.stop();
        let Some(ext) = self.registry.get(name).cloned() else {
            self.app.playback = None;
            self.app.error(format!("Unknown extractor '{name}'"));
            return;
        };
        self.app
            .set_status(Status::Loading(format!("Resolving {}…", item.title)));
        let token = self.play_token;
        let tx = self.tx.clone();
        let (id, title) = (item.id.clone(), item.title.clone());
        tokio::spawn(async move {
            let result = ext.resolve(&id, &title).await;
            let _ = tx.send(AppEvent::StreamReady { token, result });
        });
    }

    fn start_engine(&mut self, src: Source) {
        if let Err(e) = self.playback.play(src, self.play_token) {
            self.app.playback = None;
            self.app.error(format!("{e:#}"));
        }
    }

    /// The engine produced the first audio of the current track.
    fn on_started(&mut self, info: TrackInfo) {
        self.started = true;
        self.failures = 0;
        if let Some(pb) = &mut self.app.playback {
            if info.duration.is_some() {
                pb.duration = info.duration;
            }
            pb.format = info.format;
        }
        if matches!(self.app.status, Status::Loading(_)) {
            self.app.set_status(Status::Idle);
        }
        if let Some((idx, pos)) = self.resume.take() {
            if Some(idx) == self.app.queue.current_index() && info.seekable {
                self.seek_to(Duration::from_secs(pos));
            }
        }
        self.start_report();
        self.request_lyrics();
    }

    /// Keep the engine's preload equal to what the queue plays next.
    fn refresh_preload(&mut self) {
        if !self.started || self.awaiting_switch || self.app.playback.is_none() {
            return;
        }
        let next = self
            .app
            .queue
            .peek_next()
            .and_then(|item| engine_source(item).map(|src| (item.id.clone(), src)));
        match (next, &self.preload) {
            (Some((id, _)), Some(p)) if p.id == id => {}
            (Some((id, src)), _) => {
                let token = self.next_token();
                self.playback.preload(src, token);
                self.preload = Some(Preload { token, id });
            }
            (None, Some(_)) => {
                self.playback.cancel_preload();
                self.preload = None;
            }
            (None, None) => {}
        }
    }

    /// The preloaded track took over gaplessly: advance the queue to it.
    fn on_gapless_switch(&mut self, token: u64, id: &str, info: TrackInfo) {
        self.awaiting_switch = false;
        self.finish_report();
        let advanced = self.app.queue.advance_auto().map(|item| item.id.clone());
        if advanced.as_deref() != Some(id) {
            // The queue changed under the switch: play what it says now.
            self.play_current();
            return;
        }
        let Some(item) = self.app.queue.current() else {
            return;
        };
        self.play_token = token;
        self.stream_title = None;
        self.app.playback = Some(PlaybackState {
            title: display_title(item),
            duration: item.duration.map(Duration::from_secs),
            elapsed: Duration::ZERO,
            paused: false,
            format: StreamFormat::default(),
        });
        self.lyrics_key = None;
        self.mark_dirty();
        self.on_started(info);
    }

    fn on_engine(&mut self, event: EngineEvent) -> bool {
        match event {
            EngineEvent::Started { token, info } => {
                if token == self.play_token {
                    self.on_started(info);
                } else if let Some(p) = self.preload.take_if(|p| p.token == token) {
                    self.on_gapless_switch(token, &p.id, info);
                } else {
                    return false;
                }
            }
            EngineEvent::Ended { token } => {
                if token != self.play_token {
                    return false;
                }
                if self.preload.is_some() {
                    // `Started` for the preload follows immediately.
                    self.awaiting_switch = true;
                    return false;
                }
                self.on_track_end();
            }
            EngineEvent::Error { token, message } => {
                if token == self.play_token {
                    self.on_track_error(message);
                } else if self.preload.take_if(|p| p.token == token).is_some()
                    && self.awaiting_switch
                {
                    self.awaiting_switch = false;
                    self.on_track_end();
                } else {
                    return false;
                }
            }
            EngineEvent::StreamTitle { token, title } => {
                if token != self.play_token || title.trim().is_empty() {
                    return false;
                }
                if let Some(pb) = &mut self.app.playback {
                    pb.title = title.clone();
                }
                self.stream_title = Some(title);
                self.request_lyrics();
            }
        }
        true
    }

    /// The current track could not be played: report and skip ahead, unless
    /// every queue entry failed in a row.
    fn on_track_error(&mut self, message: String) {
        self.failures += 1;
        self.app.error(message);
        self.started = false;
        self.preload = None;
        if self.failures < self.app.queue.len() && self.app.queue.next_track().is_some() {
            self.play_current();
        } else {
            self.finish_report();
            self.app.playback = None;
            self.failures = 0;
        }
        self.mark_dirty();
    }

    fn set_paused(&mut self, paused: bool) {
        if let Some(pb) = &mut self.app.playback {
            pb.paused = paused;
            self.playback.set_paused(paused);
        }
        self.progress_report();
    }

    fn seek_to(&mut self, target: Duration) {
        let Some(pb) = &mut self.app.playback else {
            return;
        };
        let target = pb.duration.map_or(target, |d| target.min(d));
        match self.playback.seek(target) {
            Ok(()) => pb.elapsed = target,
            Err(e) => self.app.error(format!("{e:#}")),
        }
    }

    fn stop(&mut self) {
        self.finish_report();
        self.playback.stop();
        self.app.playback = None;
        self.started = false;
        self.preload = None;
        self.awaiting_switch = false;
        self.stream_title = None;
    }

    /// The current track ended on its own: advance per repeat/shuffle.
    fn on_track_end(&mut self) {
        if self.app.playback.is_none() {
            return;
        }
        if self.app.queue.advance_auto().is_some() {
            self.play_current();
        } else {
            self.stop();
            self.app.info("End of playlist");
        }
        self.mark_dirty();
    }

    /// Read the engine clock; returns whether anything visible changed.
    fn sync_position(&mut self) -> bool {
        let pos = self.playback.position();
        let Some(pb) = &mut self.app.playback else {
            return false;
        };
        if pb.paused || !self.started {
            return false;
        }
        let changed = pos.as_secs() != pb.elapsed.as_secs()
            || matches!(&self.app.lyrics, LyricsStatus::Ready(l)
                if self.app.show_lyrics
                    && l.line_index_at(pos) != l.line_index_at(pb.elapsed));
        pb.elapsed = pos;
        self.progress_report();
        changed
    }

    /// The single dispatcher every key, mouse hit and skin button goes through.
    pub fn apply(&mut self, cmd: Command) {
        match cmd {
            Command::Play => match self.app.play_status() {
                PlayStatus::Paused => self.set_paused(false),
                PlayStatus::Playing => self.seek_to(Duration::ZERO),
                PlayStatus::Stopped => {
                    if self.app.queue.is_empty() {
                        match self.app.selected_index() {
                            Some(i) => {
                                let items = self.app.results.clone();
                                self.app.queue.replace(items, Some(i));
                            }
                            None => {
                                self.app.info("Queue is empty — press Enter on a track");
                                return;
                            }
                        }
                    } else if self.app.queue.current_index().is_none() {
                        self.app.queue.jump(self.app.queue.cursor());
                    }
                    self.play_current();
                }
            },
            Command::Pause => {
                if self.app.play_status() == PlayStatus::Playing {
                    self.set_paused(true);
                }
            }
            Command::TogglePause => match self.app.play_status() {
                PlayStatus::Playing => self.set_paused(true),
                PlayStatus::Paused => self.set_paused(false),
                PlayStatus::Stopped => self.apply(Command::Play),
            },
            Command::Stop => {
                self.stop();
                self.mark_dirty();
            }
            Command::Next => {
                if self.app.queue.next_track().is_some() {
                    self.play_current();
                } else {
                    self.app.info("End of playlist");
                }
            }
            Command::Prev => {
                let elapsed = self
                    .app
                    .playback
                    .as_ref()
                    .map_or(0, |pb| pb.elapsed.as_secs());
                if elapsed > PREV_RESTART_SECS {
                    self.seek_to(Duration::ZERO);
                } else if self.app.queue.prev_track().is_some() {
                    self.play_current();
                } else {
                    self.seek_to(Duration::ZERO);
                }
            }
            Command::SeekTo(pos) => self.seek_to(pos),
            Command::SeekBy(delta) => {
                if let Some(pb) = &self.app.playback {
                    let target = pb.elapsed.as_secs().saturating_add_signed(delta);
                    self.seek_to(Duration::from_secs(target));
                }
            }
            Command::SeekFrac(frac) => {
                if let Some(d) = self.app.playback.as_ref().and_then(|pb| pb.duration) {
                    self.seek_to(d.mul_f64(f64::from(frac.clamp(0.0, 1.0))));
                }
            }
            Command::SetVolume(v) => self.set_volume(v),
            Command::VolumeBy(delta) => self.set_volume(self.app.volume + delta),
            Command::SetBalance(b) => {
                self.app.balance = if b.is_finite() {
                    b.clamp(-1.0, 1.0)
                } else {
                    0.0
                };
                self.playback.set_balance(self.app.balance);
                let pct = (self.app.balance * 100.0).round();
                self.app.info(match pct {
                    p if p < 0.0 => format!("Balance {}% left", -p),
                    p if p > 0.0 => format!("Balance {p}% right"),
                    _ => "Balance center".to_string(),
                });
                self.mark_dirty();
            }
            Command::ToggleShuffle => {
                let on = self.app.queue.toggle_shuffle();
                self.app.info(if on { "Shuffle on" } else { "Shuffle off" });
                self.mark_dirty();
            }
            Command::CycleRepeat => {
                let mode = self.app.queue.cycle_repeat();
                self.app.info(format!("Repeat: {mode:?}"));
                self.mark_dirty();
            }
            Command::SetRepeat(mode) => {
                self.app.queue.set_repeat(mode);
                self.mark_dirty();
            }
            Command::ToggleEq => {
                self.app.eq.enabled = !self.app.eq.enabled;
                self.app.info(if self.app.eq.enabled {
                    "EQ on"
                } else {
                    "EQ off"
                });
                self.eq_changed();
            }
            Command::ToggleEqAuto => {
                self.app.eq.auto = !self.app.eq.auto;
                self.mark_dirty();
            }
            Command::SetEqBand { band, db } => {
                if band < EQ_BANDS && db.is_finite() {
                    self.app.eq.bands_db[band] = db.clamp(-EQ_MAX_DB, EQ_MAX_DB);
                    self.eq_changed();
                }
            }
            Command::SetPreamp(db) => {
                if db.is_finite() {
                    self.app.eq.preamp_db = db.clamp(-EQ_MAX_DB, EQ_MAX_DB);
                    self.eq_changed();
                }
            }
            Command::EqPreset(name) => {
                if eq_presets::apply(&mut self.app.eq, &name) {
                    self.app.info(format!("EQ preset: {name}"));
                    self.eq_changed();
                } else {
                    self.app.error(format!("Unknown EQ preset '{name}'"));
                }
            }
            Command::PlayIndex(i) => {
                if self.app.queue.jump(i).is_some() {
                    self.app.queue.set_cursor(i);
                    self.play_current();
                }
            }
            Command::Select(i) => self.app.queue.set_cursor(i),
            Command::Eject => self.app.open_prompt(InputPurpose::Open),
            Command::ToggleEqWindow | Command::TogglePlaylistWindow if self.app.is_winamp() => {
                self.winamp_ui.apply(&cmd);
            }
            Command::ToggleEqWindow => self.app.show_eq = !self.app.show_eq,
            Command::TogglePlaylistWindow => self.app.show_playlist = !self.app.show_playlist,
            Command::CycleVis => {
                if self.app.is_winamp() {
                    self.winamp_ui.vis_mode = self.winamp_ui.vis_mode.next();
                } else {
                    self.app.vis_mode = self.app.vis_mode.next();
                    self.app.info(format!("Visualizer: {}", self.app.vis_mode));
                    self.mark_dirty();
                }
            }
            Command::ToggleLyrics => {
                self.app.show_lyrics = !self.app.show_lyrics;
                if !self.app.config.providers.lyrics.enabled {
                    self.app
                        .info("Lyrics are disabled ([providers.lyrics] enabled = false)");
                } else if self.app.show_lyrics && self.app.is_winamp() {
                    self.app.info("Lyrics show in the Dark and Light themes");
                }
                if self.app.show_lyrics {
                    self.lyrics_key = None;
                    self.request_lyrics();
                }
            }
            Command::Quit => self.app.should_quit = true,
        }
    }

    fn eq_changed(&mut self) {
        self.playback.set_eq(self.app.eq);
        self.mark_dirty();
    }

    fn set_volume(&mut self, v: f32) {
        self.app.volume = if v.is_finite() {
            v.clamp(0.0, 1.0)
        } else {
            1.0
        };
        self.playback.set_volume(self.app.volume);
        self.app
            .info(format!("Volume {:.0}%", self.app.volume * 100.0));
        self.mark_dirty();
    }

    // ── input ────────────────────────────────────────────────────────────

    fn on_key(&mut self, key: KeyEvent) {
        match self.app.mode {
            Mode::Browse => {
                let action = self.app.handle_browse_key(key);
                self.on_browse_action(action);
            }
            Mode::Input => match self.app.handle_input_key(key) {
                InputAction::None => {}
                InputAction::Submit(purpose, text) => self.on_submit(purpose, text),
                InputAction::Colon(cmd) => self.on_colon(&cmd),
            },
            Mode::Settings => {
                let action = self.app.handle_settings_key(key);
                self.on_settings_action(action);
            }
            Mode::SkinBrowser => {
                let action = self.app.handle_skin_browser_key(key);
                self.on_skin_browser_action(action);
            }
            Mode::Onboarding => {
                let action = self.app.handle_onboarding_key(key);
                self.on_onboarding_action(action);
            }
        }
    }

    fn on_mouse(&mut self, mouse: MouseEvent) {
        if self.app.mode != Mode::Browse {
            return;
        }
        if self.app.is_winamp() {
            let view = self.app.player_view();
            if let Some(cmd) = self.winamp_ui.on_mouse(mouse, &view) {
                self.apply(cmd);
            }
            return;
        }
        match self.app.handle_mouse(mouse) {
            MouseAction::None => {}
            MouseAction::Cmd(cmd) => self.apply(cmd),
            MouseAction::Browse(outcome) => self.on_browse_outcome(outcome),
            MouseAction::PlayResult(i) => self.on_browse_action(BrowseAction::PlayResult(i)),
            MouseAction::PrevPage => self.on_browse_action(BrowseAction::PrevPage),
            MouseAction::NextPage => self.on_browse_action(BrowseAction::NextPage),
        }
    }

    fn on_browse_action(&mut self, action: BrowseAction) {
        match action {
            BrowseAction::None => {}
            BrowseAction::Cmd(cmd) => self.apply(cmd),
            BrowseAction::Browse(outcome) => self.on_browse_outcome(outcome),
            BrowseAction::PlayResult(i) => {
                if i < self.app.results.len() {
                    let items = self.app.results.clone();
                    self.app.queue.replace(items, Some(i));
                    self.play_current();
                }
            }
            BrowseAction::Enqueue(i) => {
                if let Some(item) = self.app.results.get(i).cloned() {
                    self.enqueue(item);
                }
            }
            BrowseAction::PlayNext(i) => {
                if let Some(item) = self.app.results.get(i).cloned() {
                    self.play_next(item);
                }
            }
            BrowseAction::RemoveFromQueue(i) => {
                if let Some(item) = self.app.queue.remove(i) {
                    self.app.info(format!("Removed: {}", item.title));
                    self.mark_dirty();
                }
            }
            BrowseAction::MoveInQueue { from, to } => {
                self.app.queue.move_item(from, to);
                self.app.queue.set_cursor(to);
                self.mark_dirty();
            }
            BrowseAction::NextPage => {
                if !self.app.results.is_empty() {
                    self.app.page += 1;
                    self.dispatch_search();
                }
            }
            BrowseAction::PrevPage => {
                if self.app.page > 0 {
                    self.app.page -= 1;
                    self.dispatch_search();
                }
            }
            BrowseAction::OpenSettings => self.app.mode = Mode::Settings,
            BrowseAction::SwitchTab(forward) => self.switch_tab(forward),
            BrowseAction::Download(id, title) => {
                if self.app.deps.ytdlp == DepStatus::Missing {
                    self.app.error("yt-dlp is not installed");
                    return;
                }
                let music_dir = self.app.config.music_dir.clone();
                self.app.downloading_title = Some(title.clone());
                self.app.set_status(Status::Downloading(title.clone()));
                let tx = self.tx.clone();
                tokio::spawn(async move {
                    let result = extractor::download_audio(&id, &title, &music_dir).await;
                    let _ = tx.send(AppEvent::DownloadComplete { title, result });
                });
            }
        }
    }

    fn enqueue(&mut self, item: MediaItem) {
        let title = item.title.clone();
        self.app.queue.enqueue(item);
        self.app.info(format!(
            "Queued: {title} ({} in queue)",
            self.app.queue.len()
        ));
        self.mark_dirty();
    }

    fn play_next(&mut self, item: MediaItem) {
        let title = item.title.clone();
        self.app.queue.play_next(item);
        self.app.info(format!("Up next: {title}"));
        self.mark_dirty();
    }

    /// Act on what a provider browser asked for.
    fn on_browse_outcome(&mut self, outcome: BrowseOutcome) {
        let Tab::Provider(kind) = self.app.tab else {
            return;
        };
        match outcome {
            BrowseOutcome::None => {}
            BrowseOutcome::Fetch { token, request } => {
                let services = self.services.clone();
                let tx = self.tx.clone();
                tokio::task::spawn_blocking(move || {
                    let result = browse::fetch(&services, &request);
                    let _ = tx.send(AppEvent::BrowseLoaded {
                        kind,
                        token,
                        result,
                    });
                });
            }
            BrowseOutcome::Prompt(label) => {
                self.app.prompt_label = label;
                self.app.open_prompt(InputPurpose::ProviderSearch);
            }
            BrowseOutcome::Play { items, index } => {
                self.app.queue.replace(items, Some(index));
                self.play_current();
            }
            BrowseOutcome::Enqueue(item) => self.enqueue(item),
            BrowseOutcome::PlayNext(item) => self.play_next(item),
            BrowseOutcome::ToggleSubscription { feed_url, title } => {
                let subs = &mut self.app.config.providers.podcast.subscriptions;
                if let Some(i) = subs.iter().position(|s| *s == feed_url) {
                    subs.remove(i);
                    self.app.info(format!("Unsubscribed: {title}"));
                } else {
                    subs.push(feed_url);
                    self.app.info(format!("Subscribed: {title}"));
                }
                if let Err(e) = self.app.config.save() {
                    self.app.error(format!("Cannot save config: {e:#}"));
                }
                let subs = self.app.config.providers.podcast.subscriptions.clone();
                if let Some(browser) = self.app.browser_for(kind) {
                    let next = browser.refresh_subscriptions(&subs);
                    self.on_browse_outcome(next);
                }
            }
        }
    }

    fn switch_tab(&mut self, forward: bool) {
        let tabs = &self.app.tabs;
        let i = tabs.iter().position(|t| *t == self.app.tab).unwrap_or(0);
        let n = tabs.len();
        let next = tabs[if forward {
            (i + 1) % n
        } else {
            (i + n - 1) % n
        }];
        self.app.tab = next;
        match next {
            Tab::Local => {
                self.app.results = self.app.local_library.clone();
                if !self.app.library_scanned {
                    self.start_scan();
                }
            }
            Tab::Online => {
                self.app.results.clear();
                if self.app.deps.ytdlp == DepStatus::Missing {
                    self.app.error(
                        "yt-dlp is not installed — online search is unavailable (see `rustune doctor`)",
                    );
                }
            }
            Tab::Provider(_) => self.app.results.clear(),
        }
        self.app.page = 0;
        self.app.list_state.select(Some(0));
    }

    fn on_submit(&mut self, purpose: InputPurpose, text: String) {
        match purpose {
            InputPurpose::Search => match self.app.tab {
                Tab::Local => {
                    self.app.results = library::search(&self.app.local_library, &text);
                    self.app.list_state.select(Some(0));
                    self.app.page = 0;
                    let n = self.app.results.len();
                    if self.app.is_winamp() && n > 0 {
                        // The skin shows the queue: load the matches into it.
                        let items = self.app.results.clone();
                        self.app.queue.replace(items, None);
                        self.app.info(format!("{n} tracks in the playlist"));
                        self.mark_dirty();
                    } else {
                        self.app
                            .info(format!("{n} match{}", if n == 1 { "" } else { "es" }));
                    }
                }
                Tab::Online | Tab::Provider(_) => self.dispatch_search(),
            },
            InputPurpose::ProviderSearch => {
                if let Some(browser) = self.app.browser_mut() {
                    let outcome = browser.search(text);
                    self.on_browse_outcome(outcome);
                }
            }
            InputPurpose::SavePlaylist => {
                if self.app.queue.is_empty() {
                    self.app.error("Queue is empty — nothing to save");
                    return;
                }
                match playlist_io::save_named(
                    &config::playlists_dir(),
                    &text,
                    self.app.queue.items(),
                ) {
                    Ok(path) => self.app.info(format!(
                        "Saved {} tracks to {}",
                        self.app.queue.len(),
                        path.display()
                    )),
                    Err(e) => self.app.error(format!("{e:#}")),
                }
            }
            InputPurpose::Open => self.resolve_into_queue(vec![text]),
        }
    }

    fn on_colon(&mut self, cmd: &str) {
        match cmd.trim() {
            "clear" => {
                self.stop();
                self.app.queue.clear();
                self.resume = None;
                self.app.info("Queue cleared");
                self.mark_dirty();
            }
            "eq" => {
                let names: Vec<&str> = eq_presets::PRESETS.iter().map(|(n, _)| *n).collect();
                self.app.info(format!("EQ presets: {}", names.join(", ")));
            }
            other => match parse_colon(other) {
                Ok(command) => self.apply(command),
                Err(msg) => self.app.error(msg),
            },
        }
    }

    fn on_settings_action(&mut self, action: SettingsAction) {
        match action {
            SettingsAction::None => {}
            SettingsAction::Quit => self.save_config(),
            SettingsAction::ThemeChanged => {
                self.theme_changed();
                self.mark_dirty();
            }
            SettingsAction::OpenSkinBrowserLocal | SettingsAction::OpenSkinBrowserOnline => {
                let online = matches!(action, SettingsAction::OpenSkinBrowserOnline);
                let app = &mut self.app;
                app.mode = Mode::SkinBrowser;
                app.skin_browser_source = if online {
                    SkinBrowserSource::Online
                } else {
                    SkinBrowserSource::Local
                };
                app.skin_entries.clear();
                app.skin_list_state.select(Some(0));
                app.skin_browser_error = None;
                app.skin_browser_offset = 0;
                app.skin_browser_has_more = online;
                app.skin_downloading_md5 = None;
                app.skin_total_count = 0;
                app.skin_search_query.clear();
                app.skin_search_active = false;
                if online {
                    self.fetch_skins(0);
                } else {
                    self.list_local_skins();
                }
            }
        }
    }

    fn theme_changed(&mut self) {
        self.app.sync_analyzer();
        if self.app.is_winamp() && self.app.winamp_skin.is_none() {
            self.load_skin(self.app.skin_path.clone());
        }
    }

    fn on_skin_browser_action(&mut self, action: SkinBrowserAction) {
        match action {
            SkinBrowserAction::None | SkinBrowserAction::Back => {}
            SkinBrowserAction::Download(md5, filename) => {
                let tx = self.tx.clone();
                tokio::task::spawn_blocking(move || {
                    let result = net::download_skin(&md5, &filename, &config::skins_dir());
                    let _ = tx.send(AppEvent::SkinDownloaded { md5, result });
                });
            }
            SkinBrowserAction::LoadLocal(filename) => match Path::new(&filename).file_name() {
                Some(name) => self.load_skin(Some(config::skins_dir().join(name))),
                None => self.app.skin_browser_error = Some("Invalid skin file name".into()),
            },
            SkinBrowserAction::RequestFetch => self.fetch_skins(self.app.skin_browser_offset),
            SkinBrowserAction::Search(query) => {
                self.app.skin_search_query = query;
                self.app.skin_entries.clear();
                self.app.skin_list_state.select(Some(0));
                self.app.skin_browser_offset = 0;
                self.app.skin_browser_has_more = false;
                self.fetch_skins(0);
            }
        }
    }

    fn on_onboarding_action(&mut self, action: OnboardingAction) {
        match action {
            OnboardingAction::None | OnboardingAction::Next => {}
            OnboardingAction::SetMusicDir(dir) => {
                let path = config::expand_tilde(&dir);
                if path.is_dir() {
                    self.app.config.music_dir = path;
                } else {
                    self.app.error(format!(
                        "{} not found; keeping {}",
                        path.display(),
                        self.app.config.music_dir.display()
                    ));
                }
            }
            OnboardingAction::SelectTheme => {
                self.save_config();
                self.start_scan();
                self.theme_changed();
                self.mark_dirty();
            }
        }
    }

    // ── events ───────────────────────────────────────────────────────────

    /// Handle `first` plus everything already queued, coalescing bursts.
    /// Returns whether a redraw is needed.
    pub fn drain(&mut self, first: AppEvent, rx: &mut UnboundedReceiver<AppEvent>) -> bool {
        let mut redraw = false;
        let mut pending = Some(first);
        while let Some(event) = pending.take() {
            let next = rx.try_recv().ok();
            if next.as_ref().is_some_and(|n| supersedes(&event, n)) {
                pending = next;
                continue;
            }
            redraw |= self.handle_event(event);
            pending = next;
        }
        // Whatever changed the queue (keys, clicks, a gapless switch), the
        // engine should be decoding what plays next.
        self.refresh_preload();
        redraw
    }

    pub fn handle_event(&mut self, event: AppEvent) -> bool {
        match event {
            AppEvent::Term(Event::Key(key)) => self.on_key(key),
            AppEvent::Term(Event::Mouse(mouse)) => self.on_mouse(mouse),
            AppEvent::Term(Event::Paste(text)) => {
                if matches!(self.app.mode, Mode::Input) {
                    self.app.insert_text(&text);
                }
            }
            AppEvent::Term(Event::Resize(..)) => {}
            AppEvent::Term(Event::FocusGained | Event::FocusLost) => return false,
            AppEvent::Engine(event) => return self.on_engine(event),
            AppEvent::ItemsReady { token, results } => {
                if token != self.search_token {
                    return false;
                }
                match results {
                    Ok(items) => {
                        self.app.set_status(if items.is_empty() {
                            Status::Info("No results".into())
                        } else {
                            Status::Idle
                        });
                        self.app.results = items;
                        self.app.list_state.select(Some(0));
                    }
                    Err(e) => self.app.error(format!("Search failed: {e:#}")),
                }
            }
            AppEvent::StreamReady { token, result } => {
                if token != self.play_token || self.app.playback.is_none() {
                    return false;
                }
                match result {
                    Ok(info) => {
                        if let Some(pb) = &mut self.app.playback {
                            if !info.title.is_empty() {
                                pb.title = info.title;
                            }
                        }
                        self.start_engine(Source::Url(info.url));
                    }
                    Err(e) => self.on_track_error(format!("{e:#}")),
                }
            }
            AppEvent::LyricsReady { key, result } => {
                let lyrics = match result {
                    Ok(lyrics) => lyrics.filter(|l| !l.is_empty() || l.instrumental),
                    Err(e) => {
                        if self.lyrics_key.as_deref() == Some(key.as_str()) {
                            self.app.lyrics = LyricsStatus::Error(e);
                            return true;
                        }
                        return false;
                    }
                };
                let current = self.lyrics_key.as_deref() == Some(key.as_str());
                if current {
                    self.app.lyrics = match &lyrics {
                        Some(l) => LyricsStatus::Ready(l.clone()),
                        None => LyricsStatus::NotFound,
                    };
                }
                self.lyrics_cache.insert(key, lyrics);
                return current;
            }
            AppEvent::BrowseLoaded {
                kind,
                token,
                result,
            } => {
                if let Some(browser) = self.app.browser_for(kind) {
                    browser.loaded(token, result);
                }
                if kind == ProviderKind::Jellyfin {
                    if let Some((token, user_id)) = self.services.jellyfin_session() {
                        if let Some(jf) = self.app.config.providers.jellyfin.as_mut() {
                            jf.token = Some(token);
                            jf.user_id = Some(user_id);
                            self.save_config();
                        }
                    }
                }
            }
            AppEvent::DepsProbed(deps) => self.app.deps = deps,
            AppEvent::ScanComplete(result) => {
                self.app.library_scanned = true;
                match result {
                    Ok(items) => {
                        let n = items.len();
                        self.app.local_library = items;
                        if self.app.tab == Tab::Local {
                            self.app.results = self.app.local_library.clone();
                            self.app.list_state.select(Some(0));
                        }
                        self.app.info(format!("Library: {n} tracks"));
                    }
                    Err(e) => self.app.error(format!("Scan failed: {e:#}")),
                }
            }
            AppEvent::QueueResolved { items, errors } => {
                if !errors.is_empty() {
                    self.app.error(errors.join("; "));
                } else if matches!(self.app.status, Status::Loading(_)) {
                    self.app.set_status(Status::Idle);
                }
                if !items.is_empty() {
                    self.resume = None;
                    self.app.queue.replace(items, Some(0));
                    self.play_current();
                }
            }
            AppEvent::SkinListFetched {
                token,
                page,
                requested_offset,
            } => {
                if token != self.skin_fetch_token {
                    return false;
                }
                let app = &mut self.app;
                app.skin_browser_loading = false;
                match page {
                    Ok(page) => {
                        if requested_offset == 0 {
                            app.skin_entries = page.entries;
                        } else {
                            app.skin_entries.extend(page.entries);
                        }
                        app.skin_browser_offset = page.next_offset;
                        app.skin_browser_has_more = page.has_more;
                        app.skin_total_count = page.total.unwrap_or(app.skin_entries.len());
                        if app.skin_list_state.selected().is_none() && !app.skin_entries.is_empty()
                        {
                            app.skin_list_state.select(Some(0));
                        }
                    }
                    Err(e) => app.skin_browser_error = Some(format!("{e:#}")),
                }
            }
            AppEvent::LocalSkinsListed(entries) => {
                self.app.skin_browser_loading = false;
                self.app.skin_total_count = entries.len();
                self.app.skin_entries = entries;
                self.app.skin_list_state.select(Some(0));
            }
            AppEvent::SkinDownloaded { md5, result } => {
                self.app.skin_downloading_md5 = None;
                match result {
                    Ok(path) => {
                        for entry in &mut self.app.skin_entries {
                            if entry.md5 == md5 {
                                entry.is_local = true;
                            }
                        }
                        self.load_skin(Some(path));
                    }
                    Err(e) => self.app.skin_browser_error = Some(format!("Download failed: {e:#}")),
                }
            }
            AppEvent::SkinLoaded { path, result } => match result {
                Ok(skin) => {
                    self.app.winamp_skin = Some(*skin);
                    self.app.skin_path = path;
                    self.gfx.invalidate();
                    self.mark_dirty();
                }
                Err(e) => {
                    let msg = format!("Failed to load skin: {e:#}");
                    if self.app.mode == Mode::SkinBrowser {
                        self.app.skin_browser_error = Some(msg);
                    } else {
                        self.app.error(msg);
                    }
                    if self.app.winamp_skin.is_none() {
                        self.app.winamp_skin = Some(WinampSkin::default_skin());
                    }
                }
            },
            AppEvent::DefaultSkinReady(result) => match result {
                Ok(Some(path)) => {
                    if self.app.is_winamp() && self.app.skin_path.is_none() {
                        self.load_skin(Some(path));
                    }
                }
                Ok(None) => return false,
                Err(e) => {
                    if !self.app.is_winamp() {
                        return false;
                    }
                    self.app.info(format!("Default skin unavailable: {e:#}"));
                }
            },
            AppEvent::DownloadComplete { title, result } => {
                self.app.downloading_title = None;
                match result {
                    Ok(()) => {
                        self.app.info(format!("Downloaded: {title}"));
                        self.start_scan();
                    }
                    Err(e) => self.app.error(format!("Download failed ({title}): {e:#}")),
                }
            }
            AppEvent::StateSaved(result) => {
                self.saving = false;
                if let Err(e) = result {
                    self.app.error(format!("Cannot save session: {e:#}"));
                } else {
                    return false;
                }
            }
        }
        true
    }

    // ── clock ────────────────────────────────────────────────────────────

    fn vis_visible(&self) -> bool {
        ui::vis_visible(&self.app, &self.winamp_ui, self.screen_height)
    }

    /// When the loop must wake up without input: status expiry, the
    /// playback clock, the visualizer pacer, skin animation and pending
    /// image updates. `None` = sleep until an event arrives.
    fn next_wakeup(&self) -> Option<Duration> {
        let playing = self.app.play_status() == PlayStatus::Playing && self.started;
        let mut wake = self.app.wants_tick();
        let mut merge = |d: Option<Duration>| {
            wake = match (wake, d) {
                (Some(a), Some(b)) => Some(a.min(b)),
                (a, b) => a.or(b),
            };
        };
        if playing {
            let ms = 1000 - u64::from(self.playback.position().subsec_millis());
            merge(Some(Duration::from_millis(ms + 5)));
            if self.app.show_lyrics
                && matches!(&self.app.lyrics, LyricsStatus::Ready(l) if l.is_synced())
            {
                merge(Some(LYRICS_TICK));
            }
        }
        merge(self.pacer.timeout(
            self.vis_at.elapsed(),
            self.vis_visible(),
            playing,
            self.app.analyzer.is_idle(),
        ));
        if self.app.is_winamp() && matches!(self.app.mode, Mode::Browse | Mode::Input) {
            merge(self.winamp_ui.wants_tick(&self.app.player_view()));
            merge(self.gfx.next_flush_in());
        }
        wake
    }

    /// Time-driven updates; returns whether a redraw is needed.
    fn on_tick(&mut self) -> bool {
        let mut redraw = self.app.on_tick();
        redraw |= self.sync_position();
        let playing = self.app.play_status() == PlayStatus::Playing && self.started;
        let due = self.pacer.timeout(
            self.vis_at.elapsed(),
            self.vis_visible(),
            playing,
            self.app.analyzer.is_idle(),
        );
        if due == Some(Duration::ZERO) {
            let now = Instant::now();
            let dt = now - self.vis_at;
            self.vis_at = now;
            redraw |= self.app.analyzer.update(self.playback.tap(), dt, playing);
        }
        if self.app.is_winamp() && matches!(self.app.mode, Mode::Browse | Mode::Input) {
            let view = self.app.player_view();
            redraw |= self.winamp_ui.wants_tick(&view).is_some()
                || self.gfx.next_flush_in() == Some(Duration::ZERO);
        }
        redraw
    }

    /// Suspend the TUI, run a dependency installer, then resume.
    async fn install(&mut self, dep: Dep, input: &InputThread) -> Result<DefaultTerminal> {
        input.pause();
        let _ = crossterm::execute!(std::io::stdout(), DisableMouseCapture);
        ratatui::restore();
        let ok = tokio::task::spawn_blocking(move || install::run_install(dep))
            .await
            .unwrap_or(false);
        let terminal = ratatui::init();
        crossterm::execute!(std::io::stdout(), EnableMouseCapture)?;
        input.resume();
        self.gfx.invalidate();
        if ok {
            self.app.info(format!("{} installed", dep.binary()));
        } else {
            self.app
                .error(format!("{} was not installed", dep.binary()));
        }
        let tx = self.tx.clone();
        tokio::spawn(async move {
            let _ = tx.send(AppEvent::DepsProbed(install::probe_deps().await));
        });
        Ok(terminal)
    }
}

/// Redraw-on-change loop: sleeps until input, a background event, a clock
/// or visualizer tick, or the autosave deadline. Idle means fully asleep.
pub async fn run(
    mut terminal: DefaultTerminal,
    ctl: &mut Controller,
    mut rx: UnboundedReceiver<AppEvent>,
    input: &InputThread,
) -> Result<()> {
    let mut redraw = true;
    // Skin images are on screen and must be deleted when the view changes.
    let mut images = false;
    loop {
        if redraw {
            let mut winamp = false;
            let mut height = 0;
            terminal.draw(|frame| {
                height = frame.area().height;
                winamp = ui::draw(frame, &mut ctl.app, &mut ctl.winamp_ui, &mut ctl.gfx);
            })?;
            ctl.screen_height = height;
            if winamp {
                ctl.gfx.flush(terminal.backend_mut())?;
            } else if images {
                ctl.gfx.clear(terminal.backend_mut())?;
            }
            images = winamp;
            redraw = false;
        }
        if ctl.app.should_quit {
            if images {
                ctl.gfx.clear(terminal.backend_mut())?;
            }
            return Ok(());
        }
        if let Some(dep) = ctl.app.pending_install.take() {
            if images {
                ctl.gfx.clear(terminal.backend_mut())?;
                images = false;
            }
            terminal = ctl.install(dep, input).await?;
            redraw = true;
            continue;
        }

        let tick = ctl.next_wakeup();
        let save_at = ctl.save_deadline();
        tokio::select! {
            event = rx.recv() => {
                let Some(event) = event else { return Ok(()) };
                redraw = ctl.drain(event, &mut rx);
            }
            () = tokio::time::sleep(tick.unwrap_or_default()), if tick.is_some() => {
                redraw = ctl.on_tick();
            }
            () = tokio::time::sleep_until(save_at.unwrap_or_else(Instant::now)), if save_at.is_some() => {
                ctl.flush_state();
            }
        }
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    use crossterm::event::{KeyModifiers, MouseButton};

    use crate::gfx::Protocol;
    use crate::playback::Output;
    use crate::view::EqSettings;

    /// Records engine calls; position is settable.
    #[derive(Clone, Default)]
    struct Recorder {
        calls: Arc<Mutex<Vec<String>>>,
        position: Arc<Mutex<Duration>>,
    }

    impl Recorder {
        fn log(&self, s: String) {
            self.calls.lock().unwrap().push(s);
        }
        fn take(&self) -> Vec<String> {
            std::mem::take(&mut *self.calls.lock().unwrap())
        }
    }

    fn src_name(src: &Source) -> String {
        match src {
            Source::File(p) => p.display().to_string(),
            Source::Url(u) => u.clone(),
        }
    }

    impl Output for Recorder {
        fn play(&self, src: Source, token: u64) {
            self.log(format!("play {} #{token}", src_name(&src)));
        }
        fn preload(&self, src: Source, token: u64) {
            self.log(format!("preload {} #{token}", src_name(&src)));
        }
        fn cancel_preload(&self) {
            self.log("cancel".into());
        }
        fn pause(&self) {
            self.log("pause".into());
        }
        fn resume(&self) {
            self.log("resume".into());
        }
        fn stop(&self) {
            self.log("stop".into());
        }
        fn seek(&self, pos: Duration) -> Result<()> {
            self.log(format!("seek {}", pos.as_secs()));
            Ok(())
        }
        fn position(&self) -> Duration {
            *self.position.lock().unwrap()
        }
        fn set_volume(&self, v: f32) {
            self.log(format!("volume {v:.2}"));
        }
        fn set_balance(&self, b: f32) {
            self.log(format!("balance {b:.2}"));
        }
        fn set_eq(&self, eq: EqSettings) {
            self.log(format!("eq {} {:.1}", eq.enabled, eq.bands_db[0]));
        }
        fn set_replaygain(&self, mode: crate::audio::ReplayGainMode) {
            self.log(format!("replaygain {mode:?}"));
        }
    }

    fn test_config() -> Config {
        Config {
            onboarding_done: true,
            ..Config::default()
        }
    }

    fn gfx() -> GfxPainter {
        GfxPainter::with_cell_size(Protocol::HalfBlock, None)
    }

    fn controller() -> (Controller, UnboundedReceiver<AppEvent>) {
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
        let playback = Playback::unavailable("No audio output: test");
        let ctl = Controller::new(test_config(), SessionState::default(), tx, playback, gfx());
        (ctl, rx)
    }

    fn with_engine() -> (Controller, Recorder, UnboundedReceiver<AppEvent>) {
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
        let rec = Recorder::default();
        let playback = Playback::with_output(Box::new(rec.clone()));
        let ctl = Controller::new(test_config(), SessionState::default(), tx, playback, gfx());
        rec.take();
        (ctl, rec, rx)
    }

    fn url_item(n: usize) -> MediaItem {
        MediaItem {
            id: format!("https://example.invalid/{n}"),
            title: format!("t{n}"),
            artist: None,
            album: None,
            duration: Some(100),
            subtitle: None,
            source: SourceKind::Url,
        }
    }

    fn started(token: u64) -> AppEvent {
        AppEvent::Engine(EngineEvent::Started {
            token,
            info: TrackInfo {
                duration: Some(Duration::from_secs(90)),
                format: StreamFormat {
                    kbps: Some(320),
                    sample_rate: Some(44_100),
                    channels: Some(2),
                },
                seekable: true,
                replaygain_db: None,
            },
        })
    }

    fn ended(token: u64) -> AppEvent {
        AppEvent::Engine(EngineEvent::Ended { token })
    }

    fn playing(elapsed: u64) -> PlaybackState {
        PlaybackState {
            title: "t".into(),
            duration: Some(Duration::from_secs(100)),
            elapsed: Duration::from_secs(elapsed),
            paused: false,
            format: StreamFormat::default(),
        }
    }

    #[test]
    fn missing_output_keeps_the_ui_usable() {
        let (mut ctl, _rx) = controller();
        assert!(matches!(&ctl.app.status, Status::Error(m) if m.contains("No audio output")));
        ctl.app.queue.replace((0..3).map(url_item).collect(), None);
        ctl.apply(Command::PlayIndex(1));
        assert_eq!(ctl.app.queue.current_index(), Some(1));
        assert_eq!(ctl.app.play_status(), PlayStatus::Stopped);
        ctl.apply(Command::VolumeBy(-0.5));
        assert_eq!(ctl.app.volume, 0.5);
    }

    #[test]
    fn settings_reach_the_engine_and_mark_dirty() {
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
        let rec = Recorder::default();
        let state = SessionState {
            volume: 0.4,
            balance: -0.5,
            ..SessionState::default()
        };
        let playback = Playback::with_output(Box::new(rec.clone()));
        let mut ctl = Controller::new(test_config(), state, tx, playback, gfx());
        assert_eq!(
            rec.take(),
            [
                "volume 0.40",
                "balance -0.50",
                "eq false 0.0",
                "replaygain Off"
            ],
            "state.toml settings restored at startup"
        );
        ctl.apply(Command::VolumeBy(0.9));
        assert_eq!(ctl.app.volume, 1.0);
        ctl.apply(Command::SetBalance(0.3));
        ctl.apply(Command::EqPreset("Rock".into()));
        assert_eq!(ctl.app.eq.bands_db[0], 8.0);
        ctl.apply(Command::ToggleEq);
        ctl.apply(Command::SetEqBand { band: 0, db: 40.0 });
        assert_eq!(ctl.app.eq.bands_db[0], EQ_MAX_DB);
        ctl.apply(Command::SetEqBand { band: 99, db: 1.0 });
        assert_eq!(
            rec.take(),
            [
                "volume 1.00",
                "balance 0.30",
                "eq false 8.0",
                "eq true 8.0",
                "eq true 12.0"
            ]
        );
        ctl.apply(Command::CycleRepeat);
        ctl.apply(Command::ToggleShuffle);
        ctl.apply(Command::CycleVis);
        assert!(ctl.save_deadline().is_some());
        let state = ctl.session_state();
        assert!(state.shuffle);
        assert_eq!(state.repeat, RepeatMode::All);
        assert_eq!(state.vis, crate::vis::VisMode::Braille);
    }

    #[test]
    fn gapless_preload_follows_the_queue() {
        let (mut ctl, rec, mut rx) = with_engine();
        ctl.app.queue.replace((0..3).map(url_item).collect(), None);
        ctl.apply(Command::PlayIndex(0));
        let token = ctl.play_token;
        assert_eq!(
            rec.take(),
            [format!("play https://example.invalid/0 #{token}")]
        );
        // Nothing is preloaded before the track actually starts.
        ctl.refresh_preload();
        assert!(rec.take().is_empty());

        assert!(ctl.drain(started(token), &mut rx));
        let pb = ctl.app.playback.as_ref().unwrap();
        assert_eq!(pb.duration, Some(Duration::from_secs(90)));
        assert_eq!(pb.format.kbps, Some(320));
        let calls = rec.take();
        assert_eq!(calls.len(), 1);
        assert!(calls[0].starts_with("preload https://example.invalid/1 #"));
        let pre = ctl.preload.as_ref().unwrap().token;

        // Playing item 2 next changes what follows: the preload is replaced.
        ctl.on_browse_action(BrowseAction::Cmd(Command::SetRepeat(RepeatMode::Off)));
        ctl.app.queue.move_item(2, 1);
        ctl.refresh_preload();
        let calls = rec.take();
        assert!(
            calls[0].starts_with("preload https://example.invalid/2 #"),
            "{calls:?}"
        );
        let pre2 = ctl.preload.as_ref().unwrap().token;
        assert_ne!(pre, pre2);

        // End of track: the engine switches by itself; the queue follows.
        assert!(!ctl.drain(ended(token), &mut rx));
        assert_eq!(ctl.app.queue.current_index(), Some(0));
        assert!(ctl.drain(started(pre2), &mut rx));
        assert_eq!(
            ctl.app.queue.current().unwrap().id,
            "https://example.invalid/2"
        );
        assert_eq!(ctl.play_token, pre2);
        assert!(
            rec.take().iter().all(|c| !c.starts_with("play ")),
            "no restart"
        );
    }

    #[test]
    fn preload_is_cancelled_when_nothing_follows() {
        let (mut ctl, rec, mut rx) = with_engine();
        ctl.app.queue.replace((0..2).map(url_item).collect(), None);
        ctl.apply(Command::PlayIndex(0));
        ctl.drain(started(ctl.play_token), &mut rx);
        rec.take();
        ctl.app.queue.remove(1);
        ctl.refresh_preload();
        assert_eq!(rec.take(), ["cancel"]);
        assert!(ctl.preload.is_none());
        // Without a preload the end of the track stops at the queue end.
        ctl.drain(ended(ctl.play_token), &mut rx);
        assert_eq!(ctl.app.play_status(), PlayStatus::Stopped);
    }

    #[test]
    fn track_end_without_preload_advances_or_wraps() {
        let (mut ctl, rec, mut rx) = with_engine();
        ctl.app.queue.replace((0..2).map(url_item).collect(), None);
        ctl.app.queue.set_repeat(RepeatMode::All);
        ctl.apply(Command::PlayIndex(1));
        let token = ctl.play_token;
        rec.take();
        ctl.drain(ended(token), &mut rx);
        assert_eq!(ctl.app.queue.current_index(), Some(0), "repeat all wraps");
        assert_eq!(
            rec.take()[0],
            format!("play https://example.invalid/0 #{}", ctl.play_token)
        );
        // Stale events of the replaced track are ignored.
        assert!(!ctl.handle_event(ended(token)));
        assert_eq!(ctl.app.queue.current_index(), Some(0));
    }

    #[test]
    fn engine_errors_skip_but_never_loop_forever() {
        let (mut ctl, _rec, mut rx) = with_engine();
        ctl.app.queue.replace((0..2).map(url_item).collect(), None);
        ctl.app.queue.set_repeat(RepeatMode::All);
        ctl.apply(Command::PlayIndex(0));
        let err = |token| {
            AppEvent::Engine(EngineEvent::Error {
                token,
                message: "boom".into(),
            })
        };
        ctl.drain(err(ctl.play_token), &mut rx);
        assert_eq!(ctl.app.queue.current_index(), Some(1));
        assert!(ctl.app.playback.is_some());
        ctl.drain(err(ctl.play_token), &mut rx);
        assert!(ctl.app.playback.is_none(), "every entry failed: stop");
        assert!(matches!(&ctl.app.status, Status::Error(m) if m == "boom"));
    }

    #[test]
    fn stream_title_updates_now_playing() {
        let (mut ctl, _rec, mut rx) = with_engine();
        ctl.app.queue.replace(vec![url_item(0)], None);
        ctl.apply(Command::PlayIndex(0));
        let token = ctl.play_token;
        ctl.drain(started(token), &mut rx);
        assert!(ctl.drain(
            AppEvent::Engine(EngineEvent::StreamTitle {
                token,
                title: "Artist - Song".into(),
            }),
            &mut rx
        ));
        assert_eq!(ctl.app.player_view().title, "Artist - Song");
    }

    #[test]
    fn clock_follows_engine_position_and_pause() {
        let (mut ctl, rec, mut rx) = with_engine();
        ctl.app.queue.replace(vec![url_item(0)], None);
        ctl.apply(Command::PlayIndex(0));
        ctl.drain(started(ctl.play_token), &mut rx);
        *rec.position.lock().unwrap() = Duration::from_millis(2_500);
        assert!(ctl.sync_position());
        assert!(!ctl.sync_position(), "same second: no redraw");
        assert_eq!(ctl.app.player_view().elapsed, Duration::from_millis(2_500));
        assert!(ctl.next_wakeup().is_some(), "ticks while playing");
        rec.take();
        ctl.apply(Command::TogglePause);
        assert_eq!(rec.take(), ["pause"]);
        assert_eq!(ctl.next_wakeup(), None, "fully idle when paused");
        ctl.apply(Command::SeekBy(10));
        assert_eq!(rec.take(), ["seek 12"]);
        ctl.apply(Command::SeekFrac(0.5));
        assert_eq!(rec.take(), ["seek 45"]);
    }

    #[test]
    fn next_prev_walk_the_queue() {
        let (mut ctl, _rx) = controller();
        ctl.app.queue.replace((0..3).map(url_item).collect(), None);
        ctl.apply(Command::Next);
        assert_eq!(ctl.app.queue.current_index(), Some(0));
        ctl.apply(Command::Next);
        ctl.apply(Command::Next);
        assert_eq!(ctl.app.queue.current_index(), Some(2));
        ctl.apply(Command::Next);
        assert_eq!(ctl.app.queue.current_index(), Some(2), "repeat off stops");
        ctl.apply(Command::Prev);
        assert_eq!(ctl.app.queue.current_index(), Some(1));
    }

    #[test]
    fn prev_restarts_after_three_seconds() {
        let (mut ctl, rec, _rx) = with_engine();
        ctl.app.queue.replace((0..3).map(url_item).collect(), None);
        ctl.app.queue.jump(1);
        ctl.app.playback = Some(playing(10));
        ctl.apply(Command::Prev);
        assert_eq!(ctl.app.queue.current_index(), Some(1));
        assert_eq!(rec.take(), ["seek 0"]);
    }

    #[test]
    fn seek_commands_clamp_to_track() {
        let (mut ctl, _rec, _rx) = with_engine();
        ctl.app.playback = Some(playing(50));
        let elapsed = |c: &Controller| c.app.playback.as_ref().unwrap().elapsed.as_secs();
        ctl.apply(Command::SeekBy(-80));
        assert_eq!(elapsed(&ctl), 0);
        ctl.apply(Command::SeekBy(500));
        assert_eq!(elapsed(&ctl), 100);
        ctl.apply(Command::SeekFrac(0.25));
        assert_eq!(elapsed(&ctl), 25);
        ctl.apply(Command::SeekTo(Duration::from_secs(1000)));
        assert_eq!(elapsed(&ctl), 100);
    }

    #[test]
    fn results_actions_build_the_queue() {
        let (mut ctl, _rx) = controller();
        ctl.app.results = (0..4).map(url_item).collect();
        ctl.on_browse_action(BrowseAction::Enqueue(3));
        ctl.on_browse_action(BrowseAction::PlayNext(1));
        assert_eq!(ctl.app.queue.len(), 2);
        ctl.on_browse_action(BrowseAction::PlayResult(2));
        assert_eq!(ctl.app.queue.len(), 4);
        assert_eq!(ctl.app.queue.current_index(), Some(2));
        ctl.on_browse_action(BrowseAction::MoveInQueue { from: 2, to: 0 });
        assert_eq!(ctl.app.queue.current_index(), Some(0));
        ctl.on_browse_action(BrowseAction::RemoveFromQueue(3));
        assert_eq!(ctl.app.queue.len(), 3);
        ctl.on_colon("clear");
        assert!(ctl.app.queue.is_empty());
    }

    #[test]
    fn tabs_cycle_through_sources() {
        let (mut ctl, _rx) = controller();
        let n = ctl.app.tabs.len();
        assert!(n >= 4, "local, online, radio, podcasts");
        ctl.on_browse_action(BrowseAction::SwitchTab(true));
        assert_eq!(ctl.app.tab, Tab::Online);
        ctl.on_browse_action(BrowseAction::SwitchTab(true));
        assert_eq!(ctl.app.tab, Tab::Provider(ProviderKind::Radio));
        ctl.on_browse_action(BrowseAction::SwitchTab(false));
        ctl.on_browse_action(BrowseAction::SwitchTab(false));
        assert_eq!(ctl.app.tab, Tab::Local);
        ctl.on_browse_action(BrowseAction::SwitchTab(false));
        assert_eq!(ctl.app.tab, ctl.app.tabs[n - 1], "wraps backwards");
    }

    #[test]
    fn provider_play_and_subscription_outcomes() {
        let (mut ctl, _rx) = controller();
        ctl.app.tab = Tab::Provider(ProviderKind::Podcast);
        ctl.on_browse_outcome(BrowseOutcome::Play {
            items: (0..3).map(url_item).collect(),
            index: 2,
        });
        assert_eq!(ctl.app.queue.len(), 3);
        assert_eq!(ctl.app.queue.current_index(), Some(2));
        ctl.on_browse_outcome(BrowseOutcome::Prompt("Search podcasts"));
        assert_eq!(ctl.app.mode, Mode::Input);
        assert_eq!(ctl.app.input_purpose, InputPurpose::ProviderSearch);
    }

    #[test]
    fn lyrics_come_from_cache_per_track() {
        let (mut ctl, _rec, mut rx) = with_engine();
        let mut item = url_item(0);
        item.artist = Some("Band".into());
        item.title = "Song".into();
        ctl.app.queue.replace(vec![item], None);
        ctl.app.show_lyrics = true;
        let key = "Band\u{1f}Song".to_string();
        ctl.lyrics_cache
            .insert(key.clone(), Some(Lyrics::from_text("[00:01.00]hello")));
        ctl.apply(Command::PlayIndex(0));
        ctl.drain(started(ctl.play_token), &mut rx);
        assert!(matches!(&ctl.app.lyrics, LyricsStatus::Ready(l) if l.is_synced()));
        // A late answer for another track does not replace it.
        assert!(!ctl.handle_event(AppEvent::LyricsReady {
            key: "x\u{1f}y".into(),
            result: Ok(None),
        }));
        assert!(matches!(ctl.app.lyrics, LyricsStatus::Ready(_)));
        assert_eq!(
            lyrics_query(None, "A - B"),
            ("A".to_string(), "B".to_string())
        );
    }

    #[test]
    fn winamp_mode_routes_window_toggles_to_the_skin() {
        let (mut ctl, _rx) = controller();
        ctl.app.theme = Theme::from_name("Winamp");
        let before = (ctl.winamp_ui.show_eq, ctl.app.show_eq);
        ctl.apply(Command::ToggleEqWindow);
        assert_eq!(ctl.winamp_ui.show_eq, !before.0);
        assert_eq!(ctl.app.show_eq, before.1, "standard panel untouched");
        ctl.apply(Command::Eject);
        assert_eq!(ctl.app.input_purpose, InputPurpose::Open);
        ctl.apply(Command::Quit);
        assert!(ctl.app.should_quit);
    }

    #[test]
    fn stale_async_results_are_ignored() {
        let (mut ctl, _rx) = controller();
        ctl.search_token = 2;
        assert!(!ctl.handle_event(AppEvent::ItemsReady {
            token: 1,
            results: Ok(vec![url_item(0)]),
        }));
        assert!(ctl.app.results.is_empty());
        assert!(ctl.handle_event(AppEvent::ItemsReady {
            token: 2,
            results: Ok(vec![url_item(0)]),
        }));
        assert_eq!(ctl.app.results.len(), 1);
    }

    #[test]
    fn drain_coalesces_drags() {
        let (mut ctl, mut rx) = controller();
        let drag = |col| {
            AppEvent::Term(Event::Mouse(MouseEvent {
                kind: MouseEventKind::Drag(MouseButton::Left),
                column: col,
                row: 0,
                modifiers: KeyModifiers::NONE,
            }))
        };
        assert!(supersedes(&drag(1), &drag(2)));
        assert!(!supersedes(&drag(1), &ended(1)));
        ctl.tx.send(drag(2)).unwrap();
        ctl.tx.send(drag(3)).unwrap();
        assert!(ctl.drain(drag(1), &mut rx));
        assert!(rx.try_recv().is_err());
    }

    #[test]
    fn colon_commands_parse() {
        let ok = |s: &str| parse_colon(s).unwrap();
        assert_eq!(ok("q"), Command::Quit);
        assert_eq!(ok("pause"), Command::Pause);
        assert_eq!(ok("vol 40"), Command::SetVolume(0.4));
        assert_eq!(ok("bal -50"), Command::SetBalance(-0.5));
        assert_eq!(ok("repeat one"), Command::SetRepeat(RepeatMode::One));
        assert_eq!(ok("repeat"), Command::CycleRepeat);
        assert_eq!(ok("seek 1:30"), Command::SeekTo(Duration::from_secs(90)));
        assert_eq!(
            ok("seek 1:00:05"),
            Command::SeekTo(Duration::from_secs(3605))
        );
        assert_eq!(ok("seek +10"), Command::SeekBy(10));
        assert_eq!(ok("seek -7"), Command::SeekBy(-7));
        assert_eq!(ok("band 10 -3"), Command::SetEqBand { band: 9, db: -3.0 });
        assert_eq!(ok("preamp 2.5"), Command::SetPreamp(2.5));
        assert_eq!(ok("eq toggle"), Command::ToggleEq);
        assert_eq!(ok("eq auto"), Command::ToggleEqAuto);
        assert_eq!(ok("eq full bass"), Command::EqPreset("full bass".into()));
        for bad in [
            "band 11 1",
            "band 0 1",
            "vol",
            "vol loud",
            "seek x",
            "seek 99999999999999999999:0",
            "repeat sometimes",
            "frobnicate",
            "stop now",
        ] {
            assert!(parse_colon(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn resume_restores_queue_from_state() {
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
        let state = SessionState {
            queue: (0..3).map(|n| QueueEntry::from(&url_item(n))).collect(),
            queue_index: Some(2),
            position_secs: 42,
            repeat: RepeatMode::One,
            vis: crate::vis::VisMode::Fire,
            ..SessionState::default()
        };
        let playback = Playback::unavailable("test");
        let ctl = Controller::new(Config::default(), state, tx, playback, gfx());
        assert_eq!(ctl.app.queue.len(), 3);
        assert_eq!(ctl.app.queue.current_index(), Some(2));
        assert_eq!(ctl.app.queue.repeat(), RepeatMode::One);
        assert_eq!(ctl.app.vis_mode, crate::vis::VisMode::Fire);
        assert_eq!(ctl.resume, Some((2, 42)));
        assert_eq!(ctl.session_state().position_secs, 42);
    }
}
