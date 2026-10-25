//! The one key table. The browse-view key handler, the prompt, the command
//! palette, the hint bars, `--help` and the README tables all read
//! [`BINDINGS`], so bindings and their documentation cannot drift apart.

use std::fmt::{self, Write as _};
use std::sync::LazyLock;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use crate::command::Command;

/// Seek steps in seconds (Shift = large).
const SEEK_SMALL: i64 = 5;
const SEEK_LARGE: i64 = 30;
const VOLUME_STEP: f32 = 0.05;

/// Where a binding is live; a set of contexts.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Ctx(u16);

impl Ctx {
    /// Every browse view (Dark, Light and Winamp).
    pub const GLOBAL: Self = Self(1);
    /// Local / Online result list.
    pub const LIBRARY: Self = Self(1 << 1);
    /// Online result list only.
    pub const ONLINE: Self = Self(1 << 2);
    /// Radio, Podcasts, Subsonic and Jellyfin browsers.
    pub const PROVIDER: Self = Self(1 << 3);
    pub const PODCAST: Self = Self(1 << 4);
    /// Playlist window (`P`).
    pub const PLAYLIST: Self = Self(1 << 5);
    /// EQ panel (`e`); its keys shadow the others while it is open.
    pub const EQ: Self = Self(1 << 6);
    /// Single-line prompt (search, `:` commands, save, open).
    pub const PROMPT: Self = Self(1 << 7);
    /// Command palette.
    pub const PALETTE: Self = Self(1 << 8);
    /// Skin browser (installed and online skins).
    pub const SKINS: Self = Self(1 << 9);
    /// Skin browser showing the online museum.
    pub const SKINS_ONLINE: Self = Self(1 << 10);
    /// The browse-view lists (library, provider tabs, playlist window).
    const LISTS: Self = Self::LIBRARY.or(Self::PROVIDER).or(Self::PLAYLIST);
    /// Every list moved through with `j` / `k`, including the skin browser.
    const MOVABLE: Self = Self::LISTS.or(Self::SKINS);

    pub const fn or(self, other: Self) -> Self {
        Self(self.0 | other.0)
    }

    pub const fn without(self, other: Self) -> Self {
        Self(self.0 & !other.0)
    }

    pub const fn intersects(self, other: Self) -> bool {
        self.0 & other.0 != 0
    }
}

/// A key with the modifiers that must be held.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Key {
    code: KeyCode,
    mods: KeyModifiers,
}

const fn ch(c: char) -> Key {
    key(KeyCode::Char(c))
}

const fn ctrl(c: char) -> Key {
    Key {
        code: KeyCode::Char(c),
        mods: KeyModifiers::CONTROL,
    }
}

const fn key(code: KeyCode) -> Key {
    Key {
        code,
        mods: KeyModifiers::NONE,
    }
}

const fn shift(code: KeyCode) -> Key {
    Key {
        code,
        mods: KeyModifiers::SHIFT,
    }
}

impl Key {
    /// Shift is part of a character (`S`, `?`, `+`) and of Shift+Tab, so it
    /// is ignored there; Ctrl+letter matches either case.
    pub fn matches(&self, event: &KeyEvent) -> bool {
        let mut mods =
            event.modifiers & (KeyModifiers::SHIFT | KeyModifiers::CONTROL | KeyModifiers::ALT);
        let mut code = event.code;
        match code {
            KeyCode::Char(c) => {
                mods.remove(KeyModifiers::SHIFT);
                if mods.contains(KeyModifiers::CONTROL) {
                    code = KeyCode::Char(c.to_ascii_lowercase());
                }
            }
            KeyCode::BackTab => mods.remove(KeyModifiers::SHIFT),
            _ => {}
        }
        code == self.code && mods == self.mods
    }

    /// The plain character key `c` (no modifiers).
    pub fn is_char(&self, c: char) -> bool {
        self.code == KeyCode::Char(c) && self.mods.is_empty()
    }
}

impl fmt::Display for Key {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let ctrl = self.mods.contains(KeyModifiers::CONTROL);
        if ctrl {
            f.write_str("Ctrl+")?;
        }
        if self.mods.contains(KeyModifiers::ALT) {
            f.write_str("Alt+")?;
        }
        if self.mods.contains(KeyModifiers::SHIFT) {
            f.write_str("Shift+")?;
        }
        match self.code {
            KeyCode::Char(' ') => f.write_str("Space"),
            KeyCode::Char(c) if ctrl => write!(f, "{}", c.to_ascii_uppercase()),
            KeyCode::Char(c) => write!(f, "{c}"),
            KeyCode::Left => f.write_str("←"),
            KeyCode::Right => f.write_str("→"),
            KeyCode::Up => f.write_str("↑"),
            KeyCode::Down => f.write_str("↓"),
            KeyCode::Enter => f.write_str("Enter"),
            KeyCode::Esc => f.write_str("Esc"),
            KeyCode::Backspace => f.write_str("Backspace"),
            KeyCode::Delete => f.write_str("Del"),
            KeyCode::Home => f.write_str("Home"),
            KeyCode::End => f.write_str("End"),
            KeyCode::PageUp => f.write_str("PgUp"),
            KeyCode::PageDown => f.write_str("PgDn"),
            KeyCode::Tab => f.write_str("Tab"),
            KeyCode::BackTab => f.write_str("Shift+Tab"),
            other => write!(f, "{other:?}"),
        }
    }
}

/// Everything a key (or the palette) can do. View-dependent actions such as
/// `Down` or `Activate` are routed by the app to the focused list.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    // Playback
    Prev,
    Play,
    TogglePause,
    Stop,
    Next,
    SeekBack,
    SeekForward,
    SeekBackLong,
    SeekForwardLong,
    JumpToTime,
    VolumeUp,
    VolumeDown,
    Shuffle,
    Repeat,
    EqToggle,
    EqPresetPrev,
    EqPresetNext,
    ClearQueue,
    // Panels, themes and general
    EqPanel,
    PlaylistWindow,
    Lyrics,
    Visualizer,
    Theme,
    Open,
    SavePlaylist,
    NextTab,
    PrevTab,
    Settings,
    CommandPrompt,
    Palette,
    Quit,
    // Lists
    Down,
    Up,
    Top,
    Bottom,
    PageDown,
    PageUp,
    Activate,
    Enqueue,
    PlayNext,
    // Library
    Search,
    NextPage,
    PrevPage,
    Download,
    // Provider tabs
    ProviderSearch,
    Back,
    Reload,
    Subscribe,
    // Skin browser
    SkinFilter,
    // Playlist window
    MoveDown,
    MoveUp,
    Remove,
    // EQ panel
    EqSlotPrev,
    EqSlotNext,
    EqRaise,
    EqLower,
    EqRaiseMore,
    EqLowerMore,
    EqReset,
    // Prompt
    Submit,
    Cancel,
    HistoryPrev,
    HistoryNext,
    CursorLeft,
    CursorRight,
    LineStart,
    LineEnd,
    DeleteBack,
    DeleteForward,
    ClearLine,
    // Palette
    PaletteUp,
    PaletteDown,
    PaletteRun,
    PaletteClose,
}

impl Action {
    /// The player command for actions that need no view state.
    pub fn command(self) -> Option<Command> {
        Some(match self {
            Self::Prev => Command::Prev,
            Self::Play => Command::Play,
            Self::TogglePause => Command::TogglePause,
            Self::Stop => Command::Stop,
            Self::Next => Command::Next,
            Self::SeekBack => Command::SeekBy(-SEEK_SMALL),
            Self::SeekForward => Command::SeekBy(SEEK_SMALL),
            Self::SeekBackLong => Command::SeekBy(-SEEK_LARGE),
            Self::SeekForwardLong => Command::SeekBy(SEEK_LARGE),
            Self::VolumeUp => Command::VolumeBy(VOLUME_STEP),
            Self::VolumeDown => Command::VolumeBy(-VOLUME_STEP),
            Self::Shuffle => Command::ToggleShuffle,
            Self::Repeat => Command::CycleRepeat,
            Self::EqToggle => Command::ToggleEq,
            Self::EqPanel => Command::ToggleEqWindow,
            Self::PlaylistWindow => Command::TogglePlaylistWindow,
            Self::Lyrics => Command::ToggleLyrics,
            Self::Visualizer => Command::CycleVis,
            Self::Theme => Command::CycleTheme,
            Self::Open => Command::Eject,
            Self::Quit => Command::Quit,
            _ => return None,
        })
    }
}

/// Groups for the README, `--help` and the palette.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Section {
    Playback,
    General,
    Lists,
    Library,
    Providers,
    Playlist,
    Equalizer,
    Skins,
    Prompt,
    Palette,
}

impl Section {
    pub const ALL: [Self; 10] = [
        Self::Playback,
        Self::General,
        Self::Lists,
        Self::Library,
        Self::Providers,
        Self::Playlist,
        Self::Equalizer,
        Self::Skins,
        Self::Prompt,
        Self::Palette,
    ];

    pub fn title(self) -> &'static str {
        match self {
            Self::Playback => "Playback",
            Self::General => "Panels, themes and general",
            Self::Lists => "Lists (library, provider tabs, playlist window, skin browser)",
            Self::Library => "Library (Local and Online tabs)",
            Self::Providers => "Provider tabs (Radio, Podcasts, Subsonic, Jellyfin)",
            Self::Playlist => "Playlist window (P)",
            Self::Equalizer => "EQ panel (e)",
            Self::Skins => "Skin browser (Settings → Theme Winamp → i or o)",
            Self::Prompt => "Prompt (search, : commands, save, open)",
            Self::Palette => "Command palette (? or Ctrl+K)",
        }
    }
}

pub struct Binding {
    /// Empty = only reachable from the palette.
    pub keys: &'static [Key],
    pub action: Action,
    pub ctx: Ctx,
    pub section: Section,
    pub desc: &'static str,
    /// Short label for the hint bars; empty = not hinted.
    pub hint: &'static str,
}

const fn bind(
    section: Section,
    ctx: Ctx,
    keys: &'static [Key],
    action: Action,
    desc: &'static str,
    hint: &'static str,
) -> Binding {
    Binding {
        keys,
        action,
        ctx,
        section,
        desc,
        hint,
    }
}

use Action as A;
use KeyCode as K;
use Section as S;

const G: Ctx = Ctx::GLOBAL;

pub static BINDINGS: &[Binding] = &[
    // ── Playback ──
    bind(S::Playback, G, &[ch('z')], A::Prev, "Previous track", ""),
    bind(
        S::Playback,
        G,
        &[ch('x')],
        A::Play,
        "Play (restarts a playing track)",
        "",
    ),
    bind(
        S::Playback,
        G,
        &[ch(' '), ch('c')],
        A::TogglePause,
        "Pause / resume",
        "pause",
    ),
    bind(S::Playback, G, &[ch('v')], A::Stop, "Stop", ""),
    bind(S::Playback, G, &[ch('b')], A::Next, "Next track", ""),
    bind(
        S::Playback,
        G,
        &[key(K::Left)],
        A::SeekBack,
        "Seek back 5 s",
        "",
    ),
    bind(
        S::Playback,
        G,
        &[key(K::Right)],
        A::SeekForward,
        "Seek forward 5 s",
        "",
    ),
    bind(
        S::Playback,
        G,
        &[shift(K::Left)],
        A::SeekBackLong,
        "Seek back 30 s",
        "",
    ),
    bind(
        S::Playback,
        G,
        &[shift(K::Right)],
        A::SeekForwardLong,
        "Seek forward 30 s",
        "",
    ),
    bind(
        S::Playback,
        G,
        &[ctrl('j')],
        A::JumpToTime,
        "Jump to a time (:seek)",
        "",
    ),
    bind(
        S::Playback,
        G,
        &[ch('+'), ch('=')],
        A::VolumeUp,
        "Volume up",
        "",
    ),
    bind(S::Playback, G, &[ch('-')], A::VolumeDown, "Volume down", ""),
    bind(S::Playback, G, &[ch('S')], A::Shuffle, "Toggle shuffle", ""),
    bind(
        S::Playback,
        G,
        &[ch('R')],
        A::Repeat,
        "Cycle repeat (off, all, one)",
        "",
    ),
    bind(
        S::Playback,
        G,
        &[ch('o')],
        A::EqToggle,
        "Turn the EQ on or off",
        "",
    ),
    bind(
        S::Playback,
        G,
        &[ch('[')],
        A::EqPresetPrev,
        "Previous EQ preset",
        "",
    ),
    bind(
        S::Playback,
        G,
        &[ch(']')],
        A::EqPresetNext,
        "Next EQ preset",
        "",
    ),
    bind(
        S::Playback,
        G,
        &[],
        A::ClearQueue,
        "Stop and clear the queue",
        "",
    ),
    // ── Panels, themes and general ──
    bind(
        S::General,
        G,
        &[ch('e')],
        A::EqPanel,
        "Toggle the EQ panel",
        "EQ",
    ),
    bind(
        S::General,
        G,
        &[ch('P')],
        A::PlaylistWindow,
        "Toggle the playlist window",
        "playlist",
    ),
    bind(
        S::General,
        G,
        &[ch('y')],
        A::Lyrics,
        "Toggle the lyrics panel",
        "lyrics",
    ),
    bind(
        S::General,
        G,
        &[ch('V')],
        A::Visualizer,
        "Cycle the visualizer mode",
        "",
    ),
    bind(
        S::General,
        G,
        &[ch('t')],
        A::Theme,
        "Cycle the theme (Dark, Light, Winamp)",
        "",
    ),
    bind(
        S::General,
        G,
        &[ch('l')],
        A::Open,
        "Open a file, folder, URL or playlist",
        "",
    ),
    bind(
        S::General,
        G,
        &[ch('w')],
        A::SavePlaylist,
        "Save the queue as an .m3u playlist",
        "",
    ),
    bind(
        S::General,
        G,
        &[key(K::Tab)],
        A::NextTab,
        "Next source tab",
        "source",
    ),
    bind(
        S::General,
        G,
        &[key(K::BackTab)],
        A::PrevTab,
        "Previous source tab",
        "",
    ),
    bind(S::General, G, &[ch('s')], A::Settings, "Settings", ""),
    bind(
        S::General,
        G,
        &[ch(':')],
        A::CommandPrompt,
        "Type a : command (e.g. :theme light)",
        "",
    ),
    bind(
        S::General,
        G,
        &[ch('?'), ctrl('k')],
        A::Palette,
        "Command palette: every action, filterable",
        "commands",
    ),
    bind(
        S::General,
        G,
        &[ch('q'), ctrl('c')],
        A::Quit,
        "Quit",
        "quit",
    ),
    // ── Lists ──
    bind(
        S::Lists,
        Ctx::MOVABLE,
        &[ch('j'), key(K::Down)],
        A::Down,
        "Move down",
        "",
    ),
    bind(
        S::Lists,
        Ctx::MOVABLE,
        &[ch('k'), key(K::Up)],
        A::Up,
        "Move up",
        "",
    ),
    bind(
        S::Lists,
        Ctx::MOVABLE,
        &[ch('g'), key(K::Home)],
        A::Top,
        "First item",
        "",
    ),
    bind(
        S::Lists,
        Ctx::MOVABLE,
        &[ch('G'), key(K::End)],
        A::Bottom,
        "Last item",
        "",
    ),
    bind(
        S::Lists,
        Ctx::MOVABLE,
        &[key(K::PageDown)],
        A::PageDown,
        "Page down",
        "",
    ),
    bind(
        S::Lists,
        Ctx::MOVABLE,
        &[key(K::PageUp)],
        A::PageUp,
        "Page up",
        "",
    ),
    bind(
        S::Lists,
        Ctx::LISTS,
        &[key(K::Enter)],
        A::Activate,
        "Play the selection (library: the list from there; providers: open folders)",
        "play",
    ),
    bind(
        S::Lists,
        Ctx::LIBRARY.or(Ctx::PROVIDER),
        &[ch('a')],
        A::Enqueue,
        "Append the selection to the queue",
        "queue",
    ),
    bind(
        S::Lists,
        Ctx::LIBRARY.or(Ctx::PROVIDER),
        &[ch('A')],
        A::PlayNext,
        "Play the selection next",
        "",
    ),
    // ── Library ──
    bind(
        S::Library,
        Ctx::LIBRARY.or(Ctx::PLAYLIST),
        &[ch('/')],
        A::Search,
        "Search (or type a : command)",
        "search",
    ),
    bind(
        S::Library,
        Ctx::ONLINE,
        &[ch('n')],
        A::NextPage,
        "Next result page (Online)",
        "page",
    ),
    bind(
        S::Library,
        Ctx::ONLINE,
        &[ch('p')],
        A::PrevPage,
        "Previous result page (Online)",
        "",
    ),
    bind(
        S::Library,
        Ctx::ONLINE,
        &[ch('d')],
        A::Download,
        "Download the selection (Online)",
        "download",
    ),
    // ── Provider tabs ──
    bind(
        S::Providers,
        Ctx::PROVIDER,
        &[ch('/')],
        A::ProviderSearch,
        "Search this source",
        "search",
    ),
    bind(
        S::Providers,
        Ctx::PROVIDER,
        &[key(K::Backspace), key(K::Esc)],
        A::Back,
        "Up a level",
        "back",
    ),
    bind(
        S::Providers,
        Ctx::PROVIDER,
        &[ch('r')],
        A::Reload,
        "Reload the current level",
        "reload",
    ),
    bind(
        S::Providers,
        Ctx::PODCAST,
        &[ch('f')],
        A::Subscribe,
        "Subscribe to or unsubscribe from the podcast",
        "subscribe",
    ),
    // ── Playlist window ──
    bind(
        S::Playlist,
        Ctx::PLAYLIST,
        &[ch('J')],
        A::MoveDown,
        "Move the track down",
        "",
    ),
    bind(
        S::Playlist,
        Ctx::PLAYLIST,
        &[ch('K')],
        A::MoveUp,
        "Move the track up",
        "",
    ),
    bind(
        S::Playlist,
        Ctx::PLAYLIST,
        &[ch('D'), key(K::Delete)],
        A::Remove,
        "Remove the track from the queue",
        "remove",
    ),
    // ── EQ panel ──
    bind(
        S::Equalizer,
        Ctx::EQ,
        &[key(K::Left), ch('h')],
        A::EqSlotPrev,
        "Previous slider",
        "",
    ),
    bind(
        S::Equalizer,
        Ctx::EQ,
        &[key(K::Right), ch('l')],
        A::EqSlotNext,
        "Next slider",
        "select",
    ),
    bind(
        S::Equalizer,
        Ctx::EQ,
        &[key(K::Up), ch('k')],
        A::EqRaise,
        "Raise the slider 1 dB",
        "adjust",
    ),
    bind(
        S::Equalizer,
        Ctx::EQ,
        &[key(K::Down), ch('j')],
        A::EqLower,
        "Lower the slider 1 dB",
        "",
    ),
    bind(
        S::Equalizer,
        Ctx::EQ,
        &[ch('K')],
        A::EqRaiseMore,
        "Raise the slider 3 dB",
        "",
    ),
    bind(
        S::Equalizer,
        Ctx::EQ,
        &[ch('J')],
        A::EqLowerMore,
        "Lower the slider 3 dB",
        "",
    ),
    bind(
        S::Equalizer,
        Ctx::EQ,
        &[ch('0')],
        A::EqReset,
        "Reset the slider to 0 dB",
        "reset",
    ),
    bind(
        S::Equalizer,
        Ctx::EQ,
        &[key(K::Esc)],
        A::EqPanel,
        "Close the EQ panel",
        "close",
    ),
    // ── Skin browser ──
    bind(
        S::Skins,
        Ctx::SKINS,
        &[key(K::Enter)],
        A::Activate,
        "Apply the highlighted skin (Online: install it first)",
        "apply",
    ),
    bind(
        S::Skins,
        Ctx::SKINS_ONLINE,
        &[ch('/')],
        A::Search,
        "Search every classic skin in the museum (Online)",
        "search",
    ),
    bind(
        S::Skins,
        Ctx::SKINS_ONLINE,
        &[ch('f')],
        A::SkinFilter,
        "Switch between the whole museum and curated skins (Online)",
        "all/curated",
    ),
    bind(
        S::Skins,
        Ctx::SKINS,
        &[key(K::Tab)],
        A::NextTab,
        "Switch between installed and online skins",
        "installed/online",
    ),
    bind(
        S::Skins,
        Ctx::SKINS,
        &[ch('r')],
        A::Reload,
        "Reload the list",
        "",
    ),
    bind(
        S::Skins,
        Ctx::SKINS,
        &[key(K::Esc), ch('q')],
        A::Back,
        "Clear the search, or go back to Settings",
        "back",
    ),
    // ── Prompt ──
    bind(
        S::Prompt,
        Ctx::PROMPT,
        &[key(K::Enter)],
        A::Submit,
        "Submit",
        "submit",
    ),
    bind(
        S::Prompt,
        Ctx::PROMPT,
        &[key(K::Esc)],
        A::Cancel,
        "Cancel",
        "cancel",
    ),
    bind(
        S::Prompt,
        Ctx::PROMPT,
        &[key(K::Up)],
        A::HistoryPrev,
        "Previous search",
        "history",
    ),
    bind(
        S::Prompt,
        Ctx::PROMPT,
        &[key(K::Down)],
        A::HistoryNext,
        "Next search",
        "",
    ),
    bind(
        S::Prompt,
        Ctx::PROMPT,
        &[key(K::Left)],
        A::CursorLeft,
        "Cursor left",
        "",
    ),
    bind(
        S::Prompt,
        Ctx::PROMPT,
        &[key(K::Right)],
        A::CursorRight,
        "Cursor right",
        "",
    ),
    bind(
        S::Prompt,
        Ctx::PROMPT,
        &[key(K::Home), ctrl('a')],
        A::LineStart,
        "Start of the line",
        "",
    ),
    bind(
        S::Prompt,
        Ctx::PROMPT,
        &[key(K::End), ctrl('e')],
        A::LineEnd,
        "End of the line",
        "",
    ),
    bind(
        S::Prompt,
        Ctx::PROMPT,
        &[key(K::Backspace)],
        A::DeleteBack,
        "Delete before the cursor",
        "",
    ),
    bind(
        S::Prompt,
        Ctx::PROMPT,
        &[key(K::Delete)],
        A::DeleteForward,
        "Delete at the cursor",
        "",
    ),
    bind(
        S::Prompt,
        Ctx::PROMPT,
        &[ctrl('u')],
        A::ClearLine,
        "Clear the input",
        "clear",
    ),
    // ── Command palette ──
    bind(
        S::Palette,
        Ctx::PALETTE,
        &[key(K::Up), ctrl('p')],
        A::PaletteUp,
        "Previous entry",
        "",
    ),
    bind(
        S::Palette,
        Ctx::PALETTE,
        &[key(K::Down), ctrl('n')],
        A::PaletteDown,
        "Next entry",
        "move",
    ),
    bind(
        S::Palette,
        Ctx::PALETTE,
        &[key(K::Enter)],
        A::PaletteRun,
        "Run the selected entry",
        "run",
    ),
    bind(
        S::Palette,
        Ctx::PALETTE,
        &[key(K::Backspace)],
        A::DeleteBack,
        "Delete a filter character",
        "",
    ),
    bind(
        S::Palette,
        Ctx::PALETTE,
        &[ctrl('u')],
        A::ClearLine,
        "Clear the filter",
        "",
    ),
    bind(
        S::Palette,
        Ctx::PALETTE,
        &[key(K::Esc), ctrl('k')],
        A::PaletteClose,
        "Close the palette",
        "close",
    ),
];

static LABELS: LazyLock<Vec<String>> = LazyLock::new(|| {
    BINDINGS
        .iter()
        .map(|b| {
            let mut label = String::new();
            for (i, k) in b.keys.iter().enumerate() {
                if i > 0 {
                    label.push('/');
                }
                let _ = write!(label, "{k}");
            }
            label
        })
        .collect()
});

/// Key labels (`Space/c`, `Shift+←`), index-aligned with [`BINDINGS`].
pub fn labels() -> &'static [String] {
    &LABELS
}

/// The action `event` triggers in `active`. EQ panel keys win over the
/// others while the panel is open.
pub fn lookup(active: Ctx, event: &KeyEvent) -> Option<Action> {
    let find = |ctx: Ctx| {
        BINDINGS
            .iter()
            .find(|b| b.ctx.intersects(ctx) && b.keys.iter().any(|k| k.matches(event)))
            .map(|b| b.action)
    };
    if active.intersects(Ctx::EQ) {
        if let Some(action) = find(Ctx::EQ) {
            return Some(action);
        }
    }
    find(active.without(Ctx::EQ))
}

/// `(keys, hint)` pairs for a hint bar: view-specific bindings first.
pub fn hints(active: Ctx) -> impl Iterator<Item = (&'static str, &'static str)> {
    let pick = move |specific: bool| {
        BINDINGS
            .iter()
            .zip(labels())
            .filter(move |(b, _)| {
                !b.hint.is_empty() && b.ctx.intersects(active) && (b.ctx != Ctx::GLOBAL) == specific
            })
            .map(|(b, label)| (label.as_str(), b.hint))
    };
    pick(true).chain(pick(false))
}

/// The `(keys, hint)` of `action` in `active`, for hint bars showing a subset.
pub fn hint_for(active: Ctx, action: Action) -> Option<(&'static str, &'static str)> {
    BINDINGS
        .iter()
        .zip(labels())
        .find(|(b, _)| b.action == action && b.ctx.intersects(active) && !b.hint.is_empty())
        .map(|(b, label)| (label.as_str(), b.hint))
}

/// The key reference for `--help`.
pub fn write_help(out: &mut String) {
    for section in Section::ALL {
        let _ = writeln!(out, "KEYS ({}):", section.title());
        for (b, label) in BINDINGS.iter().zip(labels()) {
            if b.section != section {
                continue;
            }
            let label = if b.keys.is_empty() {
                "(palette)"
            } else {
                label
            };
            let _ = writeln!(out, "    {label:<16} {}", b.desc);
        }
        out.push('\n');
    }
}

/// The README keybinding tables.
#[cfg(test)]
pub fn markdown() -> String {
    let mut out = String::new();
    for section in Section::ALL {
        let _ = writeln!(
            out,
            "#### {}\n\n| Key | Action |\n|-----|--------|",
            section.title()
        );
        for b in BINDINGS.iter().filter(|b| b.section == section) {
            let keys = if b.keys.is_empty() {
                "*palette*".to_string()
            } else {
                b.keys
                    .iter()
                    .map(|k| format!("`{k}`"))
                    .collect::<Vec<_>>()
                    .join(" / ")
            };
            let _ = writeln!(out, "| {keys} | {} |", b.desc);
        }
        out.push('\n');
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ev(code: KeyCode, mods: KeyModifiers) -> KeyEvent {
        KeyEvent::new(code, mods)
    }

    /// Every context combination the app can be in.
    fn combos() -> Vec<Ctx> {
        let views = [
            Ctx::LIBRARY,
            Ctx::LIBRARY.or(Ctx::ONLINE),
            Ctx::PROVIDER,
            Ctx::PROVIDER.or(Ctx::PODCAST),
            Ctx::PLAYLIST,
        ];
        let mut out = vec![
            Ctx::PROMPT,
            Ctx::PALETTE,
            Ctx::SKINS,
            Ctx::SKINS.or(Ctx::SKINS_ONLINE),
        ];
        for view in views {
            out.push(Ctx::GLOBAL.or(view));
            out.push(Ctx::GLOBAL.or(view).or(Ctx::EQ));
        }
        out
    }

    #[test]
    fn no_key_is_bound_twice_in_one_layer() {
        for active in combos() {
            for layer in [active.without(Ctx::EQ), Ctx::EQ] {
                if !active.intersects(layer) {
                    continue;
                }
                let live: Vec<&Binding> = BINDINGS
                    .iter()
                    .filter(|b| b.ctx.intersects(active) && b.ctx.intersects(layer))
                    .collect();
                for (i, a) in live.iter().enumerate() {
                    for b in &live[i + 1..] {
                        for k in a.keys {
                            assert!(
                                !b.keys.contains(k),
                                "{k} is bound to both '{}' and '{}'",
                                a.desc,
                                b.desc
                            );
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn modifiers_and_shifted_characters_match() {
        let g = Ctx::GLOBAL.or(Ctx::LIBRARY);
        let shifted = ev(KeyCode::Char('S'), KeyModifiers::SHIFT);
        assert_eq!(lookup(g, &shifted), Some(Action::Shuffle));
        assert_eq!(
            lookup(g, &ev(KeyCode::Char('S'), KeyModifiers::NONE)),
            Some(Action::Shuffle)
        );
        assert_eq!(
            lookup(g, &ev(KeyCode::Char('?'), KeyModifiers::SHIFT)),
            Some(Action::Palette)
        );
        assert_eq!(
            lookup(g, &ev(KeyCode::Char('k'), KeyModifiers::CONTROL)),
            Some(Action::Palette)
        );
        assert_eq!(
            lookup(g, &ev(KeyCode::Char('K'), KeyModifiers::CONTROL)),
            Some(Action::Palette)
        );
        assert_eq!(
            lookup(g, &ev(KeyCode::BackTab, KeyModifiers::SHIFT)),
            Some(Action::PrevTab)
        );
        assert_eq!(
            lookup(g, &ev(KeyCode::Left, KeyModifiers::SHIFT)),
            Some(Action::SeekBackLong)
        );
        assert_eq!(
            lookup(g, &ev(KeyCode::Left, KeyModifiers::NONE)),
            Some(Action::SeekBack)
        );
        // Ctrl / Alt never fall back to the plain key.
        assert_eq!(
            lookup(g, &ev(KeyCode::Char('x'), KeyModifiers::CONTROL)),
            None
        );
        assert_eq!(lookup(g, &ev(KeyCode::Char('q'), KeyModifiers::ALT)), None);
    }

    #[test]
    fn eq_panel_keys_shadow_the_rest() {
        let lib = Ctx::GLOBAL.or(Ctx::LIBRARY);
        let l = ev(KeyCode::Char('l'), KeyModifiers::NONE);
        let j = ev(KeyCode::Char('j'), KeyModifiers::NONE);
        assert_eq!(lookup(lib, &l), Some(Action::Open));
        assert_eq!(lookup(lib, &j), Some(Action::Down));
        let eq = lib.or(Ctx::EQ);
        assert_eq!(lookup(eq, &l), Some(Action::EqSlotNext));
        assert_eq!(lookup(eq, &j), Some(Action::EqLower));
        // Keys the panel does not use still reach the player.
        assert_eq!(
            lookup(eq, &ev(KeyCode::Char('b'), KeyModifiers::NONE)),
            Some(Action::Next)
        );
        // Panel keys do nothing when it is closed and nothing else uses them.
        assert_eq!(
            lookup(lib, &ev(KeyCode::Char('0'), KeyModifiers::NONE)),
            None
        );
    }

    #[test]
    fn labels_and_hints() {
        let label = |action: Action| {
            let i = BINDINGS.iter().position(|b| b.action == action).unwrap();
            labels()[i].as_str()
        };
        assert_eq!(label(Action::TogglePause), "Space/c");
        assert_eq!(label(Action::SeekBackLong), "Shift+←");
        assert_eq!(label(Action::Palette), "?/Ctrl+K");
        assert_eq!(label(Action::PrevTab), "Shift+Tab");
        let hints: Vec<_> = hints(Ctx::GLOBAL.or(Ctx::PROVIDER).or(Ctx::PODCAST)).collect();
        assert_eq!(
            hints[0],
            ("Enter", "play"),
            "view-specific hints come first"
        );
        assert!(hints.contains(&("f", "subscribe")));
        assert!(hints.contains(&("?/Ctrl+K", "commands")));
        assert!(!hints.iter().any(|(_, h)| *h == "download"));
    }

    #[test]
    fn skin_browser_keys_are_its_own() {
        let local = Ctx::SKINS;
        let online = Ctx::SKINS.or(Ctx::SKINS_ONLINE);
        let key = |c| ev(KeyCode::Char(c), KeyModifiers::NONE);
        assert_eq!(lookup(online, &key('j')), Some(Action::Down));
        assert_eq!(lookup(online, &key('f')), Some(Action::SkinFilter));
        assert_eq!(lookup(local, &key('f')), None, "online only");
        assert_eq!(lookup(online, &key('q')), Some(Action::Back));
        assert_eq!(lookup(online, &key(' ')), None, "no player keys");
        assert_eq!(
            lookup(online, &ev(KeyCode::Enter, KeyModifiers::NONE)),
            Some(Action::Activate)
        );
        let hints: Vec<_> = hints(online).collect();
        assert_eq!(hints[0], ("Enter", "apply"));
        assert!(hints.contains(&("f", "all/curated")));
        assert!(!hints(local).any(|(_, h)| h == "search"));
        assert_eq!(
            hint_for(Ctx::PROMPT, Action::Submit),
            Some(("Enter", "submit"))
        );
        assert_eq!(hint_for(local, Action::SkinFilter), None);
        // The skin browser's keys never reach the browse views.
        let browse = Ctx::GLOBAL.or(Ctx::LIBRARY);
        assert_ne!(lookup(browse, &key('f')), Some(Action::SkinFilter));
    }

    #[test]
    fn help_lists_every_section() {
        let mut help = String::new();
        write_help(&mut help);
        for section in Section::ALL {
            assert!(help.contains(section.title()), "{}", section.title());
        }
        let clear = help
            .lines()
            .find(|l| l.ends_with("Stop and clear the queue"))
            .unwrap();
        assert!(clear.trim_start().starts_with("(palette)"), "{clear}");
        assert!(help.contains("Space/c"));
    }

    #[test]
    fn readme_keybindings_are_generated_from_the_table() {
        let readme = include_str!("../README.md").replace("\r\n", "\n");
        let expected = markdown();
        let block = readme
            .split_once("<!-- keymap:start -->\n")
            .and_then(|(_, rest)| rest.split_once("<!-- keymap:end -->"))
            .map(|(block, _)| block);
        assert!(
            block == Some(expected.as_str()),
            "README.md keybindings are out of date. Put this between the \
             <!-- keymap:start --> and <!-- keymap:end --> markers:\n\n{expected}"
        );
    }
}
