use crossterm::event::{MouseButton, MouseEvent, MouseEventKind};
use ratatui::layout::{Position, Rect};
use ratatui::widgets::ListState;

use crate::config::Config;
use crate::media::{MediaItem, SourceKind};
use crate::skin::WinampSkin;
use crate::theme::Theme;
use crate::ui::skin_layout::SkinLayout;

#[derive(Debug, Clone, PartialEq)]
pub enum Mode {
    Browse,
    Input,
    Settings,
    Onboarding,
    SkinBrowser,
}

#[derive(Debug, Clone)]
pub enum Status {
    Idle,
    Searching(String),
    Loading(String),
    Scanning(String),
    Downloading(String),
    Error(String),
}

#[derive(Debug, Clone)]
pub struct PlaybackState {
    pub title: String,
    pub duration_secs: u64,
    pub elapsed_secs: u64,
    pub paused: bool,
}

/// Stored layout rects for mouse hit-testing.
#[derive(Debug, Clone, Default)]
#[allow(dead_code)]
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

/// Actions returned by mouse handler.
#[allow(dead_code)]
pub enum MouseAction {
    None,
    SelectResult(usize),
    PlaySelected,
    Seek(f64),
    TogglePause,
    PrevPage,
    NextPage,
    ScrollUp,
    ScrollDown,
}

/// Onboarding step (first-run only).
#[derive(Debug, Clone, PartialEq)]
pub enum OnboardingStep {
    Welcome,
    Dependencies,
    MusicDir,
    Theme,
}

/// Settings field being edited.
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
#[allow(dead_code)]
pub struct SkinEntry {
    pub md5: String,
    pub filename: String,
    pub display_name: String,
    pub is_local: bool,
    pub nsfw: bool,
    pub average_color: Option<String>,
}

/// Actions returned by skin browser key handler.
pub enum SkinBrowserAction {
    None,
    Back,
    Download(String, String), // md5, filename
    LoadLocal(String),        // filename
    RequestFetch,
    Search(String),
}

/// Which skin browser mode is active.
#[derive(Debug, Clone, PartialEq)]
pub enum SkinBrowserSource {
    Local,
    Online,
}

pub struct App {
    pub mode: Mode,
    pub results: Vec<MediaItem>,
    pub list_state: ListState,
    pub page: usize,
    pub input_text: String,
    pub input_cursor: usize,
    pub input_history: Vec<String>,
    pub history_index: usize,
    pub playback: Option<PlaybackState>,
    pub status: Status,
    pub should_quit: bool,
    pub mpv_kill: Option<tokio::sync::oneshot::Sender<()>>,
    pub layout_rects: LayoutRects,
    // New fields
    pub config: Config,
    pub theme: Theme,
    pub winamp_skin: Option<WinampSkin>,
    pub skin_layout: Option<SkinLayout>,
    pub active_source: SourceKind,
    pub onboarding_step: OnboardingStep,
    pub onboarding_dep_selected: usize, // 0 = mpv, 1 = yt-dlp
    pub pending_install: Option<usize>,  // Some(0) = mpv, Some(1) = yt-dlp
    pub settings_field: SettingsField,
    #[allow(dead_code)]
    pub settings_list_state: ListState,
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
}

// Actions returned by key handlers
pub enum BrowseAction {
    None,
    Play(String, String), // id, title
    NextPage,
    PrevPage,
    TogglePause,
    OpenSettings,
    SwitchSource,
    Download(String, String), // id, title
}

pub enum InputAction {
    None,
    Search(String),
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
    SelectTheme(String),
}

impl App {
    pub fn new(config: Config) -> Self {
        let theme = Theme::from_name(&config.theme);
        let mut list_state = ListState::default();
        list_state.select(Some(0));
        let mut settings_list_state = ListState::default();
        settings_list_state.select(Some(0));
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
            page: 0,
            input_text: String::new(),
            input_cursor: 0,
            input_history: Vec::new(),
            history_index: 0,
            playback: None,
            status: Status::Idle,
            should_quit: false,
            mpv_kill: None,
            layout_rects: LayoutRects::default(),
            config,
            theme,
            winamp_skin: None, // loaded lazily when Winamp theme is selected
            skin_layout: None,
            active_source: SourceKind::Local,
            onboarding_step: OnboardingStep::Welcome,
            onboarding_dep_selected: 0,
            pending_install: None,
            settings_field: SettingsField::MusicDir,
            settings_list_state,
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
        }
    }

    pub fn select_next(&mut self) {
        if self.results.is_empty() {
            return;
        }
        let i = match self.list_state.selected() {
            Some(i) if i >= self.results.len() - 1 => 0,
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
            Some(0) => self.results.len() - 1,
            Some(i) => i - 1,
            None => 0,
        };
        self.list_state.select(Some(i));
    }

    pub fn select_first(&mut self) {
        if !self.results.is_empty() {
            self.list_state.select(Some(0));
        }
    }

    pub fn select_last(&mut self) {
        if !self.results.is_empty() {
            self.list_state.select(Some(self.results.len() - 1));
        }
    }

    pub fn selected_result(&self) -> Option<&MediaItem> {
        self.list_state.selected().map(|i| &self.results[i])
    }

    pub fn kill_mpv(&mut self) {
        if let Some(kill) = self.mpv_kill.take() {
            let _ = kill.send(());
        }
        self.playback = None;
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

    pub fn handle_browse_key(
        &mut self,
        key: crossterm::event::KeyEvent,
    ) -> BrowseAction {
        use crossterm::event::{KeyCode, KeyModifiers};

        if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('c') {
            self.kill_mpv();
            self.should_quit = true;
            return BrowseAction::None;
        }

        match key.code {
            KeyCode::Char('q') => {
                self.kill_mpv();
                self.should_quit = true;
                BrowseAction::None
            }
            KeyCode::Char('/') => {
                self.mode = Mode::Input;
                self.input_text.clear();
                self.input_cursor = 0;
                BrowseAction::None
            }
            KeyCode::Char('j') | KeyCode::Down => {
                self.select_next();
                BrowseAction::None
            }
            KeyCode::Char('k') | KeyCode::Up => {
                self.select_prev();
                BrowseAction::None
            }
            KeyCode::Char('g') | KeyCode::Home => {
                self.select_first();
                BrowseAction::None
            }
            KeyCode::Char('G') | KeyCode::End => {
                self.select_last();
                BrowseAction::None
            }
            KeyCode::Char('n') => BrowseAction::NextPage,
            KeyCode::Char('p') => BrowseAction::PrevPage,
            KeyCode::Char(' ') => BrowseAction::TogglePause,
            KeyCode::Char('s') | KeyCode::Char('S') => BrowseAction::OpenSettings,
            KeyCode::Tab => BrowseAction::SwitchSource,
            KeyCode::Char('d') => {
                if matches!(self.active_source, SourceKind::Extractor(_))
                    && self.downloading_title.is_none()
                {
                    if let Some(result) = self.selected_result() {
                        return BrowseAction::Download(result.id.clone(), result.title.clone());
                    }
                }
                BrowseAction::None
            }
            KeyCode::Enter => {
                if let Some(result) = self.selected_result() {
                    BrowseAction::Play(result.id.clone(), result.title.clone())
                } else {
                    BrowseAction::None
                }
            }
            _ => BrowseAction::None,
        }
    }

    pub fn handle_input_key(
        &mut self,
        key: crossterm::event::KeyEvent,
    ) -> InputAction {
        use crossterm::event::{KeyCode, KeyModifiers};

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

                if self.input_history.last().map(|s| s.as_str()) != Some(text.as_str()) {
                    self.input_history.push(text.clone());
                }
                self.history_index = self.input_history.len();

                if let Some(cmd) = text.strip_prefix(':') {
                    match cmd.trim() {
                        "q" | "quit" => {
                            self.kill_mpv();
                            self.should_quit = true;
                            return InputAction::None;
                        }
                        _ => {
                            self.status = Status::Error(format!("Unknown command: :{cmd}"));
                            return InputAction::None;
                        }
                    }
                }

                self.mode = Mode::Browse;
                self.page = 0;
                InputAction::Search(text)
            }
            KeyCode::Backspace => {
                if self.input_cursor > 0 {
                    self.input_cursor -= 1;
                    if let Some((idx, _)) = self.input_text.char_indices().nth(self.input_cursor) {
                        self.input_text.remove(idx);
                    }
                }
                InputAction::None
            }
            KeyCode::Delete => {
                let char_count = self.input_text.chars().count();
                if self.input_cursor < char_count {
                    if let Some((idx, _)) = self.input_text.char_indices().nth(self.input_cursor) {
                        self.input_text.remove(idx);
                    }
                }
                InputAction::None
            }
            KeyCode::Left => {
                if self.input_cursor > 0 {
                    self.input_cursor -= 1;
                }
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
            KeyCode::Up => {
                if !self.input_history.is_empty() && self.history_index > 0 {
                    self.history_index -= 1;
                    self.input_text = self.input_history[self.history_index].clone();
                    self.input_cursor = self.input_text.chars().count();
                }
                InputAction::None
            }
            KeyCode::Down => {
                if !self.input_history.is_empty()
                    && self.history_index < self.input_history.len() - 1
                {
                    self.history_index += 1;
                    self.input_text = self.input_history[self.history_index].clone();
                    self.input_cursor = self.input_text.chars().count();
                } else if !self.input_history.is_empty() {
                    self.history_index = self.input_history.len();
                    self.input_text.clear();
                    self.input_cursor = 0;
                }
                InputAction::None
            }
            KeyCode::Char(c) => {
                if key.modifiers.contains(KeyModifiers::CONTROL) {
                    match c {
                        'a' => self.input_cursor = 0,
                        'e' => self.input_cursor = self.input_text.chars().count(),
                        'u' => {
                            self.input_text.clear();
                            self.input_cursor = 0;
                        }
                        _ => {}
                    }
                    return InputAction::None;
                }

                if let Some((idx, _)) = self.input_text.char_indices().nth(self.input_cursor) {
                    self.input_text.insert(idx, c);
                } else {
                    self.input_text.push(c);
                }
                self.input_cursor += 1;
                InputAction::None
            }
            _ => InputAction::None,
        }
    }

    pub fn handle_settings_key(
        &mut self,
        key: crossterm::event::KeyEvent,
    ) -> SettingsAction {
        use crossterm::event::KeyCode;

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
            KeyCode::Enter => {
                if self.settings_field == SettingsField::Theme {
                    // Always cycle through built-in themes
                    let builtins = Theme::builtins();
                    let current_idx = builtins
                        .iter()
                        .position(|t| t.name == self.theme.name)
                        .unwrap_or(0);
                    let next_idx = (current_idx + 1) % builtins.len();
                    self.theme = builtins[next_idx].clone();
                    self.config.theme = self.theme.name.clone();

                    // Load/unload Winamp skin when switching to/from Winamp theme
                    if self.theme.name == "Winamp" && self.winamp_skin.is_none() {
                        return SettingsAction::ThemeChanged;
                    } else if self.theme.name != "Winamp" {
                        self.winamp_skin = None;
                        self.skin_layout = None;
                    }
                }
                SettingsAction::None
            }
            KeyCode::Char('i') => {
                if self.settings_field == SettingsField::Theme && self.theme.name == "Winamp" {
                    return SettingsAction::OpenSkinBrowserLocal;
                }
                SettingsAction::None
            }
            KeyCode::Char('o') => {
                if self.settings_field == SettingsField::Theme && self.theme.name == "Winamp" {
                    return SettingsAction::OpenSkinBrowserOnline;
                }
                SettingsAction::None
            }
            _ => SettingsAction::None,
        }
    }

    pub fn handle_onboarding_key(
        &mut self,
        key: crossterm::event::KeyEvent,
    ) -> OnboardingAction {
        use crossterm::event::KeyCode;

        match self.onboarding_step {
            OnboardingStep::Welcome => match key.code {
                KeyCode::Enter | KeyCode::Char(' ') => {
                    self.onboarding_step = OnboardingStep::Dependencies;
                    OnboardingAction::Next
                }
                _ => OnboardingAction::None,
            },
            OnboardingStep::Dependencies => match key.code {
                KeyCode::Char('j') | KeyCode::Down => {
                    self.onboarding_dep_selected = (self.onboarding_dep_selected + 1) % 2;
                    OnboardingAction::None
                }
                KeyCode::Char('k') | KeyCode::Up => {
                    self.onboarding_dep_selected = (self.onboarding_dep_selected + 1) % 2;
                    OnboardingAction::None
                }
                KeyCode::Char('i') => {
                    self.pending_install = Some(self.onboarding_dep_selected);
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
                    if self.input_cursor > 0 {
                        self.input_cursor -= 1;
                        if let Some((idx, _)) = self.input_text.char_indices().nth(self.input_cursor) {
                            self.input_text.remove(idx);
                        }
                    }
                    OnboardingAction::None
                }
                KeyCode::Char(c) => {
                    if let Some((idx, _)) = self.input_text.char_indices().nth(self.input_cursor) {
                        self.input_text.insert(idx, c);
                    } else {
                        self.input_text.push(c);
                    }
                    self.input_cursor += 1;
                    OnboardingAction::None
                }
                _ => OnboardingAction::None,
            },
            OnboardingStep::Theme => {
                let builtins = Theme::builtins();
                match key.code {
                    KeyCode::Char('j') | KeyCode::Down => {
                        let current_idx = builtins
                            .iter()
                            .position(|t| t.name == self.theme.name)
                            .unwrap_or(0);
                        let next_idx = (current_idx + 1) % builtins.len();
                        self.theme = builtins[next_idx].clone();
                        OnboardingAction::None
                    }
                    KeyCode::Char('k') | KeyCode::Up => {
                        let current_idx = builtins
                            .iter()
                            .position(|t| t.name == self.theme.name)
                            .unwrap_or(0);
                        let next_idx = if current_idx == 0 {
                            builtins.len() - 1
                        } else {
                            current_idx - 1
                        };
                        self.theme = builtins[next_idx].clone();
                        OnboardingAction::None
                    }
                    KeyCode::Enter => {
                        self.config.theme = self.theme.name.clone();
                        self.config.onboarding_done = true;
                        self.mode = Mode::Browse;
                        OnboardingAction::SelectTheme(self.theme.name.clone())
                    }
                    _ => OnboardingAction::None,
                }
            }
        }
    }

    pub fn handle_skin_browser_key(
        &mut self,
        key: crossterm::event::KeyEvent,
    ) -> SkinBrowserAction {
        use crossterm::event::KeyCode;

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
                    if !query.is_empty() {
                        SkinBrowserAction::Search(query)
                    } else {
                        SkinBrowserAction::None
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

        // Normal browse mode
        match key.code {
            KeyCode::Esc | KeyCode::Char('q') => {
                self.mode = Mode::Settings;
                SkinBrowserAction::Back
            }
            KeyCode::Char('j') | KeyCode::Down => {
                if !self.skin_entries.is_empty() {
                    let i = match self.skin_list_state.selected() {
                        Some(i) if i >= self.skin_entries.len() - 1 => 0,
                        Some(i) => i + 1,
                        None => 0,
                    };
                    self.skin_list_state.select(Some(i));
                }
                SkinBrowserAction::None
            }
            KeyCode::Char('k') | KeyCode::Up => {
                if !self.skin_entries.is_empty() {
                    let i = match self.skin_list_state.selected() {
                        Some(0) => self.skin_entries.len() - 1,
                        Some(i) => i - 1,
                        None => 0,
                    };
                    self.skin_list_state.select(Some(i));
                }
                SkinBrowserAction::None
            }
            KeyCode::Char('g') | KeyCode::Home => {
                if !self.skin_entries.is_empty() {
                    self.skin_list_state.select(Some(0));
                }
                SkinBrowserAction::None
            }
            KeyCode::Char('G') | KeyCode::End => {
                if !self.skin_entries.is_empty() {
                    self.skin_list_state.select(Some(self.skin_entries.len() - 1));
                }
                SkinBrowserAction::None
            }
            KeyCode::Enter => {
                if let Some(i) = self.skin_list_state.selected() {
                    if let Some(entry) = self.skin_entries.get(i) {
                        if entry.is_local {
                            return SkinBrowserAction::LoadLocal(
                                entry.filename.clone(),
                            );
                        } else if self.skin_downloading_md5.is_none() {
                            self.skin_downloading_md5 = Some(entry.md5.clone());
                            return SkinBrowserAction::Download(
                                entry.md5.clone(),
                                entry.filename.clone(),
                            );
                        }
                    }
                }
                SkinBrowserAction::None
            }
            KeyCode::Char('n') => {
                if self.skin_browser_has_more && !self.skin_browser_loading {
                    self.skin_browser_offset += self.skin_entries.len().max(50);
                    return SkinBrowserAction::RequestFetch;
                }
                SkinBrowserAction::None
            }
            KeyCode::Char('/') => {
                self.skin_search_active = true;
                self.skin_search_query.clear();
                SkinBrowserAction::None
            }
            _ => SkinBrowserAction::None,
        }
    }

    pub fn handle_mouse(&mut self, event: MouseEvent) -> MouseAction {
        let col = event.column;
        let row = event.row;

        match event.kind {
            MouseEventKind::ScrollUp => {
                self.select_prev();
                MouseAction::None
            }
            MouseEventKind::ScrollDown => {
                self.select_next();
                MouseAction::None
            }
            MouseEventKind::Down(MouseButton::Left) => {
                let rects = &self.layout_rects;

                if rects.pause_button.area() > 0
                    && rects.pause_button.contains(Position::new(col, row))
                    && self.playback.is_some()
                {
                    return MouseAction::TogglePause;
                }

                if rects.prev_page.area() > 0
                    && rects.prev_page.contains(Position::new(col, row))
                {
                    return MouseAction::PrevPage;
                }

                if rects.next_page.area() > 0
                    && rects.next_page.contains(Position::new(col, row))
                {
                    return MouseAction::NextPage;
                }

                if rects.results.area() > 0 && rects.results.contains(Position::new(col, row)) {
                    let inner_row = row as usize;
                    if inner_row > rects.results.y as usize + 1
                        && inner_row < (rects.results.y + rects.results.height) as usize
                    {
                        let idx = inner_row - rects.results.y as usize - 1;
                        if idx < self.results.len() {
                            self.list_state.select(Some(idx));
                            return MouseAction::PlaySelected;
                        }
                    }
                    return MouseAction::None;
                }

                if rects.player_bar.area() > 0 && rects.player_bar.contains(Position::new(col, row)) {
                    if let Some(ref playback) = self.playback {
                        if playback.duration_secs > 0 {
                            let bar_x = col as f64 - rects.player_bar.x as f64;
                            let ratio = (bar_x / rects.player_bar.width as f64).clamp(0.0, 1.0);
                            let seek_pos = ratio * playback.duration_secs as f64;
                            return MouseAction::Seek(seek_pos);
                        }
                    }
                    return MouseAction::None;
                }

                MouseAction::None
            }
            _ => MouseAction::None,
        }
    }
}
