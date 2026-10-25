//! The controller owns `App` plus the playback backends, applies every
//! `Command` through one dispatcher (`apply`), reacts to background events,
//! and runs the redraw-on-change event loop.

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
    display_title, App, BrowseAction, InputAction, InputPurpose, Mode, MouseAction,
    OnboardingAction, PlaybackState, SettingsAction, SkinBrowserAction, SkinBrowserSource,
    SkinEntry, Status,
};
use crate::command::Command;
use crate::config::{self, Config};
use crate::eq_presets;
use crate::event::AppEvent;
use crate::extractor::{self, ExtractorRegistry, YtdlpExtractor};
use crate::install::{self, Dep, DepStatus};
use crate::library;
use crate::media::{MediaItem, SourceKind};
use crate::net;
use crate::playback::{Backend, Playback};
use crate::playlist_io;
use crate::skin::WinampSkin;
use crate::state::{QueueEntry, SessionState};
use crate::term_input::InputThread;
use crate::theme::Theme;
use crate::ui;
use crate::ui::skin_layout::SkinLayout;
use crate::view::{PlayStatus, RepeatMode, EQ_BANDS, EQ_MAX_DB};

/// Session state is written at most this long after the first unsaved change.
const SAVE_DELAY: Duration = Duration::from_secs(2);
/// "Previous" restarts the current track when it has played longer than this.
const PREV_RESTART_SECS: u64 = 3;
const SKIN_PAGE: usize = 50;

pub struct Controller {
    pub app: App,
    playback: Playback,
    tx: UnboundedSender<AppEvent>,
    registry: ExtractorRegistry,
    /// Pending resume position: (queue index, seconds).
    resume: Option<(usize, u64)>,
    play_token: u64,
    search_token: u64,
    skin_fetch_token: u64,
    end_signalled: bool,
    save_at: Option<Instant>,
    saving: bool,
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

impl Controller {
    pub fn new(config: Config, state: SessionState, tx: UnboundedSender<AppEvent>) -> Self {
        let mut registry = ExtractorRegistry::default();
        registry.add(Arc::new(YtdlpExtractor::new(Duration::from_secs(
            config.search_timeout_secs.max(1),
        ))));

        let mut app = App::new(config);
        if let Some(theme) = state.theme.as_deref() {
            app.theme = Theme::from_name(theme);
            app.config.theme = app.theme.name.clone();
        }
        app.volume = state.volume;
        app.balance = state.balance;
        app.eq = state.eq;
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

        Self {
            app,
            playback: Playback::new(tx.clone()),
            tx,
            registry,
            resume,
            play_token: 0,
            search_token: 0,
            skin_fetch_token: 0,
            end_signalled: false,
            save_at: None,
            saving: false,
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
        if self.app.theme.name == "Winamp" {
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
        let SourceKind::Extractor(name) = &self.app.active_source else {
            return;
        };
        let Some(ext) = self.registry.get(name).cloned() else {
            self.app.error(format!("Unknown extractor '{name}'"));
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
            (Some(pb), _) => pb.elapsed_secs,
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

    /// Final synchronous save and backend teardown.
    pub fn shutdown(self) {
        let _ = self.app.config.save();
        let _ = self.session_state().save(&config::state_path());
        self.playback.shutdown();
    }

    // ── playback ─────────────────────────────────────────────────────────

    fn mpv_missing(&self) -> bool {
        self.app.deps.mpv == DepStatus::Missing
    }

    /// Start the queue's current item.
    fn play_current(&mut self) {
        let Some(item) = self.app.queue.current().cloned() else {
            return;
        };
        self.play_token += 1;
        self.end_signalled = false;
        self.app.playback = Some(PlaybackState {
            title: display_title(&item),
            duration_secs: item.duration.unwrap_or(0),
            elapsed_secs: 0,
            paused: false,
        });
        self.mark_dirty();
        match &item.source {
            SourceKind::Local => {
                let result = self
                    .playback
                    .play_local(Path::new(&item.id), self.mpv_missing());
                self.after_start(result);
            }
            SourceKind::Url => {
                let result = self.playback.play_url(&item.id, self.mpv_missing());
                self.after_start(result);
            }
            SourceKind::Extractor(name) => {
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
        }
    }

    fn after_start(&mut self, result: Result<()>) {
        match result {
            Ok(()) => {
                if matches!(self.app.status, Status::Loading(_) | Status::Error(_)) {
                    self.app.set_status(Status::Idle);
                }
                if let Some((idx, pos)) = self.resume.take() {
                    if Some(idx) == self.app.queue.current_index() {
                        self.seek_to(Duration::from_secs(pos));
                    }
                }
            }
            Err(e) => {
                self.app.playback = None;
                self.app.error(format!("{e:#}"));
            }
        }
    }

    fn set_paused(&mut self, paused: bool) {
        if let Some(pb) = &mut self.app.playback {
            pb.paused = paused;
            self.playback.set_paused(paused);
        }
    }

    fn seek_to(&mut self, target: Duration) {
        let Some(pb) = &mut self.app.playback else {
            return;
        };
        let target = if pb.duration_secs > 0 {
            target.min(Duration::from_secs(pb.duration_secs))
        } else {
            target
        };
        match self.playback.seek(target) {
            Ok(()) => pb.elapsed_secs = target.as_secs(),
            Err(e) => self.app.error(format!("{e:#}")),
        }
    }

    fn stop(&mut self) {
        self.playback.stop();
        self.app.playback = None;
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

    /// The single dispatcher every key, mouse hit and (later) IPC/MPRIS
    /// request goes through.
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
                let elapsed = self.app.playback.as_ref().map_or(0, |pb| pb.elapsed_secs);
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
                    let target = pb.elapsed_secs.saturating_add_signed(delta);
                    self.seek_to(Duration::from_secs(target));
                }
            }
            Command::SeekFrac(frac) => {
                if let Some(pb) = &self.app.playback {
                    if pb.duration_secs > 0 {
                        let secs = pb.duration_secs as f64 * f64::from(frac.clamp(0.0, 1.0));
                        self.seek_to(Duration::from_secs_f64(secs));
                    }
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
                self.mark_dirty();
            }
            Command::ToggleEqAuto => {
                self.app.eq.auto = !self.app.eq.auto;
                self.mark_dirty();
            }
            Command::SetEqBand { band, db } => {
                if band < EQ_BANDS && db.is_finite() {
                    self.app.eq.bands_db[band] = db.clamp(-EQ_MAX_DB, EQ_MAX_DB);
                    self.mark_dirty();
                }
            }
            Command::SetPreamp(db) => {
                if db.is_finite() {
                    self.app.eq.preamp_db = db.clamp(-EQ_MAX_DB, EQ_MAX_DB);
                    self.mark_dirty();
                }
            }
            Command::EqPreset(name) => {
                if eq_presets::apply(&mut self.app.eq, &name) {
                    self.app.info(format!("EQ preset: {name}"));
                    self.mark_dirty();
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
            Command::ToggleEqWindow => self.app.show_eq = !self.app.show_eq,
            Command::TogglePlaylistWindow => self.app.show_playlist = !self.app.show_playlist,
            Command::Quit => self.app.should_quit = true,
        }
    }

    fn set_volume(&mut self, v: f32) {
        self.app.volume = if v.is_finite() {
            v.clamp(0.0, 1.0)
        } else {
            1.0
        };
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
        match self.app.handle_mouse(mouse) {
            MouseAction::None => {}
            MouseAction::Cmd(cmd) => self.apply(cmd),
            MouseAction::PlayResult(i) => self.on_browse_action(BrowseAction::PlayResult(i)),
            MouseAction::PrevPage => self.on_browse_action(BrowseAction::PrevPage),
            MouseAction::NextPage => self.on_browse_action(BrowseAction::NextPage),
        }
    }

    fn on_browse_action(&mut self, action: BrowseAction) {
        match action {
            BrowseAction::None => {}
            BrowseAction::Cmd(cmd) => self.apply(cmd),
            BrowseAction::PlayResult(i) => {
                if i < self.app.results.len() {
                    let items = self.app.results.clone();
                    self.app.queue.replace(items, Some(i));
                    self.play_current();
                }
            }
            BrowseAction::Enqueue(i) => {
                if let Some(item) = self.app.results.get(i).cloned() {
                    let title = item.title.clone();
                    self.app.queue.enqueue(item);
                    self.app.info(format!(
                        "Queued: {title} ({} in queue)",
                        self.app.queue.len()
                    ));
                    self.mark_dirty();
                }
            }
            BrowseAction::PlayNext(i) => {
                if let Some(item) = self.app.results.get(i).cloned() {
                    let title = item.title.clone();
                    self.app.queue.play_next(item);
                    self.app.info(format!("Up next: {title}"));
                    self.mark_dirty();
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
            BrowseAction::SwitchSource => self.switch_source(),
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

    fn switch_source(&mut self) {
        match self.app.active_source {
            SourceKind::Local => {
                let name = self.app.config.extractor.clone();
                if self.registry.get(&name).is_none() {
                    self.app
                        .error(format!("Unknown extractor '{name}' in config"));
                    return;
                }
                if name == "ytdlp" && self.app.deps.ytdlp == DepStatus::Missing {
                    self.app.error(
                        "yt-dlp is not installed — online search is unavailable (see `rustune doctor`)",
                    );
                    return;
                }
                self.app.active_source = SourceKind::Extractor(name);
                self.app.results.clear();
            }
            SourceKind::Extractor(_) | SourceKind::Url => {
                self.app.active_source = SourceKind::Local;
                self.app.results = self.app.local_library.clone();
                if !self.app.library_scanned {
                    self.start_scan();
                }
            }
        }
        self.app.page = 0;
        self.app.list_state.select(Some(0));
    }

    fn on_submit(&mut self, purpose: InputPurpose, text: String) {
        match purpose {
            InputPurpose::Search => match self.app.active_source {
                SourceKind::Local => {
                    self.app.results = library::search(&self.app.local_library, &text);
                    self.app.list_state.select(Some(0));
                    self.app.page = 0;
                    let n = self.app.results.len();
                    self.app
                        .info(format!("{n} match{}", if n == 1 { "" } else { "es" }));
                }
                SourceKind::Extractor(_) | SourceKind::Url => self.dispatch_search(),
            },
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
            SettingsAction::Quit => {
                if let Err(e) = self.app.config.save() {
                    self.app.error(format!("Cannot save config: {e:#}"));
                }
            }
            SettingsAction::ThemeChanged => {
                if self.app.theme.name == "Winamp" && self.app.winamp_skin.is_none() {
                    self.load_skin(self.app.skin_path.clone());
                }
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
            OnboardingAction::SelectTheme(_) => {
                if let Err(e) = self.app.config.save() {
                    self.app.error(format!("Cannot save config: {e:#}"));
                }
                self.start_scan();
                if self.app.theme.name == "Winamp" {
                    self.load_skin(self.app.skin_path.clone());
                }
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
                let mpv_missing = self.mpv_missing();
                let result = result.and_then(|info| {
                    if let Some(pb) = &mut self.app.playback {
                        if !info.title.is_empty() {
                            pb.title = info.title;
                        }
                    }
                    self.playback.play_url(&info.url, mpv_missing)
                });
                self.after_start(result);
            }
            AppEvent::PlaybackProgress {
                elapsed_secs,
                duration_secs,
            } => {
                let Some(pb) = &mut self.app.playback else {
                    return false;
                };
                let changed = pb.elapsed_secs != elapsed_secs
                    || (duration_secs > 0 && pb.duration_secs != duration_secs);
                pb.elapsed_secs = elapsed_secs;
                if duration_secs > 0 {
                    pb.duration_secs = duration_secs;
                }
                if !self.end_signalled && !pb.paused && self.playback.finished() {
                    self.end_signalled = true;
                    let _ = self.tx.send(AppEvent::PlaybackComplete);
                }
                return changed;
            }
            AppEvent::PlaybackComplete => {
                // A late signal for a track that was already replaced.
                if self.playback.active() == Backend::Local && !self.playback.finished() {
                    return false;
                }
                self.on_track_end();
            }
            AppEvent::DepsProbed(deps) => self.app.deps = deps,
            AppEvent::ScanComplete(result) => {
                self.app.library_scanned = true;
                match result {
                    Ok(items) => {
                        let n = items.len();
                        self.app.local_library = items;
                        if self.app.active_source == SourceKind::Local {
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
                    self.app.skin_layout = SkinLayout::from_skin(&skin);
                    self.app.winamp_skin = Some(*skin);
                    self.app.skin_path = path;
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
                    if self.app.theme.name == "Winamp" && self.app.skin_path.is_none() {
                        self.load_skin(Some(path));
                    }
                }
                Ok(None) => return false,
                Err(e) => {
                    if self.app.theme.name != "Winamp" {
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

/// Redraw-on-change loop: sleeps until input, a background event, a
/// status-expiry tick or the autosave deadline. Idle means fully asleep.
pub async fn run(
    mut terminal: DefaultTerminal,
    ctl: &mut Controller,
    mut rx: UnboundedReceiver<AppEvent>,
    input: &InputThread,
) -> Result<()> {
    let mut redraw = true;
    loop {
        if redraw {
            terminal.draw(|frame| ui::draw(frame, &mut ctl.app))?;
            redraw = false;
        }
        if ctl.app.should_quit {
            return Ok(());
        }
        if let Some(dep) = ctl.app.pending_install.take() {
            terminal = ctl.install(dep, input).await?;
            redraw = true;
            continue;
        }

        let tick = ctl.app.wants_tick();
        let save_at = ctl.save_deadline();
        tokio::select! {
            event = rx.recv() => {
                let Some(event) = event else { return Ok(()) };
                redraw = ctl.drain(event, &mut rx);
            }
            () = tokio::time::sleep(tick.unwrap_or_default()), if tick.is_some() => {
                redraw = ctl.app.on_tick();
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
    use crossterm::event::{KeyModifiers, MouseButton};

    fn controller() -> (Controller, UnboundedReceiver<AppEvent>) {
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
        let config = Config {
            onboarding_done: true,
            ..Config::default()
        };
        let ctl = Controller::new(config, SessionState::default(), tx);
        (ctl, rx)
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

    /// Streams fail fast when mpv is known missing, which lets these tests
    /// exercise the dispatcher without audio hardware or processes.
    fn offline(ctl: &mut Controller, n: usize) {
        ctl.app.deps.mpv = DepStatus::Missing;
        ctl.app.queue.replace((0..n).map(url_item).collect(), None);
    }

    #[test]
    fn state_settings_commands_clamp_and_mark_dirty() {
        let (mut ctl, _rx) = controller();
        ctl.apply(Command::VolumeBy(0.5));
        assert_eq!(ctl.app.volume, 1.0);
        ctl.apply(Command::SetVolume(-3.0));
        assert_eq!(ctl.app.volume, 0.0);
        ctl.apply(Command::SetBalance(0.3));
        assert!((ctl.app.balance - 0.3).abs() < 1e-6);
        ctl.apply(Command::SetEqBand { band: 3, db: 40.0 });
        assert_eq!(ctl.app.eq.bands_db[3], EQ_MAX_DB);
        ctl.apply(Command::SetEqBand { band: 99, db: 1.0 });
        ctl.apply(Command::EqPreset("Rock".into()));
        assert_eq!(ctl.app.eq.bands_db[0], 8.0);
        ctl.apply(Command::CycleRepeat);
        assert_eq!(ctl.app.queue.repeat(), RepeatMode::All);
        ctl.apply(Command::ToggleShuffle);
        assert!(ctl.app.queue.shuffle());
        assert!(ctl.save_deadline().is_some());

        let state = ctl.session_state();
        assert_eq!(state.volume, 0.0);
        assert!(state.shuffle);
        assert_eq!(state.repeat, RepeatMode::All);
    }

    #[test]
    fn window_toggles_eject_and_quit() {
        let (mut ctl, _rx) = controller();
        ctl.apply(Command::ToggleEqWindow);
        ctl.apply(Command::TogglePlaylistWindow);
        assert!(ctl.app.show_eq && ctl.app.show_playlist);
        ctl.apply(Command::Eject);
        assert_eq!(ctl.app.mode, Mode::Input);
        assert_eq!(ctl.app.input_purpose, InputPurpose::Open);
        ctl.apply(Command::Quit);
        assert!(ctl.app.should_quit);
    }

    #[test]
    fn failed_start_reports_error_and_stays_stopped() {
        let (mut ctl, _rx) = controller();
        offline(&mut ctl, 3);
        ctl.apply(Command::PlayIndex(1));
        assert_eq!(ctl.app.queue.current_index(), Some(1));
        assert_eq!(ctl.app.play_status(), PlayStatus::Stopped);
        assert!(matches!(&ctl.app.status, Status::Error(m) if m.contains("mpv")));
    }

    #[test]
    fn next_prev_walk_the_queue() {
        let (mut ctl, _rx) = controller();
        offline(&mut ctl, 3);
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
        let (mut ctl, _rx) = controller();
        offline(&mut ctl, 3);
        ctl.app.queue.jump(1);
        ctl.app.playback = Some(PlaybackState {
            title: "t1".into(),
            duration_secs: 100,
            elapsed_secs: 10,
            paused: false,
        });
        ctl.apply(Command::Prev);
        assert_eq!(ctl.app.queue.current_index(), Some(1));
        // Idle backend: seeking is a no-op success, position resets.
        assert_eq!(ctl.app.playback.as_ref().map(|p| p.elapsed_secs), Some(0));
    }

    #[test]
    fn seek_commands_clamp_to_track() {
        let (mut ctl, _rx) = controller();
        ctl.app.playback = Some(PlaybackState {
            title: String::new(),
            duration_secs: 100,
            elapsed_secs: 50,
            paused: false,
        });
        ctl.apply(Command::SeekBy(-80));
        assert_eq!(ctl.app.playback.as_ref().unwrap().elapsed_secs, 0);
        ctl.apply(Command::SeekBy(500));
        assert_eq!(ctl.app.playback.as_ref().unwrap().elapsed_secs, 100);
        ctl.apply(Command::SeekFrac(0.25));
        assert_eq!(ctl.app.playback.as_ref().unwrap().elapsed_secs, 25);
        ctl.apply(Command::SeekTo(Duration::from_secs(1000)));
        assert_eq!(ctl.app.playback.as_ref().unwrap().elapsed_secs, 100);
    }

    #[test]
    fn track_end_stops_at_queue_end_or_wraps() {
        let (mut ctl, _rx) = controller();
        offline(&mut ctl, 2);
        ctl.app.queue.jump(1);
        ctl.app.playback = Some(PlaybackState {
            title: "t1".into(),
            duration_secs: 100,
            elapsed_secs: 100,
            paused: false,
        });
        ctl.on_track_end();
        assert_eq!(ctl.app.play_status(), PlayStatus::Stopped);
        assert_eq!(ctl.app.queue.current_index(), Some(1));

        ctl.app.queue.set_repeat(RepeatMode::All);
        ctl.app.playback = Some(PlaybackState {
            title: "t1".into(),
            duration_secs: 100,
            elapsed_secs: 100,
            paused: false,
        });
        ctl.on_track_end();
        assert_eq!(ctl.app.queue.current_index(), Some(0), "repeat all wraps");
    }

    #[test]
    fn results_actions_build_the_queue() {
        let (mut ctl, _rx) = controller();
        ctl.app.deps.mpv = DepStatus::Missing;
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
    fn progress_updates_only_redraw_on_change() {
        let (mut ctl, _rx) = controller();
        assert!(!ctl.handle_event(AppEvent::PlaybackProgress {
            elapsed_secs: 1,
            duration_secs: 10,
        }));
        ctl.app.playback = Some(PlaybackState {
            title: String::new(),
            duration_secs: 0,
            elapsed_secs: 0,
            paused: false,
        });
        assert!(ctl.handle_event(AppEvent::PlaybackProgress {
            elapsed_secs: 1,
            duration_secs: 10,
        }));
        assert!(!ctl.handle_event(AppEvent::PlaybackProgress {
            elapsed_secs: 1,
            duration_secs: 0,
        }));
        assert_eq!(ctl.app.playback.as_ref().unwrap().duration_secs, 10);
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
        assert!(!supersedes(&drag(1), &AppEvent::PlaybackComplete));
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
            ..SessionState::default()
        };
        let ctl = Controller::new(Config::default(), state, tx);
        assert_eq!(ctl.app.queue.len(), 3);
        assert_eq!(ctl.app.queue.current_index(), Some(2));
        assert_eq!(ctl.app.queue.repeat(), RepeatMode::One);
        assert_eq!(ctl.resume, Some((2, 42)));
        assert_eq!(ctl.session_state().position_secs, 42);
    }
}
