//! UI-facing application state and key/mouse → action mapping. Player
//! actions come out as `Command`s; the controller applies them.

use std::path::PathBuf;
use std::time::{Duration, Instant};

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use ratatui::layout::{Position, Rect};
use ratatui::widgets::ListState;

use crate::command::Command;
use crate::config::Config;
use crate::install::{Dep, Deps};
use crate::media::{MediaItem, SourceKind};
use crate::queue::Queue;
use crate::skin::WinampSkin;
use crate::theme::Theme;
use crate::ui::skin_layout::SkinLayout;
use crate::view::{EqSettings, PlayStatus, PlayerView, StreamFormat};

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
    pub duration_secs: u64,
    pub elapsed_secs: u64,
    pub paused: bool,
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
    SwitchSource,
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
    pub skin_layout: Option<SkinLayout>,
    /// File of the loaded skin (`None` = built-in colours).
    pub skin_path: Option<PathBuf>,
    pub active_source: SourceKind,
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
    pub show_playlist: bool,
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
            skin_layout: None,
            skin_path: None,
            active_source: SourceKind::Local,
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
            show_playlist: false,
            deps: Deps::default(),
        }
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
        let (title, elapsed, duration) = match &self.playback {
            Some(pb) => (
                pb.title.as_str(),
                Duration::from_secs(pb.elapsed_secs),
                (pb.duration_secs > 0).then(|| Duration::from_secs(pb.duration_secs)),
            ),
            None => (
                current.map_or("", |i| i.title.as_str()),
                Duration::ZERO,
                current.and_then(|i| i.duration).map(Duration::from_secs),
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
            format: StreamFormat::default(),
            spectrum: &[],
            scope: &[],
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
        if let Some(cmd) = Self::transport_command(&key) {
            return BrowseAction::Cmd(cmd);
        }
        match key.code {
            KeyCode::Char('/') => {
                self.open_prompt(InputPurpose::Search);
                BrowseAction::None
            }
            KeyCode::Char('w') => {
                self.open_prompt(InputPurpose::SavePlaylist);
                BrowseAction::None
            }
            KeyCode::Char('s') => BrowseAction::OpenSettings,
            KeyCode::Tab => BrowseAction::SwitchSource,
            _ if self.show_playlist => self.playlist_key(key),
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
        let online = matches!(self.active_source, SourceKind::Extractor(_));
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
                if self.theme.name != "Winamp" {
                    self.winamp_skin = None;
                    self.skin_layout = None;
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
        a.active_source = SourceKind::Extractor("ytdlp".into());
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
        assert!(v.spectrum.is_empty() && v.scope.is_empty());

        a.playback = Some(PlaybackState {
            title: display_title(&item(0)),
            duration_secs: 0,
            elapsed_secs: 7,
            paused: true,
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
            duration_secs: 100,
            elapsed_secs: 0,
            paused: false,
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
}
