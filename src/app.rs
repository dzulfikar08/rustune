use ratatui::widgets::ListState;

use crate::youtube::SearchResult;

#[derive(Debug, Clone, PartialEq)]
pub enum Mode {
    Browse,
    Input,
}

#[derive(Debug, Clone)]
pub enum Status {
    Idle,
    Searching(String),
    Loading(String),
    Error(String),
}

#[derive(Debug, Clone)]
pub struct PlaybackState {
    pub title: String,
    pub duration_secs: u64,
    pub elapsed_secs: u64,
    pub paused: bool,
}

pub struct App {
    pub mode: Mode,
    pub results: Vec<SearchResult>,
    pub list_state: ListState,
    pub page: usize,
    pub input_text: String,
    pub input_cursor: usize, // char index within input_text
    pub input_history: Vec<String>,
    pub history_index: usize,
    pub playback: Option<PlaybackState>,
    pub status: Status,
    pub should_quit: bool,
    pub mpv_kill: Option<tokio::sync::oneshot::Sender<()>>,
}

// Actions returned by key handlers
pub enum BrowseAction {
    None,
    Play(String, String), // video_id, title
    NextPage,
    PrevPage,
    TogglePause,
}

pub enum InputAction {
    None,
    Search(String),
}

impl App {
    pub fn new() -> Self {
        let mut list_state = ListState::default();
        list_state.select(Some(0));
        Self {
            mode: Mode::Browse,
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

    pub fn selected_result(&self) -> Option<&SearchResult> {
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

        // Ctrl+C always quits
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

                // Add to history (skip duplicates)
                if self.input_history.last().map(|s| s.as_str()) != Some(text.as_str()) {
                    self.input_history.push(text.clone());
                }
                self.history_index = self.input_history.len();

                // Check for commands (: prefix)
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
                // Handle Ctrl+A, Ctrl+E, Ctrl+U inside Char arm
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

                // Normal character insertion
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
}
