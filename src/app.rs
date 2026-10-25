//! UI-facing application state and key/mouse → action mapping. Player
//! actions come out as `Command`s; the controller applies them.

use std::path::PathBuf;
use std::time::{Duration, Instant};

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use ratatui::layout::{Position, Rect};
use ratatui::widgets::ListState;

use crate::browse::{BrowseOutcome, Browser};
use crate::command::Command;
use crate::config::Config;
use crate::install::{Dep, Deps};
use crate::media::MediaItem;
use crate::providers::lyrics::Lyrics;
use crate::providers::ProviderKind;
use crate::queue::Queue;
use crate::skin::WinampSkin;
use crate::theme::Theme;
use crate::view::{EqSettings, PlayStatus, PlayerView, StreamFormat};
use crate::vis::analyzer::{Analyzer, AnalyzerConfig};
use crate::vis::palette::VisPalette;
use crate::vis::VisMode;

/// How long informational status messages stay visible.
const INFO_TTL: Duration = Duration::from_secs(4);
const SEEK_SMALL: i64 = 5;
const SEEK_LARGE: i64 = 30;
const VOLUME_STEP: f32 = 0.05;

#[derive(Debug, Clone, PartialEq)]
pub enum Mode {
    Browse,
    Input,
    Settings,
    Onboarding,
    SkinBrowser,
}

/// What the single-line prompt is collecting.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InputPurpose {
    Search,
    SavePlaylist,
    /// File, directory, playlist or URL to play (Winamp "eject").
    Open,
    /// Search inside the active provider tab.
    ProviderSearch,
}

/// A source tab in the library view (cycled with Tab).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tab {
    Local,
    /// yt-dlp search.
    Online,
    Provider(ProviderKind),
}

impl Tab {
    pub fn label(self) -> &'static str {
        match self {
            Tab::Local => "Local",
            Tab::Online => "Online",
            Tab::Provider(ProviderKind::Radio) => "Radio",
            Tab::Provider(ProviderKind::Podcast) => "Podcasts",
            Tab::Provider(ProviderKind::Subsonic) => "Subsonic",
            Tab::Provider(ProviderKind::Jellyfin) => "Jellyfin",
        }
    }
}

/// Lyrics of the playing track.
#[derive(Debug, Clone)]
pub enum LyricsStatus {
    /// Nothing playing (or panel never needed).
    Idle,
    Loading,
    NotFound,
    Error(String),
    Ready(Lyrics),
}

#[derive(Debug, Clone)]
pub enum Status {
    Idle,
    Searching(String),
    Loading(String),
    Scanning(String),
    Downloading(String),
    /// Transient confirmation (expires after a few seconds).
    Info(String),
    Error(String),
}

#[derive(Debug, Clone)]
pub struct PlaybackState {
    pub title: String,
    pub duration: Option<Duration>,
    pub elapsed: Duration,
    pub paused: bool,
    pub format: StreamFormat,
}

/// Layout rects for mouse hit-testing (written by the renderers).
#[derive(Debug, Clone, Default)]
#[allow(dead_code)] // `player_info`, `input`, `help` are recorded for renderers; not hit-tested yet.
pub struct LayoutRects {
    pub results: Rect,
    pub player_info: Rect,
    pub player_bar: Rect,
    pub input: Rect,
    pub help: Rect,
    pub pause_button: Rect,
    pub prev_page: Rect,
    pub next_page: Rect,
}

pub enum MouseAction {
    None,
    Cmd(Command),
    Browse(BrowseOutcome),
    PlayResult(usize),
    PrevPage,
    NextPage,
}

#[derive(Debug, Clone, PartialEq)]
pub enum OnboardingStep {
    Welcome,
    Dependencies,
    MusicDir,
    Theme,
}

#[derive(Debug, Clone, PartialEq)]
pub enum SettingsField {
    MusicDir,
    Extensions,
    Theme,
    PageSize,
    Extractor,
}

/// A single entry in the skin browser.
#[derive(Debug, Clone)]
pub struct SkinEntry {
    pub md5: String,
    /// Sanitised file name inside the skins directory.
    pub filename: String,
    pub display_name: String,
    pub is_local: bool,
    pub average_color: Option<String>,
}

pub enum SkinBrowserAction {
    None,
    Back,
    Download(String, String), // md5, filename
    LoadLocal(String),        // filename
    RequestFetch,
    Search(String),
}

#[derive(Debug, Clone, PartialEq)]
pub enum SkinBrowserSource {
    Local,
    Online,
}

pub enum BrowseAction {
    None,
    Cmd(Command),
    /// Replace the queue with the result list and play this index.
    PlayResult(usize),
    Enqueue(usize),
    PlayNext(usize),
    RemoveFromQueue(usize),
    MoveInQueue {
        from: usize,
        to: usize,
    },
    NextPage,
    PrevPage,
    OpenSettings,
    /// Next (`true`) or previous source tab.
    SwitchTab(bool),
    /// Something happened in a provider tab's browser.
    Browse(BrowseOutcome),
    Download(String, String), // id, title
}

pub enum InputAction {
    None,
    Submit(InputPurpose, String),
    /// `:command` typed into the search prompt (without the colon).
    Colon(String),
}

pub enum SettingsAction {
    None,
    Quit,
    ThemeChanged,
    OpenSkinBrowserLocal,
    OpenSkinBrowserOnline,
}

pub enum OnboardingAction {
    None,
    Next,
    SetMusicDir(String),
    /// Theme chosen (already stored in `config.theme`); onboarding is done.
    SelectTheme,
}

pub struct App {
    pub mode: Mode,
    pub results: Vec<MediaItem>,
    pub list_state: ListState,
    pub queue_list_state: ListState,
    pub page: usize,
    pub input_text: String,
    pub input_cursor: usize,
    pub input_history: Vec<String>,
    pub history_index: usize,
    pub input_purpose: InputPurpose,
    pub playback: Option<PlaybackState>,
    pub status: Status,
    status_expires: Option<Instant>,
    pub should_quit: bool,
    pub layout_rects: LayoutRects,
    pub config: Config,
    pub theme: Theme,
    pub winamp_skin: Option<WinampSkin>,
    /// File of the loaded skin (`None` = built-in colours).
    pub skin_path: Option<PathBuf>,
    pub tab: Tab,
    /// Tabs in cycling order: Local, Online, then enabled providers.
    pub tabs: Vec<Tab>,
    /// One browser per provider tab.
    pub browsers: Vec<Browser>,
    /// Prompt text for `InputPurpose::ProviderSearch`.
    pub prompt_label: &'static str,
    pub onboarding_step: OnboardingStep,
    pub onboarding_dep_selected: usize,
    pub pending_install: Option<Dep>,
    pub settings_field: SettingsField,
    // Skin browser
    pub skin_entries: Vec<SkinEntry>,
    pub skin_list_state: ListState,
    pub skin_browser_loading: bool,
    pub skin_browser_error: Option<String>,
    pub skin_browser_offset: usize,
    pub skin_browser_has_more: bool,
    pub skin_downloading_md5: Option<String>,
    pub skin_total_count: usize,
    pub skin_browser_source: SkinBrowserSource,
    pub skin_search_query: String,
    pub skin_search_active: bool,
    pub downloading_title: Option<String>,
    pub local_library: Vec<MediaItem>,
    pub library_scanned: bool,
    // Player
    pub queue: Queue,
    /// 0.0..=1.0
    pub volume: f32,
    /// -1.0..=1.0
    pub balance: f32,
    pub eq: EqSettings,
    pub show_eq: bool,
    /// Focused EQ panel slider (0 = preamp, 1..=10 = bands).
    pub eq_focus: usize,
    pub show_playlist: bool,
    pub show_lyrics: bool,
    pub lyrics: LyricsStatus,
    pub vis_mode: VisMode,
    pub analyzer: Analyzer,
    pub vis_palette: VisPalette,
    /// Width and mode the analyzer bands were last sized for.
    vis_fit: (u16, VisMode),
    /// The analyzer uses the Winamp configuration.
    analyzer_winamp: bool,
    pub deps: Deps,
}

/// "Artist - Title" when the artist is known.
pub fn display_title(item: &MediaItem) -> String {
    match &item.artist {
        Some(artist) => format!("{artist} - {}", item.title),
        None => item.title.clone(),
    }
}

impl App {
    pub fn new(config: Config) -> Self {
        let theme = Theme::from_name(&config.theme);
        let mut tabs = vec![Tab::Local, Tab::Online];
        tabs.extend(config.providers.enabled().into_iter().map(Tab::Provider));
        let browsers = tabs
            .iter()
            .filter_map(|tab| match tab {
                Tab::Provider(kind) => Some(Browser::new(*kind)),
                _ => None,
            })
            .collect();
        let analyzer_winamp = theme.name == "Winamp";
        let analyzer = Analyzer::new(if analyzer_winamp {
            AnalyzerConfig::winamp()
        } else {
            AnalyzerConfig::default()
        });
        let mut list_state = ListState::default();
        list_state.select(Some(0));
        let mut skin_list_state = ListState::default();
        skin_list_state.select(Some(0));

        Self {
            mode: if config.onboarding_done {
                Mode::Browse
            } else {
                Mode::Onboarding
            },
            results: Vec::new(),
            list_state,
            queue_list_state: ListState::default(),
            page: 0,
            input_text: String::new(),
            input_cursor: 0,
            input_history: Vec::new(),
            history_index: 0,
            input_purpose: InputPurpose::Search,
            playback: None,
            status: Status::Idle,
            status_expires: None,
            should_quit: false,
            layout_rects: LayoutRects::default(),
            config,
            theme,
            winamp_skin: None,
            skin_path: None,
            tab: Tab::Local,
            tabs,
            browsers,
            prompt_label: "Search",
            onboarding_step: OnboardingStep::Welcome,
            onboarding_dep_selected: 0,
            pending_install: None,
            settings_field: SettingsField::MusicDir,
            skin_entries: Vec::new(),
            skin_list_state,
            skin_browser_loading: false,
            skin_browser_error: None,
            skin_browser_offset: 0,
            skin_browser_has_more: true,
            skin_downloading_md5: None,
            skin_total_count: 0,
            skin_browser_source: SkinBrowserSource::Local,
            skin_search_query: String::new(),
            skin_search_active: false,
            downloading_title: None,
            local_library: Vec::new(),
            library_scanned: false,
            queue: Queue::default(),
            volume: 1.0,
            balance: 0.0,
            eq: EqSettings::default(),
            show_eq: false,
            eq_focus: 0,
            show_playlist: false,
            show_lyrics: false,
            lyrics: LyricsStatus::Idle,
            vis_mode: VisMode::default(),
            analyzer,
            vis_palette: VisPalette::default(),
            vis_fit: (0, VisMode::None),
            analyzer_winamp,
            deps: Deps::default(),
        }
    }

    pub fn is_winamp(&self) -> bool {
        self.theme.name == "Winamp"
    }

    /// Use the Winamp analyzer (19 bars, 75-point scope) for the skin and
    /// the width-fitted one for the other themes. Call after theme changes.
    pub fn sync_analyzer(&mut self) {
        let winamp = self.is_winamp();
        if winamp == self.analyzer_winamp {
            return;
        }
        self.analyzer_winamp = winamp;
        self.analyzer = Analyzer::new(if winamp {
            AnalyzerConfig::winamp()
        } else {
            AnalyzerConfig::default()
        });
        self.vis_fit = (0, VisMode::None);
    }

    /// Size the analyzer for a visualizer strip `width` cells wide.
    pub fn fit_vis(&mut self, width: u16) {
        if self.vis_fit == (width, self.vis_mode) {
            return;
        }
        self.vis_fit = (width, self.vis_mode);
        let bands = self.vis_mode.bands_for_width(width);
        if bands > 0 {
            self.analyzer.set_bands(bands);
        }
        if self.vis_mode == VisMode::Scope {
            self.analyzer.set_scope_len(usize::from(width) * 2);
        }
    }

    /// The browser of the active provider tab.
    pub fn browser_mut(&mut self) -> Option<&mut Browser> {
        let Tab::Provider(kind) = self.tab else {
            return None;
        };
        self.browsers.iter_mut().find(|b| b.kind == kind)
    }

    pub fn browser_for(&mut self, kind: ProviderKind) -> Option<&mut Browser> {
        self.browsers.iter_mut().find(|b| b.kind == kind)
    }

    // ── status ───────────────────────────────────────────────────────────

    pub fn set_status(&mut self, status: Status) {
        self.status_expires = matches!(status, Status::Info(_)).then(|| Instant::now() + INFO_TTL);
        self.status = status;
    }

    pub fn info(&mut self, msg: impl Into<String>) {
        self.set_status(Status::Info(msg.into()));
    }

    pub fn error(&mut self, msg: impl Into<String>) {
        self.set_status(Status::Error(msg.into()));
    }

    /// Delay until the UI needs to redraw without any event (expiring a
    /// status message); `None` when nothing is time-driven.
    pub fn wants_tick(&self) -> Option<Duration> {
        self.status_expires
            .map(|at| at.saturating_duration_since(Instant::now()))
    }

    /// Time-driven updates; returns whether anything visible changed.
    pub fn on_tick(&mut self) -> bool {
        match self.status_expires {
            Some(at) if Instant::now() >= at => {
                self.status_expires = None;
                self.status = Status::Idle;
                true
            }
            _ => false,
        }
    }

    // ── player ───────────────────────────────────────────────────────────

    pub fn play_status(&self) -> PlayStatus {
        match &self.playback {
            None => PlayStatus::Stopped,
            Some(pb) if pb.paused => PlayStatus::Paused,
            Some(_) => PlayStatus::Playing,
        }
    }

    /// Snapshot for renderers (TUI themes and the Winamp skin compositor).
    pub fn player_view(&self) -> PlayerView<'_> {
        let current = self.queue.current();
        let (title, elapsed, duration, format) = match &self.playback {
            Some(pb) => (pb.title.as_str(), pb.elapsed, pb.duration, pb.format),
            None => (
                current.map_or("", |i| i.title.as_str()),
                Duration::ZERO,
                current.and_then(|i| i.duration).map(Duration::from_secs),
                StreamFormat::default(),
            ),
        };
        PlayerView {
            title,
            status: self.play_status(),
            elapsed,
            duration,
            volume: self.volume,
            balance: self.balance,
            shuffle: self.queue.shuffle(),
            repeat: self.queue.repeat(),
            eq: self.eq,
            format,
            spectrum: self.analyzer.spectrum(),
            scope: self.analyzer.scope(),
            playlist: self.queue.items(),
            current: self.queue.current_index(),
            selected: self.queue.cursor(),
        }
    }

    // ── result list ──────────────────────────────────────────────────────

    pub fn select_next(&mut self) {
        if self.results.is_empty() {
            return;
        }
        let i = match self.list_state.selected() {
            Some(i) if i + 1 >= self.results.len() => 0,
            Some(i) => i + 1,
            None => 0,
        };
        self.list_state.select(Some(i));
    }

    pub fn select_prev(&mut self) {
        if self.results.is_empty() {
            return;
        }
        let i = match self.list_state.selected() {
            Some(0) | None => self.results.len() - 1,
            Some(i) => (i - 1).min(self.results.len() - 1),
        };
        self.list_state.select(Some(i));
    }

    pub fn selected_index(&self) -> Option<usize> {
        self.list_state
            .selected()
            .filter(|&i| i < self.results.len())
    }

    pub fn selected_result(&self) -> Option<&MediaItem> {
        self.selected_index().map(|i| &self.results[i])
    }

    pub fn format_duration(secs: u64) -> String {
        let h = secs / 3600;
        let m = (secs % 3600) / 60;
        let s = secs % 60;
        if h > 0 {
            format!("{h}:{m:02}:{s:02}")
        } else {
            format!("{m}:{s:02}")
        }
    }

    // ── prompt ───────────────────────────────────────────────────────────

    pub fn open_prompt(&mut self, purpose: InputPurpose) {
        self.mode = Mode::Input;
        self.input_purpose = purpose;
        self.input_text.clear();
        self.input_cursor = 0;
        self.history_index = self.input_history.len();
    }

    pub fn insert_text(&mut self, text: &str) {
        for c in text.chars().filter(|c| !c.is_control()) {
            self.insert_char(c);
        }
    }

    fn insert_char(&mut self, c: char) {
        match self.input_text.char_indices().nth(self.input_cursor) {
            Some((idx, _)) => self.input_text.insert(idx, c),
            None => self.input_text.push(c),
        }
        self.input_cursor += 1;
    }

    fn delete_before_cursor(&mut self) {
        if self.input_cursor > 0 {
            self.input_cursor -= 1;
            if let Some((idx, _)) = self.input_text.char_indices().nth(self.input_cursor) {
                self.input_text.remove(idx);
            }
        }
    }

    // ── keys ─────────────────────────────────────────────────────────────

    /// Player keys shared by every browse view (Winamp transport layout).
    fn transport_command(key: &KeyEvent) -> Option<Command> {
        let shift = key.modifiers.contains(KeyModifiers::SHIFT);
        Some(match key.code {
            KeyCode::Char('q') => Command::Quit,
            KeyCode::Char('z') => Command::Prev,
            KeyCode::Char('x') => Command::Play,
            KeyCode::Char('c') => Command::TogglePause,
            KeyCode::Char('v') => Command::Stop,
            KeyCode::Char('b') => Command::Next,
            KeyCode::Char(' ') => Command::TogglePause,
            KeyCode::Left => Command::SeekBy(if shift { -SEEK_LARGE } else { -SEEK_SMALL }),
            KeyCode::Right => Command::SeekBy(if shift { SEEK_LARGE } else { SEEK_SMALL }),
            KeyCode::Char('+' | '=') => Command::VolumeBy(VOLUME_STEP),
            KeyCode::Char('-') => Command::VolumeBy(-VOLUME_STEP),
            KeyCode::Char('S') => Command::ToggleShuffle,
            KeyCode::Char('R') => Command::CycleRepeat,
            KeyCode::Char('e') => Command::ToggleEqWindow,
            KeyCode::Char('P') => Command::TogglePlaylistWindow,
            KeyCode::Char('l') => Command::Eject,
            KeyCode::Char('V') => Command::CycleVis,
            KeyCode::Char('y') => Command::ToggleLyrics,
            _ => return None,
        })
    }

    pub fn handle_browse_key(&mut self, key: KeyEvent) -> BrowseAction {
        if key.modifiers.contains(KeyModifiers::CONTROL) {
            return match key.code {
                KeyCode::Char('c') => BrowseAction::Cmd(Command::Quit),
                _ => BrowseAction::None,
            };
        }
        if self.show_eq && !self.is_winamp() {
            let before = self.eq_focus;
            if let Some(cmd) = crate::ui::eq_key(&key, &mut self.eq_focus, &self.eq) {
                return BrowseAction::Cmd(cmd);
            }
            if self.eq_focus != before {
                return BrowseAction::None;
            }
            if key.code == KeyCode::Esc {
                return BrowseAction::Cmd(Command::ToggleEqWindow);
            }
        }
        if let Some(cmd) = Self::transport_command(&key) {
            return BrowseAction::Cmd(cmd);
        }
        let provider_tab = matches!(self.tab, Tab::Provider(_)) && !self.show_playlist;
        match key.code {
            KeyCode::Char('/') if !provider_tab => {
                self.open_prompt(InputPurpose::Search);
                BrowseAction::None
            }
            KeyCode::Char('w') => {
                self.open_prompt(InputPurpose::SavePlaylist);
                BrowseAction::None
            }
            KeyCode::Char('s') => BrowseAction::OpenSettings,
            KeyCode::Tab => BrowseAction::SwitchTab(true),
            KeyCode::BackTab => BrowseAction::SwitchTab(false),
            _ if self.show_playlist => self.playlist_key(key),
            _ if provider_tab => {
                let subs = self.config.providers.podcast.subscriptions.clone();
                match self.browser_mut() {
                    Some(browser) => BrowseAction::Browse(browser.handle_key(&key, &subs)),
                    None => BrowseAction::None,
                }
            }
            _ => self.results_key(key),
        }
    }

    fn playlist_key(&mut self, key: KeyEvent) -> BrowseAction {
        let len = self.queue.len();
        let cursor = self.queue.cursor();
        if len == 0 {
            return BrowseAction::None;
        }
        match key.code {
            KeyCode::Char('j') | KeyCode::Down => {
                BrowseAction::Cmd(Command::Select((cursor + 1).min(len - 1)))
            }
            KeyCode::Char('k') | KeyCode::Up => {
                BrowseAction::Cmd(Command::Select(cursor.saturating_sub(1)))
            }
            KeyCode::Char('g') | KeyCode::Home => BrowseAction::Cmd(Command::Select(0)),
            KeyCode::Char('G') | KeyCode::End => BrowseAction::Cmd(Command::Select(len - 1)),
            KeyCode::Enter => BrowseAction::Cmd(Command::PlayIndex(cursor)),
            KeyCode::Char('J') if cursor + 1 < len => BrowseAction::MoveInQueue {
                from: cursor,
                to: cursor + 1,
            },
            KeyCode::Char('K') if cursor > 0 => BrowseAction::MoveInQueue {
                from: cursor,
                to: cursor - 1,
            },
            KeyCode::Char('D') | KeyCode::Delete => BrowseAction::RemoveFromQueue(cursor),
            _ => BrowseAction::None,
        }
    }

    fn results_key(&mut self, key: KeyEvent) -> BrowseAction {
        let online = self.tab == Tab::Online;
        match key.code {
            KeyCode::Char('j') | KeyCode::Down => {
                self.select_next();
                BrowseAction::None
            }
            KeyCode::Char('k') | KeyCode::Up => {
                self.select_prev();
                BrowseAction::None
            }
            KeyCode::Char('g') | KeyCode::Home => {
                if !self.results.is_empty() {
                    self.list_state.select(Some(0));
                }
                BrowseAction::None
            }
            KeyCode::Char('G') | KeyCode::End => {
                if !self.results.is_empty() {
                    self.list_state.select(Some(self.results.len() - 1));
                }
                BrowseAction::None
            }
            KeyCode::Char('n') if online => BrowseAction::NextPage,
            KeyCode::Char('p') if online => BrowseAction::PrevPage,
            KeyCode::Char('d') if online && self.downloading_title.is_none() => {
                match self.selected_result() {
                    Some(r) => BrowseAction::Download(r.id.clone(), r.title.clone()),
                    None => BrowseAction::None,
                }
            }
            KeyCode::Enter => self
                .selected_index()
                .map_or(BrowseAction::None, BrowseAction::PlayResult),
            KeyCode::Char('a') => self
                .selected_index()
                .map_or(BrowseAction::None, BrowseAction::Enqueue),
            KeyCode::Char('A') => self
                .selected_index()
                .map_or(BrowseAction::None, BrowseAction::PlayNext),
            _ => BrowseAction::None,
        }
    }

    pub fn handle_input_key(&mut self, key: KeyEvent) -> InputAction {
        let search = self.input_purpose == InputPurpose::Search;
        match key.code {
            KeyCode::Esc => {
                self.mode = Mode::Browse;
                InputAction::None
            }
            KeyCode::Enter => {
                let text = self.input_text.trim().to_string();
                if text.is_empty() {
                    return InputAction::None;
                }
                self.mode = Mode::Browse;
                if !search {
                    return InputAction::Submit(self.input_purpose, text);
                }
                if self.input_history.last() != Some(&text) {
                    self.input_history.push(text.clone());
                }
                self.history_index = self.input_history.len();
                if let Some(cmd) = text.strip_prefix(':') {
                    return InputAction::Colon(cmd.trim().to_string());
                }
                self.page = 0;
                InputAction::Submit(InputPurpose::Search, text)
            }
            KeyCode::Backspace => {
                self.delete_before_cursor();
                InputAction::None
            }
            KeyCode::Delete => {
                if let Some((idx, _)) = self.input_text.char_indices().nth(self.input_cursor) {
                    self.input_text.remove(idx);
                }
                InputAction::None
            }
            KeyCode::Left => {
                self.input_cursor = self.input_cursor.saturating_sub(1);
                InputAction::None
            }
            KeyCode::Right => {
                if self.input_cursor < self.input_text.chars().count() {
                    self.input_cursor += 1;
                }
                InputAction::None
            }
            KeyCode::Home => {
                self.input_cursor = 0;
                InputAction::None
            }
            KeyCode::End => {
                self.input_cursor = self.input_text.chars().count();
                InputAction::None
            }
            KeyCode::Up if search => {
                if self.history_index > 0 {
                    self.history_index -= 1;
                    self.input_text = self.input_history[self.history_index].clone();
                    self.input_cursor = self.input_text.chars().count();
                }
                InputAction::None
            }
            KeyCode::Down if search => {
                if self.history_index + 1 < self.input_history.len() {
                    self.history_index += 1;
                    self.input_text = self.input_history[self.history_index].clone();
                } else {
                    self.history_index = self.input_history.len();
                    self.input_text.clear();
                }
                self.input_cursor = self.input_text.chars().count();
                InputAction::None
            }
            KeyCode::Char(c) if key.modifiers.contains(KeyModifiers::CONTROL) => {
                match c {
                    'a' => self.input_cursor = 0,
                    'e' => self.input_cursor = self.input_text.chars().count(),
                    'u' => {
                        self.input_text.clear();
                        self.input_cursor = 0;
                    }
                    _ => {}
                }
                InputAction::None
            }
            KeyCode::Char(c) => {
                self.insert_char(c);
                InputAction::None
            }
            _ => InputAction::None,
        }
    }

    pub fn handle_settings_key(&mut self, key: KeyEvent) -> SettingsAction {
        match key.code {
            KeyCode::Esc | KeyCode::Char('q') => {
                self.mode = Mode::Browse;
                SettingsAction::Quit
            }
            KeyCode::Char('j') | KeyCode::Down => {
                self.settings_field = match self.settings_field {
                    SettingsField::MusicDir => SettingsField::Extensions,
                    SettingsField::Extensions => SettingsField::Theme,
                    SettingsField::Theme => SettingsField::PageSize,
                    SettingsField::PageSize => SettingsField::Extractor,
                    SettingsField::Extractor => SettingsField::MusicDir,
                };
                SettingsAction::None
            }
            KeyCode::Char('k') | KeyCode::Up => {
                self.settings_field = match self.settings_field {
                    SettingsField::MusicDir => SettingsField::Extractor,
                    SettingsField::Extensions => SettingsField::MusicDir,
                    SettingsField::Theme => SettingsField::Extensions,
                    SettingsField::PageSize => SettingsField::Theme,
                    SettingsField::Extractor => SettingsField::PageSize,
                };
                SettingsAction::None
            }
            KeyCode::Enter if self.settings_field == SettingsField::Theme => {
                let builtins = Theme::builtins();
                let current_idx = builtins
                    .iter()
                    .position(|t| t.name == self.theme.name)
                    .unwrap_or(0);
                self.theme = builtins[(current_idx + 1) % builtins.len()].clone();
                self.config.theme = self.theme.name.clone();
                if !self.is_winamp() {
                    self.winamp_skin = None;
                }
                SettingsAction::ThemeChanged
            }
            KeyCode::Char('i')
                if self.settings_field == SettingsField::Theme && self.theme.name == "Winamp" =>
            {
                SettingsAction::OpenSkinBrowserLocal
            }
            KeyCode::Char('o')
                if self.settings_field == SettingsField::Theme && self.theme.name == "Winamp" =>
            {
                SettingsAction::OpenSkinBrowserOnline
            }
            _ => SettingsAction::None,
        }
    }

    pub fn handle_onboarding_key(&mut self, key: KeyEvent) -> OnboardingAction {
        match self.onboarding_step {
            OnboardingStep::Welcome => match key.code {
                KeyCode::Enter | KeyCode::Char(' ') => {
                    self.onboarding_step = OnboardingStep::Dependencies;
                    OnboardingAction::Next
                }
                _ => OnboardingAction::None,
            },
            OnboardingStep::Dependencies => match key.code {
                KeyCode::Char('j' | 'k') | KeyCode::Down | KeyCode::Up => {
                    self.onboarding_dep_selected =
                        (self.onboarding_dep_selected + 1) % Dep::ALL.len();
                    OnboardingAction::None
                }
                KeyCode::Char('i') => {
                    self.pending_install = Dep::ALL.get(self.onboarding_dep_selected).copied();
                    OnboardingAction::None
                }
                KeyCode::Enter | KeyCode::Char(' ') => {
                    self.onboarding_step = OnboardingStep::MusicDir;
                    OnboardingAction::Next
                }
                _ => OnboardingAction::None,
            },
            OnboardingStep::MusicDir => match key.code {
                KeyCode::Enter => {
                    let dir = if self.input_text.trim().is_empty() {
                        self.config.music_dir.to_string_lossy().to_string()
                    } else {
                        self.input_text.trim().to_string()
                    };
                    self.input_text.clear();
                    self.input_cursor = 0;
                    self.onboarding_step = OnboardingStep::Theme;
                    OnboardingAction::SetMusicDir(dir)
                }
                KeyCode::Esc => {
                    self.input_text.clear();
                    self.input_cursor = 0;
                    self.onboarding_step = OnboardingStep::Theme;
                    OnboardingAction::Next
                }
                KeyCode::Backspace => {
                    self.delete_before_cursor();
                    OnboardingAction::None
                }
                KeyCode::Char(c) => {
                    self.insert_char(c);
                    OnboardingAction::None
                }
                _ => OnboardingAction::None,
            },
            OnboardingStep::Theme => {
                let builtins = Theme::builtins();
                let current_idx = builtins
                    .iter()
                    .position(|t| t.name == self.theme.name)
                    .unwrap_or(0);
                match key.code {
                    KeyCode::Char('j') | KeyCode::Down => {
                        self.theme = builtins[(current_idx + 1) % builtins.len()].clone();
                        OnboardingAction::None
                    }
                    KeyCode::Char('k') | KeyCode::Up => {
                        let prev = current_idx.checked_sub(1).unwrap_or(builtins.len() - 1);
                        self.theme = builtins[prev].clone();
                        OnboardingAction::None
                    }
                    KeyCode::Enter => {
                        self.config.theme = self.theme.name.clone();
                        self.config.onboarding_done = true;
                        self.mode = Mode::Browse;
                        OnboardingAction::SelectTheme
                    }
                    _ => OnboardingAction::None,
                }
            }
        }
    }

    pub fn handle_skin_browser_key(&mut self, key: KeyEvent) -> SkinBrowserAction {
        // Search input mode — capture all keys
        if self.skin_search_active {
            return match key.code {
                KeyCode::Esc => {
                    self.skin_search_active = false;
                    self.skin_search_query.clear();
                    SkinBrowserAction::None
                }
                KeyCode::Enter => {
                    let query = self.skin_search_query.trim().to_string();
                    self.skin_search_active = false;
                    if query.is_empty() {
                        SkinBrowserAction::None
                    } else {
                        SkinBrowserAction::Search(query)
                    }
                }
                KeyCode::Backspace => {
                    self.skin_search_query.pop();
                    SkinBrowserAction::None
                }
                KeyCode::Char(c) => {
                    self.skin_search_query.push(c);
                    SkinBrowserAction::None
                }
                _ => SkinBrowserAction::None,
            };
        }

        let len = self.skin_entries.len();
        match key.code {
            KeyCode::Esc | KeyCode::Char('q') => {
                self.mode = Mode::Settings;
                SkinBrowserAction::Back
            }
            KeyCode::Char('j') | KeyCode::Down if len > 0 => {
                let i = match self.skin_list_state.selected() {
                    Some(i) if i + 1 >= len => 0,
                    Some(i) => i + 1,
                    None => 0,
                };
                self.skin_list_state.select(Some(i));
                SkinBrowserAction::None
            }
            KeyCode::Char('k') | KeyCode::Up if len > 0 => {
                let i = match self.skin_list_state.selected() {
                    Some(0) | None => len - 1,
                    Some(i) => i - 1,
                };
                self.skin_list_state.select(Some(i));
                SkinBrowserAction::None
            }
            KeyCode::Char('g') | KeyCode::Home if len > 0 => {
                self.skin_list_state.select(Some(0));
                SkinBrowserAction::None
            }
            KeyCode::Char('G') | KeyCode::End if len > 0 => {
                self.skin_list_state.select(Some(len - 1));
                SkinBrowserAction::None
            }
            KeyCode::Enter => {
                let Some(entry) = self
                    .skin_list_state
                    .selected()
                    .and_then(|i| self.skin_entries.get(i))
                else {
                    return SkinBrowserAction::None;
                };
                if entry.is_local {
                    SkinBrowserAction::LoadLocal(entry.filename.clone())
                } else if self.skin_downloading_md5.is_none() {
                    let action =
                        SkinBrowserAction::Download(entry.md5.clone(), entry.filename.clone());
                    self.skin_downloading_md5 = Some(entry.md5.clone());
                    action
                } else {
                    SkinBrowserAction::None
                }
            }
            KeyCode::Char('n')
                if self.skin_browser_source == SkinBrowserSource::Online
                    && self.skin_browser_has_more
                    && !self.skin_browser_loading =>
            {
                SkinBrowserAction::RequestFetch
            }
            KeyCode::Char('/') if self.skin_browser_source == SkinBrowserSource::Online => {
                self.skin_search_active = true;
                self.skin_search_query.clear();
                SkinBrowserAction::None
            }
            _ => SkinBrowserAction::None,
        }
    }

    // ── mouse ────────────────────────────────────────────────────────────

    /// Row in the list body under `row`, accounting for the border and the
    /// list's scroll offset.
    fn list_row(&self, row: u16, offset: usize) -> Option<usize> {
        let area = self.layout_rects.results;
        let top = area.y + 1;
        let bottom = area.y + area.height.saturating_sub(1);
        (top..bottom)
            .contains(&row)
            .then(|| offset + usize::from(row - top))
    }

    pub fn handle_mouse(&mut self, event: MouseEvent) -> MouseAction {
        let pos = Position::new(event.column, event.row);
        let hit = |r: Rect| r.area() > 0 && r.contains(pos);
        match event.kind {
            MouseEventKind::ScrollUp | MouseEventKind::ScrollDown => {
                let down = matches!(event.kind, MouseEventKind::ScrollDown);
                if self.show_playlist {
                    let len = self.queue.len();
                    let cursor = self.queue.cursor();
                    if len == 0 {
                        return MouseAction::None;
                    }
                    let next = if down {
                        (cursor + 1).min(len - 1)
                    } else {
                        cursor.saturating_sub(1)
                    };
                    return MouseAction::Cmd(Command::Select(next));
                }
                if let Some(browser) = self.browser_mut() {
                    browser.scroll(down);
                    return MouseAction::None;
                }
                if down {
                    self.select_next();
                } else {
                    self.select_prev();
                }
                MouseAction::None
            }
            MouseEventKind::Down(MouseButton::Left) => {
                let rects = &self.layout_rects;
                if hit(rects.pause_button) && self.playback.is_some() {
                    return MouseAction::Cmd(Command::TogglePause);
                }
                if hit(rects.prev_page) {
                    return MouseAction::PrevPage;
                }
                if hit(rects.next_page) {
                    return MouseAction::NextPage;
                }
                if hit(rects.results) {
                    if self.show_playlist {
                        return match self.list_row(event.row, self.queue_list_state.offset()) {
                            Some(i) if i < self.queue.len() => {
                                MouseAction::Cmd(Command::PlayIndex(i))
                            }
                            _ => MouseAction::None,
                        };
                    }
                    if matches!(self.tab, Tab::Provider(_)) {
                        let subs = self.config.providers.podcast.subscriptions.clone();
                        let rows = self.layout_rects.results;
                        let Some(browser) = self.browser_mut() else {
                            return MouseAction::None;
                        };
                        let top = rows.y + 1;
                        let bottom = rows.y + rows.height.saturating_sub(1);
                        if !(top..bottom).contains(&event.row) {
                            return MouseAction::None;
                        }
                        let i = browser.offset() + usize::from(event.row - top);
                        if i >= browser.len() {
                            return MouseAction::None;
                        }
                        return MouseAction::Browse(browser.activate(i, &subs));
                    }
                    return match self.list_row(event.row, self.list_state.offset()) {
                        Some(i) if i < self.results.len() => {
                            self.list_state.select(Some(i));
                            MouseAction::PlayResult(i)
                        }
                        _ => MouseAction::None,
                    };
                }
                let bar = rects.player_bar;
                if hit(bar) && self.playback.is_some() {
                    let frac = f32::from(event.column - bar.x) / f32::from(bar.width.max(1));
                    return MouseAction::Cmd(Command::SeekFrac(frac.clamp(0.0, 1.0)));
                }
                MouseAction::None
            }
            _ => MouseAction::None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::media::SourceKind;
    use crate::view::RepeatMode;

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    fn shift(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::SHIFT)
    }

    fn item(n: usize) -> MediaItem {
        MediaItem {
            id: format!("/m/{n}.mp3"),
            title: format!("t{n}"),
            artist: (n == 0).then(|| "Band".to_string()),
            album: None,
            duration: Some(60),
            subtitle: None,
            source: SourceKind::Local,
        }
    }

    fn app() -> App {
        let mut app = App::new(Config::default());
        app.mode = Mode::Browse;
        app.results = (0..3).map(item).collect();
        app
    }

    fn cmd(action: BrowseAction) -> Option<Command> {
        match action {
            BrowseAction::Cmd(c) => Some(c),
            _ => None,
        }
    }

    #[test]
    fn transport_keys_map_to_commands() {
        let mut a = app();
        let cases = [
            (key(KeyCode::Char('z')), Command::Prev),
            (key(KeyCode::Char('x')), Command::Play),
            (key(KeyCode::Char('c')), Command::TogglePause),
            (key(KeyCode::Char('v')), Command::Stop),
            (key(KeyCode::Char('b')), Command::Next),
            (key(KeyCode::Char(' ')), Command::TogglePause),
            (key(KeyCode::Left), Command::SeekBy(-5)),
            (key(KeyCode::Right), Command::SeekBy(5)),
            (shift(KeyCode::Left), Command::SeekBy(-30)),
            (shift(KeyCode::Right), Command::SeekBy(30)),
            (key(KeyCode::Char('+')), Command::VolumeBy(0.05)),
            (key(KeyCode::Char('=')), Command::VolumeBy(0.05)),
            (key(KeyCode::Char('-')), Command::VolumeBy(-0.05)),
            (shift(KeyCode::Char('S')), Command::ToggleShuffle),
            (shift(KeyCode::Char('R')), Command::CycleRepeat),
            (key(KeyCode::Char('e')), Command::ToggleEqWindow),
            (shift(KeyCode::Char('P')), Command::TogglePlaylistWindow),
            (key(KeyCode::Char('l')), Command::Eject),
            (shift(KeyCode::Char('V')), Command::CycleVis),
            (key(KeyCode::Char('y')), Command::ToggleLyrics),
            (key(KeyCode::Char('q')), Command::Quit),
            (
                KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL),
                Command::Quit,
            ),
        ];
        for (k, expected) in cases {
            assert_eq!(cmd(a.handle_browse_key(k)), Some(expected), "{k:?}");
        }
    }

    #[test]
    fn enter_and_a_act_on_selected_result() {
        let mut a = app();
        a.list_state.select(Some(2));
        assert!(matches!(
            a.handle_browse_key(key(KeyCode::Enter)),
            BrowseAction::PlayResult(2)
        ));
        assert!(matches!(
            a.handle_browse_key(key(KeyCode::Char('a'))),
            BrowseAction::Enqueue(2)
        ));
        assert!(matches!(
            a.handle_browse_key(shift(KeyCode::Char('A'))),
            BrowseAction::PlayNext(2)
        ));
        a.results.clear();
        assert!(matches!(
            a.handle_browse_key(key(KeyCode::Enter)),
            BrowseAction::None
        ));
    }

    #[test]
    fn paging_only_online() {
        let mut a = app();
        assert!(matches!(
            a.handle_browse_key(key(KeyCode::Char('n'))),
            BrowseAction::None
        ));
        a.tab = Tab::Online;
        assert!(matches!(
            a.handle_browse_key(key(KeyCode::Char('n'))),
            BrowseAction::NextPage
        ));
        assert!(matches!(
            a.handle_browse_key(key(KeyCode::Char('p'))),
            BrowseAction::PrevPage
        ));
    }

    #[test]
    fn playlist_window_keys_drive_the_queue() {
        let mut a = app();
        a.queue.replace((0..3).map(item).collect(), None);
        a.show_playlist = true;
        assert_eq!(
            cmd(a.handle_browse_key(key(KeyCode::Char('j')))),
            Some(Command::Select(1))
        );
        a.queue.set_cursor(2);
        assert_eq!(
            cmd(a.handle_browse_key(key(KeyCode::Char('j')))),
            Some(Command::Select(2))
        );
        assert_eq!(
            cmd(a.handle_browse_key(key(KeyCode::Enter))),
            Some(Command::PlayIndex(2))
        );
        assert!(matches!(
            a.handle_browse_key(shift(KeyCode::Char('K'))),
            BrowseAction::MoveInQueue { from: 2, to: 1 }
        ));
        assert!(matches!(
            a.handle_browse_key(shift(KeyCode::Char('J'))),
            BrowseAction::None
        ));
        assert!(matches!(
            a.handle_browse_key(key(KeyCode::Delete)),
            BrowseAction::RemoveFromQueue(2)
        ));
    }

    #[test]
    fn prompt_purposes_and_colon_commands() {
        let mut a = app();
        a.handle_browse_key(key(KeyCode::Char('w')));
        assert_eq!(a.mode, Mode::Input);
        assert_eq!(a.input_purpose, InputPurpose::SavePlaylist);
        a.insert_text("mix\n1");
        assert!(matches!(
            a.handle_input_key(key(KeyCode::Enter)),
            InputAction::Submit(InputPurpose::SavePlaylist, s) if s == "mix1"
        ));
        assert!(a.input_history.is_empty(), "only searches enter history");

        a.handle_browse_key(key(KeyCode::Char('/')));
        a.insert_text(":eq rock");
        assert!(matches!(
            a.handle_input_key(key(KeyCode::Enter)),
            InputAction::Colon(s) if s == "eq rock"
        ));
        assert_eq!(a.mode, Mode::Browse);
    }

    #[test]
    fn input_editing_handles_multibyte() {
        let mut a = app();
        a.open_prompt(InputPurpose::Search);
        a.insert_text("héllo");
        a.handle_input_key(key(KeyCode::Left));
        a.handle_input_key(key(KeyCode::Backspace));
        assert_eq!(a.input_text, "hélo");
        a.handle_input_key(key(KeyCode::Home));
        a.handle_input_key(key(KeyCode::Delete));
        assert_eq!(a.input_text, "élo");
        a.handle_input_key(KeyEvent::new(KeyCode::Char('u'), KeyModifiers::CONTROL));
        assert!(a.input_text.is_empty());
    }

    #[test]
    fn player_view_reflects_queue_and_playback() {
        let mut a = app();
        a.queue.replace((0..3).map(item).collect(), Some(1));
        a.queue.set_repeat(RepeatMode::All);
        a.volume = 0.5;
        let v = a.player_view();
        assert_eq!(v.status, PlayStatus::Stopped);
        assert_eq!(v.title, "t1");
        assert_eq!(v.duration, Some(Duration::from_secs(60)));
        assert_eq!(v.current, Some(1));
        assert_eq!(v.playlist.len(), 3);
        assert_eq!(v.repeat, RepeatMode::All);
        assert_eq!(v.volume, 0.5);
        assert!(v.spectrum.iter().all(|&b| b == 0.0), "silent analyzer");

        a.playback = Some(PlaybackState {
            title: display_title(&item(0)),
            duration: None,
            elapsed: Duration::from_secs(7),
            paused: true,
            format: StreamFormat::default(),
        });
        let v = a.player_view();
        assert_eq!(v.status, PlayStatus::Paused);
        assert_eq!(v.title, "Band - t0");
        assert_eq!(v.elapsed, Duration::from_secs(7));
        assert_eq!(v.duration, None);
    }

    #[test]
    fn info_status_expires() {
        let mut a = app();
        assert_eq!(a.wants_tick(), None);
        a.info("hello");
        assert!(a.wants_tick().is_some_and(|d| d <= INFO_TTL));
        assert!(!a.on_tick());
        a.status_expires = Some(Instant::now());
        assert!(a.on_tick());
        assert!(matches!(a.status, Status::Idle));
        a.error("bad");
        assert_eq!(a.wants_tick(), None, "errors stay until replaced");
    }

    #[test]
    fn mouse_click_uses_scroll_offset_and_seek_fraction() {
        let mut a = app();
        a.results = (0..50).map(item).collect();
        a.layout_rects.results = Rect::new(0, 0, 40, 12);
        a.layout_rects.player_bar = Rect::new(0, 20, 100, 1);
        *a.list_state.offset_mut() = 10;
        let click = |col: u16, row: u16| MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: col,
            row,
            modifiers: KeyModifiers::NONE,
        };
        assert!(matches!(
            a.handle_mouse(click(5, 3)),
            MouseAction::PlayResult(12)
        ));
        assert!(matches!(a.handle_mouse(click(5, 0)), MouseAction::None));
        a.playback = Some(PlaybackState {
            title: String::new(),
            duration: Some(Duration::from_secs(100)),
            elapsed: Duration::ZERO,
            paused: false,
            format: StreamFormat::default(),
        });
        match a.handle_mouse(click(25, 20)) {
            MouseAction::Cmd(Command::SeekFrac(f)) => assert!((f - 0.25).abs() < 1e-6),
            _ => panic!("expected seek"),
        }
    }

    #[test]
    fn result_selection_wraps_and_survives_shrinking() {
        let mut a = app();
        a.select_prev();
        assert_eq!(a.list_state.selected(), Some(2));
        a.select_next();
        assert_eq!(a.list_state.selected(), Some(0));
        a.list_state.select(Some(9));
        assert!(a.selected_result().is_none());
    }

    #[test]
    fn tab_keys_and_eq_panel_capture() {
        let mut a = app();
        assert!(matches!(
            a.handle_browse_key(key(KeyCode::Tab)),
            BrowseAction::SwitchTab(true)
        ));
        assert!(matches!(
            a.handle_browse_key(key(KeyCode::BackTab)),
            BrowseAction::SwitchTab(false)
        ));
        assert_eq!(a.tabs[..2], [Tab::Local, Tab::Online]);
        assert!(a.tabs.contains(&Tab::Provider(ProviderKind::Radio)));
        assert!(!a.tabs.contains(&Tab::Provider(ProviderKind::Subsonic)));

        a.show_eq = true;
        // Arrows move the EQ focus instead of seeking.
        assert!(matches!(
            a.handle_browse_key(key(KeyCode::Right)),
            BrowseAction::None
        ));
        assert_eq!(a.eq_focus, 1);
        assert_eq!(
            cmd(a.handle_browse_key(key(KeyCode::Up))),
            Some(Command::SetEqBand { band: 0, db: 1.0 })
        );
        assert_eq!(
            cmd(a.handle_browse_key(key(KeyCode::Esc))),
            Some(Command::ToggleEqWindow)
        );
        // Transport keys still work with the panel open.
        assert_eq!(
            cmd(a.handle_browse_key(key(KeyCode::Char('b')))),
            Some(Command::Next)
        );
    }

    #[test]
    fn analyzer_follows_theme_and_width() {
        let mut a = app();
        a.fit_vis(40);
        assert_eq!(a.analyzer.bands(), VisMode::Bars.bands_for_width(40));
        a.theme = Theme::from_name("Winamp");
        a.sync_analyzer();
        assert_eq!(a.analyzer.bands(), AnalyzerConfig::winamp().bands);
    }
}
