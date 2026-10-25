//! Classic Winamp 2.x compositor and input model.
//!
//! [`WinampUiState`] owns the reusable window framebuffers, the small amount
//! of UI-only state (pressed element, drags, marquee clock, visible windows,
//! visualizer mode) and the screen geometry of the last frame, which it uses
//! to turn terminal mouse events into [`Command`]s.

pub mod canvas;
pub mod compose;
pub mod hit;
pub mod sprites;
pub mod vis;

#[cfg(test)]
mod preview;

use std::time::{Duration, Instant};

use crossterm::event::{MouseButton, MouseEvent, MouseEventKind};
use ratatui::layout::{Position, Rect};

use crate::command::Command;
use crate::skin::WinampSkin;
use crate::view::{PlayStatus, PlayerView};

use canvas::Canvas;
use compose::{EqCtx, MainCtx, PlaylistCtx, RowSpan};
pub use vis::VisMode;
use vis::VisState;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Window {
    Main,
    Equalizer,
    Playlist,
}

impl Window {
    fn index(self) -> usize {
        self as usize
    }
}

/// Every interactive element of the three windows.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Element {
    Options,
    Minimize,
    Shade,
    Close,
    Time,
    Vis,
    Marquee,
    Volume,
    Balance,
    EqButton,
    PlButton,
    Position,
    Prev,
    Play,
    Pause,
    Stop,
    Next,
    Eject,
    Shuffle,
    Repeat,
    EqClose,
    EqShade,
    EqOn,
    EqAuto,
    EqPresets,
    Preamp,
    EqBand(u8),
    PlClose,
    PlTracks,
    PlScroll,
    PlAdd,
    /// Playlist mini transport: 0 prev, 1 play, 2 pause, 3 stop, 4 next, 5 eject.
    PlAction(u8),
}

impl Element {
    fn is_drag(self) -> bool {
        matches!(
            self,
            Element::Volume
                | Element::Balance
                | Element::Position
                | Element::Preamp
                | Element::EqBand(_)
                | Element::PlScroll
        )
    }
}

/// A window as placed on screen in the last frame.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct WindowGeom {
    pub window: Window,
    /// Terminal cells covered by the window image.
    pub cells: Rect,
    /// Window pixels per terminal column / row.
    pub px_per_col: f32,
    pub px_per_row: f32,
    /// Window height in pixels (varies for the playlist).
    pub height_px: u32,
}

impl WindowGeom {
    /// Window pixel under the centre of cell (col, row).
    pub fn to_px(self, col: u16, row: u16) -> (f32, f32) {
        (
            (f32::from(col) - f32::from(self.cells.x) + 0.5) * self.px_per_col,
            (f32::from(row) - f32::from(self.cells.y) + 0.5) * self.px_per_row,
        )
    }
}

/// Clickable areas recorded during render, in priority order.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Region {
    Window(WindowGeom),
    /// A text-layout element (fallback mode); sliders map along the x axis.
    Element {
        element: Element,
        cells: Rect,
    },
    /// Playlist text rows: row `cells.y + r` shows track `first + r`.
    Tracks {
        cells: Rect,
        first: usize,
    },
}

impl Region {
    fn cells(&self) -> Rect {
        match self {
            Region::Window(g) => g.cells,
            Region::Element { cells, .. } | Region::Tracks { cells, .. } => *cells,
        }
    }
}

#[derive(Debug, Clone, Copy)]
enum DragGeom {
    Window(WindowGeom),
    Text(Rect),
}

#[derive(Debug, Clone, Copy)]
struct Drag {
    element: Element,
    geom: DragGeom,
}

const DOUBLE_CLICK: Duration = Duration::from_millis(400);
const MARQUEE_STEP: Duration = Duration::from_millis(220);
const ANIMATION_TICK: Duration = Duration::from_millis(40);

pub struct WinampUiState {
    pub show_eq: bool,
    pub show_playlist: bool,
    pub vis_mode: VisMode,
    /// Time display shows remaining time ("-m:ss").
    pub time_remaining: bool,
    pub focus: Window,
    /// The playlist editor shows the provider browser, not the queue.
    pub browsing: bool,
    pressed: Option<Element>,
    drag: Option<Drag>,
    seek_preview: Option<f32>,
    pl_scroll: usize,
    pl_visible: usize,
    last_selected: Option<usize>,
    browse_scroll: usize,
    last_click: Option<(Instant, usize)>,
    vis: VisState,
    epoch: Instant,
    last_vis_t: Option<Duration>,
    frames: [Canvas; 3],
    regions: Vec<Region>,
    pub(crate) pl_rows: Vec<RowSpan>,
    marquee: String,
    scratch: String,
}

impl Default for WinampUiState {
    fn default() -> Self {
        Self::new()
    }
}

impl WinampUiState {
    pub fn new() -> Self {
        Self {
            show_eq: true,
            show_playlist: true,
            vis_mode: VisMode::Analyzer,
            time_remaining: false,
            focus: Window::Main,
            browsing: false,
            pressed: None,
            drag: None,
            seek_preview: None,
            pl_scroll: 0,
            pl_visible: 0,
            last_selected: None,
            browse_scroll: 0,
            last_click: None,
            vis: VisState::default(),
            epoch: Instant::now(),
            last_vis_t: None,
            frames: Default::default(),
            regions: Vec::new(),
            pl_rows: Vec::new(),
            marquee: String::new(),
            scratch: String::new(),
        }
    }

    /// Element currently held down, if any.
    pub fn pressed(&self) -> Option<Element> {
        self.pressed
    }

    /// Animation clock (time since this state was created).
    pub fn clock(&self) -> Duration {
        self.epoch.elapsed()
    }

    /// Marquee text for `view` ("N. Title (m:ss)"), built in a reused buffer.
    pub fn marquee_for(&mut self, view: &PlayerView) -> &str {
        compose::marquee_text(&mut self.marquee, view);
        &self.marquee
    }

    pub fn canvas(&self, window: Window) -> &Canvas {
        &self.frames[window.index()]
    }

    /// Apply UI-level commands (window toggles). The app dispatcher should
    /// forward every command here; others are ignored.
    pub fn apply(&mut self, cmd: &Command) {
        match cmd {
            Command::ToggleEqWindow => self.show_eq = !self.show_eq,
            Command::TogglePlaylistWindow => self.show_playlist = !self.show_playlist,
            _ => {}
        }
    }

    /// How soon the Winamp view needs another frame without input
    /// (visualizer, marquee, clock, pause blink). `None` = fully idle.
    pub fn wants_tick(&self, view: &PlayerView) -> Option<Duration> {
        match view.status {
            PlayStatus::Stopped => None,
            PlayStatus::Paused => Some(Duration::from_millis(500)),
            PlayStatus::Playing => {
                if self.vis_mode != VisMode::Off {
                    Some(ANIMATION_TICK)
                } else if self.marquee.chars().count() >= compose::MARQUEE_MAX_CHARS {
                    Some(MARQUEE_STEP)
                } else {
                    Some(Duration::from_millis(250))
                }
            }
        }
    }

    // ── Rendering ──────────────────────────────────────────────────────

    /// Forget last frame's clickable regions; call at the start of a frame.
    pub fn begin_layout(&mut self) {
        self.regions.clear();
    }

    pub fn push_region(&mut self, region: Region) {
        self.regions.push(region);
    }

    /// Composite the main window for animation time `t`.
    pub fn compose_main(&mut self, skin: &WinampSkin, view: &PlayerView, t: Duration) {
        match view.status {
            PlayStatus::Playing => {
                let dt = self
                    .last_vis_t
                    .map_or(0.0, |last| t.saturating_sub(last).as_secs_f32());
                self.vis.update(view.spectrum, dt);
                self.last_vis_t = Some(t);
            }
            PlayStatus::Paused => self.last_vis_t = None,
            PlayStatus::Stopped => {
                self.vis.reset();
                self.last_vis_t = None;
            }
        }
        compose::marquee_text(&mut self.marquee, view);
        let ctx = MainCtx {
            focused: self.focus == Window::Main,
            pressed: self.pressed,
            seek_preview: self.seek_preview,
            time_remaining: self.time_remaining,
            blink_on: view.status != PlayStatus::Paused || (t.as_millis() / 1000).is_multiple_of(2),
            marquee: &self.marquee,
            marquee_step: (t.as_millis() / MARQUEE_STEP.as_millis()) as u64,
            vis_mode: self.vis_mode,
            vis: &self.vis,
            eq_shown: self.show_eq,
            pl_shown: self.show_playlist,
        };
        compose::compose_main(&mut self.frames[Window::Main.index()], skin, view, &ctx);
    }

    pub fn compose_eq(&mut self, skin: &WinampSkin, view: &PlayerView) {
        let ctx = EqCtx {
            focused: self.focus == Window::Equalizer,
            pressed: self.pressed,
        };
        compose::compose_eq(
            &mut self.frames[Window::Equalizer.index()],
            skin,
            &view.eq,
            &ctx,
        );
    }

    /// Clamp the playlist scroll for `visible` rows and keep a newly selected
    /// track in view. Returns the first visible track index.
    pub fn sync_playlist_scroll(&mut self, visible: usize, view: &PlayerView) -> usize {
        let len = view.playlist.len();
        self.pl_visible = visible;
        if self.last_selected != Some(view.selected) && visible > 0 {
            if view.selected < self.pl_scroll {
                self.pl_scroll = view.selected;
            } else if view.selected >= self.pl_scroll + visible {
                self.pl_scroll = view.selected + 1 - visible;
            }
            self.last_selected = Some(view.selected);
        }
        self.pl_scroll = self.pl_scroll.min(len.saturating_sub(visible));
        self.pl_scroll
    }

    /// Keep the browser's highlighted row `selected` of `len` in `visible`
    /// rows. Returns the first visible row.
    pub fn sync_browse_scroll(&mut self, visible: usize, selected: usize, len: usize) -> usize {
        let visible = visible.max(1);
        if selected < self.browse_scroll {
            self.browse_scroll = selected;
        } else if selected >= self.browse_scroll + visible {
            self.browse_scroll = selected + 1 - visible;
        }
        self.browse_scroll = self.browse_scroll.min(len.saturating_sub(visible));
        self.browse_scroll
    }

    /// Re-point the playlist editor's list rows (the provider browser draws
    /// over the queue): row `cells.y + r` is now row `first + r`.
    pub fn set_list_region(&mut self, cells: Rect, first: usize) {
        self.regions.retain(|r| !matches!(r, Region::Tracks { .. }));
        if !cells.is_empty() {
            // Ahead of the playlist window, which covers the same cells.
            self.regions.insert(0, Region::Tracks { cells, first });
        }
    }

    fn scroll_frac(&self, len: usize) -> f32 {
        let max = len.saturating_sub(self.pl_visible);
        if max == 0 {
            0.0
        } else {
            self.pl_scroll as f32 / max as f32
        }
    }

    /// Composite the playlist window (`height` px) using `self.pl_rows`.
    pub fn compose_playlist(&mut self, skin: &WinampSkin, view: &PlayerView, height: u32) {
        let scroll_frac = self.scroll_frac(view.playlist.len());
        let ctx = PlaylistCtx {
            focused: self.focus == Window::Playlist,
            pressed: self.pressed,
            height,
            rows: &self.pl_rows,
            scroll_frac,
            scratch: &mut self.scratch,
        };
        compose::compose_playlist(&mut self.frames[Window::Playlist.index()], skin, view, ctx);
    }

    // ── Input ──────────────────────────────────────────────────────────

    fn region_at(&self, col: u16, row: u16) -> Option<Region> {
        let p = Position::new(col, row);
        self.regions.iter().find(|r| r.cells().contains(p)).copied()
    }

    /// Translate a terminal mouse event into a player command, updating
    /// pressed/drag state. Uses the geometry recorded by the last render.
    pub fn on_mouse(&mut self, ev: MouseEvent, view: &PlayerView) -> Option<Command> {
        self.on_mouse_at(ev, view, Instant::now())
    }

    /// A left click on a row of the playlist editor's list of `len` rows:
    /// `(row, double_click)`.
    pub fn list_press(&mut self, ev: &MouseEvent, len: usize) -> Option<(usize, bool)> {
        if ev.kind != MouseEventKind::Down(MouseButton::Left) {
            return None;
        }
        self.list_click(ev.column, ev.row, len, Instant::now())
    }

    /// The mouse wheel over the playlist editor: `Some(true)` scrolls down.
    pub fn list_wheel(&self, ev: &MouseEvent) -> Option<bool> {
        let down = match ev.kind {
            MouseEventKind::ScrollDown => true,
            MouseEventKind::ScrollUp => false,
            _ => return None,
        };
        matches!(
            self.region_at(ev.column, ev.row)?,
            Region::Tracks { .. }
                | Region::Window(WindowGeom {
                    window: Window::Playlist,
                    ..
                })
        )
        .then_some(down)
    }

    fn list_click(
        &mut self,
        col: u16,
        row: u16,
        len: usize,
        now: Instant,
    ) -> Option<(usize, bool)> {
        let Some(Region::Tracks { cells, first }) = self.region_at(col, row) else {
            return None;
        };
        self.focus = Window::Playlist;
        let idx = first + usize::from(row - cells.y);
        if idx >= len {
            return None;
        }
        let double = self
            .last_click
            .is_some_and(|(t, i)| i == idx && now.duration_since(t) <= DOUBLE_CLICK);
        self.last_click = if double { None } else { Some((now, idx)) };
        Some((idx, double))
    }

    fn on_mouse_at(&mut self, ev: MouseEvent, view: &PlayerView, now: Instant) -> Option<Command> {
        let (col, row) = (ev.column, ev.row);
        match ev.kind {
            MouseEventKind::Down(MouseButton::Left) => self.press(col, row, view, now),
            MouseEventKind::Drag(MouseButton::Left) => self.drag_to(col, row, view),
            MouseEventKind::Up(MouseButton::Left) => self.release(col, row, view),
            MouseEventKind::ScrollUp => self.wheel(col, row, -1),
            MouseEventKind::ScrollDown => self.wheel(col, row, 1),
            _ => None,
        }
    }

    fn press(&mut self, col: u16, row: u16, view: &PlayerView, now: Instant) -> Option<Command> {
        self.pressed = None;
        self.drag = None;
        self.seek_preview = None;
        let (element, geom) = match self.region_at(col, row)? {
            Region::Tracks { .. } => {
                let (idx, double) = self.list_click(col, row, view.playlist.len(), now)?;
                return Some(if double {
                    Command::PlayIndex(idx)
                } else {
                    Command::Select(idx)
                });
            }
            Region::Window(g) => {
                self.focus = g.window;
                let (x, y) = g.to_px(col, row);
                (hit::hit(g.window, x, y, g.height_px)?, DragGeom::Window(g))
            }
            Region::Element { element, cells } => (element, DragGeom::Text(cells)),
        };
        match element {
            Element::Time => {
                self.time_remaining = !self.time_remaining;
                return None;
            }
            Element::Vis => {
                self.vis_mode = self.vis_mode.next();
                return None;
            }
            _ => {}
        }
        self.pressed = Some(element);
        if element.is_drag() {
            self.drag = Some(Drag { element, geom });
            return self.drag_value(element, geom, col, row, view);
        }
        None
    }

    fn drag_to(&mut self, col: u16, row: u16, view: &PlayerView) -> Option<Command> {
        let d = self.drag?;
        self.drag_value(d.element, d.geom, col, row, view)
    }

    /// Slider value at (col, row) for an active drag. Position only previews
    /// (seek happens on release); volume/balance/EQ apply live.
    fn drag_value(
        &mut self,
        element: Element,
        geom: DragGeom,
        col: u16,
        row: u16,
        view: &PlayerView,
    ) -> Option<Command> {
        let (x, y, frac) = match geom {
            DragGeom::Window(g) => {
                let (x, y) = g.to_px(col, row);
                (x, y, None)
            }
            DragGeom::Text(r) => {
                let f = ((f32::from(col) - f32::from(r.x) + 0.5) / f32::from(r.width.max(1)))
                    .clamp(0.0, 1.0);
                (0.0, 0.0, Some(f))
            }
        };
        match element {
            Element::Volume => Some(Command::SetVolume(
                frac.unwrap_or_else(|| hit::volume_at(x)),
            )),
            Element::Balance => Some(Command::SetBalance(
                frac.map_or_else(|| hit::balance_at(x), |f| f * 2.0 - 1.0),
            )),
            Element::Position => {
                self.seek_preview = Some(frac.unwrap_or_else(|| hit::position_at(x)));
                None
            }
            Element::Preamp => Some(Command::SetPreamp(hit::eq_db_at(y))),
            Element::EqBand(band) => Some(Command::SetEqBand {
                band: usize::from(band),
                db: hit::eq_db_at(y),
            }),
            Element::PlScroll => {
                if let DragGeom::Window(g) = geom {
                    let area = compose::playlist_track_area(g.height_px);
                    let travel = (area.h as f32 - sprites::PL_SCROLL_HANDLE.h as f32).max(1.0);
                    let f = ((y - area.y as f32 - sprites::PL_SCROLL_HANDLE.h as f32 / 2.0)
                        / travel)
                        .clamp(0.0, 1.0);
                    let max = view.playlist.len().saturating_sub(self.pl_visible);
                    self.pl_scroll = (f * max as f32).round() as usize;
                }
                None
            }
            _ => None,
        }
    }

    fn release(&mut self, col: u16, row: u16, view: &PlayerView) -> Option<Command> {
        let pressed = self.pressed.take();
        let drag = self.drag.take();
        let preview = self.seek_preview.take();
        if let Some(d) = drag {
            if d.element == Element::Position && view.status != PlayStatus::Stopped {
                return preview.map(Command::SeekFrac);
            }
            return None;
        }
        let el = pressed?;
        // A click only counts if released over the element it started on.
        let over = match self.region_at(col, row)? {
            Region::Window(g) => {
                let (x, y) = g.to_px(col, row);
                hit::hit(g.window, x, y, g.height_px)
            }
            Region::Element { element, .. } => Some(element),
            Region::Tracks { .. } => None,
        };
        (over == Some(el))
            .then(|| hit::click_command(el, view))
            .flatten()
    }

    fn wheel(&mut self, col: u16, row: u16, dir: i32) -> Option<Command> {
        match self.region_at(col, row)? {
            Region::Tracks { .. }
            | Region::Window(WindowGeom {
                window: Window::Playlist,
                ..
            }) => {
                self.pl_scroll = if dir < 0 {
                    self.pl_scroll.saturating_sub(3)
                } else {
                    self.pl_scroll + 3
                };
                None
            }
            _ => Some(Command::VolumeBy(if dir < 0 { 0.05 } else { -0.05 })),
        }
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::media::{MediaItem, SourceKind};
    use crate::view::{EqSettings, RepeatMode, StreamFormat};
    use crossterm::event::KeyModifiers;

    pub(crate) fn sample_playlist() -> &'static [MediaItem] {
        let items: Vec<MediaItem> = (0..12)
            .map(|i| MediaItem {
                id: format!("/music/{i}.mp3"),
                title: format!("Track number {}", i + 1),
                artist: Some("DJ Mike Llama".into()),
                album: None,
                duration: Some(150 + i * 17),
                subtitle: None,
                source: SourceKind::Local,
            })
            .collect();
        items.leak()
    }

    pub(crate) fn sample_view(status: PlayStatus) -> PlayerView<'static> {
        static SPECTRUM: [f32; 32] = [
            0.9, 0.95, 0.85, 0.8, 0.7, 0.75, 0.6, 0.65, 0.5, 0.55, 0.45, 0.5, 0.4, 0.35, 0.3, 0.4,
            0.3, 0.25, 0.2, 0.3, 0.2, 0.15, 0.2, 0.1, 0.15, 0.1, 0.08, 0.1, 0.05, 0.08, 0.04, 0.02,
        ];
        let mut eq = EqSettings {
            enabled: true,
            ..EqSettings::default()
        };
        eq.preamp_db = 3.0;
        eq.bands_db = [6.0, 4.0, 2.0, 0.0, -2.0, -4.0, -2.0, 0.0, 4.0, 8.0];
        PlayerView {
            title: "DJ Mike Llama - Llama Whippin' Intro",
            status,
            elapsed: Duration::from_secs(83),
            duration: Some(Duration::from_secs(185)),
            volume: 0.75,
            balance: 0.0,
            shuffle: true,
            repeat: RepeatMode::All,
            eq,
            format: StreamFormat {
                kbps: Some(128),
                sample_rate: Some(44_100),
                channels: Some(2),
            },
            spectrum: &SPECTRUM,
            scope: &[],
            playlist: sample_playlist(),
            current: Some(0),
            selected: 2,
        }
    }

    fn mouse(kind: MouseEventKind, column: u16, row: u16) -> MouseEvent {
        MouseEvent {
            kind,
            column,
            row,
            modifiers: KeyModifiers::NONE,
        }
    }

    /// Main window at 1 px per column, 2 px per row (half-block scale) at (0, 0).
    fn halfblock_state() -> WinampUiState {
        let mut s = WinampUiState::new();
        s.push_region(Region::Window(WindowGeom {
            window: Window::Main,
            cells: Rect::new(0, 0, 275, 58),
            px_per_col: 1.0,
            px_per_row: 2.0,
            height_px: 116,
        }));
        s.push_region(Region::Window(WindowGeom {
            window: Window::Equalizer,
            cells: Rect::new(0, 58, 275, 58),
            px_per_col: 1.0,
            px_per_row: 2.0,
            height_px: 116,
        }));
        s
    }

    #[test]
    fn button_click_requires_release_on_same_element() {
        let view = sample_view(PlayStatus::Playing);
        let mut s = halfblock_state();
        // Play button: x 39..62, y 88..106 → rows 44..52.
        assert_eq!(
            s.on_mouse(
                mouse(MouseEventKind::Down(MouseButton::Left), 45, 46),
                &view
            ),
            None
        );
        assert_eq!(s.pressed(), Some(Element::Play));
        assert_eq!(
            s.on_mouse(mouse(MouseEventKind::Up(MouseButton::Left), 45, 46), &view),
            Some(Command::Play)
        );
        assert_eq!(s.pressed(), None);
        // Press on stop, release on next: no command.
        s.on_mouse(
            mouse(MouseEventKind::Down(MouseButton::Left), 90, 46),
            &view,
        );
        assert_eq!(
            s.on_mouse(mouse(MouseEventKind::Up(MouseButton::Left), 115, 46), &view),
            None
        );
    }

    #[test]
    fn volume_drag_emits_live_values_and_seek_waits_for_release() {
        let view = sample_view(PlayStatus::Playing);
        let mut s = halfblock_state();
        // Volume row: y 57..70 → row 29 (centre y = 59).
        assert_eq!(
            s.on_mouse(
                mouse(MouseEventKind::Down(MouseButton::Left), 113, 29),
                &view
            ),
            Some(Command::SetVolume(0.0))
        );
        match s.on_mouse(
            mouse(MouseEventKind::Drag(MouseButton::Left), 139, 29),
            &view,
        ) {
            Some(Command::SetVolume(v)) => assert!((v - 0.5).abs() < 1e-6),
            other => panic!("unexpected {other:?}"),
        }
        assert_eq!(
            s.on_mouse(mouse(MouseEventKind::Up(MouseButton::Left), 300, 29), &view),
            None
        );

        // Position bar: y 72..82 → row 37.
        assert_eq!(
            s.on_mouse(
                mouse(MouseEventKind::Down(MouseButton::Left), 30, 37),
                &view
            ),
            None
        );
        assert_eq!(
            s.on_mouse(
                mouse(MouseEventKind::Drag(MouseButton::Left), 139, 37),
                &view
            ),
            None
        );
        match s.on_mouse(mouse(MouseEventKind::Up(MouseButton::Left), 139, 37), &view) {
            Some(Command::SeekFrac(f)) => assert!((f - 109.0 / 219.0).abs() < 1e-6),
            other => panic!("unexpected {other:?}"),
        }
    }

    #[test]
    fn eq_band_drag_maps_y_to_db() {
        let view = sample_view(PlayStatus::Playing);
        let mut s = halfblock_state();
        // Band 2 at x 114..128; EQ window starts at row 58. Top of travel y=43.5.
        match s.on_mouse(
            mouse(MouseEventKind::Down(MouseButton::Left), 120, 58 + 21),
            &view,
        ) {
            Some(Command::SetEqBand { band: 2, db }) => assert_eq!(db, 12.0),
            other => panic!("unexpected {other:?}"),
        }
        match s.on_mouse(
            mouse(MouseEventKind::Drag(MouseButton::Left), 120, 58 + 47),
            &view,
        ) {
            Some(Command::SetEqBand { band: 2, db }) => assert_eq!(db, -12.0),
            other => panic!("unexpected {other:?}"),
        }
        assert_eq!(s.focus, Window::Equalizer);
    }

    #[test]
    fn track_rows_select_and_double_click_plays() {
        let view = sample_view(PlayStatus::Playing);
        let mut s = WinampUiState::new();
        s.push_region(Region::Tracks {
            cells: Rect::new(2, 10, 40, 5),
            first: 4,
        });
        let t0 = Instant::now();
        let down = mouse(MouseEventKind::Down(MouseButton::Left), 5, 12);
        assert_eq!(s.on_mouse_at(down, &view, t0), Some(Command::Select(6)));
        assert_eq!(
            s.on_mouse_at(down, &view, t0 + Duration::from_millis(200)),
            Some(Command::PlayIndex(6))
        );
        // Slow second click is a plain select.
        assert_eq!(
            s.on_mouse_at(down, &view, t0 + Duration::from_secs(2)),
            Some(Command::Select(6))
        );
        // Rows past the end of the playlist do nothing.
        s.begin_layout();
        s.push_region(Region::Tracks {
            cells: Rect::new(2, 10, 40, 5),
            first: 11,
        });
        let down = mouse(MouseEventKind::Down(MouseButton::Left), 5, 13);
        assert_eq!(s.on_mouse_at(down, &view, t0), None);
    }

    #[test]
    fn list_rows_can_be_repointed_at_the_provider_browser() {
        let mut s = WinampUiState::new();
        s.push_region(Region::Tracks {
            cells: Rect::new(2, 4, 40, 6),
            first: 0,
        });
        s.push_region(Region::Window(WindowGeom {
            window: Window::Playlist,
            cells: Rect::new(0, 0, 50, 20),
            px_per_col: 1.0,
            px_per_row: 2.0,
            height_px: 40,
        }));
        // The browser's rows start one row lower (under its breadcrumb).
        s.set_list_region(Rect::new(2, 5, 40, 5), 7);
        let down = mouse(MouseEventKind::Down(MouseButton::Left), 3, 6);
        let t0 = Instant::now();
        assert_eq!(s.list_click(3, 6, 20, t0), Some((8, false)));
        assert_eq!(
            s.list_click(3, 6, 20, t0 + Duration::from_millis(100)),
            Some((8, true))
        );
        assert_eq!(s.list_press(&down, 8), None, "past the end");
        assert_eq!(s.list_click(3, 4, 20, t0), None, "the breadcrumb row");
        assert_eq!(
            s.list_wheel(&mouse(MouseEventKind::ScrollDown, 3, 6)),
            Some(true)
        );
        assert_eq!(
            s.list_wheel(&mouse(MouseEventKind::ScrollUp, 45, 1)),
            Some(false),
            "anywhere on the playlist window"
        );
        assert_eq!(s.list_wheel(&mouse(MouseEventKind::ScrollUp, 60, 1)), None);
        // Only one list region remains.
        s.set_list_region(Rect::default(), 0);
        assert_eq!(s.list_click(3, 6, 20, t0), None);
    }

    #[test]
    fn browse_scroll_keeps_the_highlight_visible() {
        let mut s = WinampUiState::new();
        assert_eq!(s.sync_browse_scroll(5, 0, 20), 0);
        assert_eq!(s.sync_browse_scroll(5, 7, 20), 3);
        assert_eq!(s.sync_browse_scroll(5, 5, 20), 3);
        assert_eq!(s.sync_browse_scroll(5, 1, 20), 1);
        assert_eq!(
            s.sync_browse_scroll(5, 1, 3),
            0,
            "short lists start at the top"
        );
    }

    #[test]
    fn text_layout_sliders_use_cell_fraction() {
        let view = sample_view(PlayStatus::Playing);
        let mut s = WinampUiState::new();
        s.push_region(Region::Element {
            element: Element::Balance,
            cells: Rect::new(10, 0, 10, 1),
        });
        match s.on_mouse(mouse(MouseEventKind::Down(MouseButton::Left), 19, 0), &view) {
            Some(Command::SetBalance(b)) => assert!((b - 0.9).abs() < 1e-6),
            other => panic!("unexpected {other:?}"),
        }
    }

    #[test]
    fn time_and_vis_clicks_toggle_local_state() {
        let view = sample_view(PlayStatus::Playing);
        let mut s = halfblock_state();
        s.on_mouse(
            mouse(MouseEventKind::Down(MouseButton::Left), 60, 16),
            &view,
        );
        assert!(s.time_remaining);
        s.on_mouse(
            mouse(MouseEventKind::Down(MouseButton::Left), 40, 25),
            &view,
        );
        assert_eq!(s.vis_mode, VisMode::Oscilloscope);
        assert_eq!(
            s.on_mouse(mouse(MouseEventKind::ScrollUp, 40, 25), &view),
            Some(Command::VolumeBy(0.05))
        );
    }

    #[test]
    fn scroll_sync_keeps_selection_visible() {
        let mut view = sample_view(PlayStatus::Playing);
        let mut s = WinampUiState::new();
        view.selected = 10;
        assert_eq!(s.sync_playlist_scroll(4, &view), 7);
        view.selected = 1;
        assert_eq!(s.sync_playlist_scroll(4, &view), 1);
        // Wheel scrolling past the end is clamped.
        s.pl_scroll = 100;
        assert_eq!(s.sync_playlist_scroll(4, &view), 8);
    }

    #[test]
    fn ticks_only_when_animating() {
        let s = WinampUiState::new();
        assert_eq!(s.wants_tick(&sample_view(PlayStatus::Stopped)), None);
        assert_eq!(
            s.wants_tick(&sample_view(PlayStatus::Playing)),
            Some(ANIMATION_TICK)
        );
    }

    #[test]
    fn apply_toggles_windows() {
        let mut s = WinampUiState::new();
        s.apply(&Command::ToggleEqWindow);
        assert!(!s.show_eq);
        s.apply(&Command::TogglePlaylistWindow);
        assert!(!s.show_playlist);
        s.apply(&Command::Play);
        assert!(!s.show_eq);
    }
}
