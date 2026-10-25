//! Winamp skin view: the classic main, equalizer and playlist windows drawn
//! from the skin's sprites — through a pixel graphics protocol when the
//! terminal has one, as 1:1 half-blocks when it is big enough, and as a
//! skin-coloured text layout otherwise.

use std::fmt::Write as _;

use ratatui::buffer::Buffer;
use ratatui::layout::{Position, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::Frame;

use crate::app::{App, InputPurpose, Mode, Status};
use crate::browse::{Browser, Entry, LevelState};
use crate::gfx::{GfxPainter, Protocol};
use crate::keymap::{self, Action, Ctx};
use crate::skin::{rgba_color, SkinColors, WinampSkin};
use crate::ui::skin_bitmap::draw_halfblock;
use crate::ui::skin_layout::{self, Plan, WindowPlan};
use crate::view::{PlayStatus, PlayerView, RepeatMode};
use crate::winamp::compose::{self, RowSpan};
use crate::winamp::vis::{self, BARS};
use crate::winamp::{Element, Region, WinampUiState, WindowGeom};

/// Cells of interest after rendering.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct WinampRects {
    /// Playlist text rows.
    pub playlist: Rect,
}

/// Small fixed-capacity formatter so per-frame labels never allocate.
struct StackStr {
    buf: [u8; 48],
    len: usize,
}

impl StackStr {
    fn new() -> Self {
        Self {
            buf: [0; 48],
            len: 0,
        }
    }

    fn as_str(&self) -> &str {
        std::str::from_utf8(&self.buf[..self.len]).unwrap_or("")
    }
}

impl std::fmt::Write for StackStr {
    fn write_str(&mut self, s: &str) -> std::fmt::Result {
        let b = s.as_bytes();
        let end = self.len + b.len();
        if end > self.buf.len() {
            return Err(std::fmt::Error);
        }
        self.buf[self.len..end].copy_from_slice(b);
        self.len = end;
        Ok(())
    }
}

fn mmss(secs: u64) -> StackStr {
    let mut s = StackStr::new();
    let _ = write!(s, "{}:{:02}", secs / 60, secs % 60);
    s
}

/// Render the Winamp view into `area`.
///
/// Call `gfx.flush(backend)` after `terminal.draw` returns. Mouse events go
/// to `ui.on_mouse`, which uses the geometry recorded here.
pub fn render_view(
    frame: &mut Frame,
    area: Rect,
    skin: &WinampSkin,
    view: &PlayerView,
    ui: &mut WinampUiState,
    gfx: &mut GfxPainter,
) -> WinampRects {
    let screen = frame.area();
    gfx.begin_frame(screen);
    ui.begin_layout();
    let buf = frame.buffer_mut();
    let rects = render_windows(buf, screen, area, skin, view, ui, gfx);
    gfx.end_frame(buf);
    rects
}

fn render_windows(
    buf: &mut Buffer,
    screen: Rect,
    area: Rect,
    skin: &WinampSkin,
    view: &PlayerView,
    ui: &mut WinampUiState,
    gfx: &mut GfxPainter,
) -> WinampRects {
    let graphics = gfx.protocol() != Protocol::HalfBlock;
    if !skin.has_bitmaps() {
        return render_text(buf, area, &skin.colors, view, ui);
    }
    // An image touching the last screen row can make sixel/iTerm2 terminals scroll.
    let mut img_area = area;
    if graphics && area.bottom() >= screen.bottom() {
        img_area.height = img_area.height.saturating_sub(1);
    }
    let max_scale = if graphics { 8 } else { 1 };
    let Some(plan) = skin_layout::plan(
        img_area,
        gfx.cell_size(),
        max_scale,
        ui.show_eq,
        ui.show_playlist,
    ) else {
        return render_text(buf, area, &skin.colors, view, ui);
    };

    let t = ui.clock();
    ui.compose_main(skin, view, t);
    put_window(buf, gfx, ui, &plan, &plan.main, graphics);
    if let Some(eq) = plan.eq.as_ref() {
        ui.compose_eq(skin, view);
        put_window(buf, gfx, ui, &plan, eq, graphics);
    }
    let mut rects = WinampRects::default();
    if let Some(pl) = plan.playlist.as_ref() {
        rects.playlist = render_playlist(buf, gfx, ui, skin, view, &plan, pl, graphics);
    }
    rects
}

fn slot_of(w: &WindowPlan) -> usize {
    match w.window {
        crate::winamp::Window::Main => 0,
        crate::winamp::Window::Equalizer => 1,
        crate::winamp::Window::Playlist => 2,
    }
}

fn put_window(
    buf: &mut Buffer,
    gfx: &mut GfxPainter,
    ui: &mut WinampUiState,
    plan: &Plan,
    w: &WindowPlan,
    graphics: bool,
) {
    let canvas = ui.canvas(w.window);
    let cells = if graphics {
        gfx.place(
            buf,
            slot_of(w),
            canvas.pixels(),
            canvas.width(),
            canvas.height(),
            plan.scale,
            w.at,
        )
    } else {
        draw_halfblock(buf, canvas, w.at)
    };
    let scale = plan.scale as f32;
    ui.push_region(Region::Window(WindowGeom {
        window: w.window,
        cells,
        px_per_col: f32::from(plan.cell.0) / scale,
        px_per_row: f32::from(plan.cell.1) / scale,
        height_px: w.height_px,
    }));
}

#[allow(clippy::too_many_arguments)]
fn render_playlist(
    buf: &mut Buffer,
    gfx: &mut GfxPainter,
    ui: &mut WinampUiState,
    skin: &WinampSkin,
    view: &PlayerView,
    plan: &Plan,
    pl: &WindowPlan,
    graphics: bool,
) -> Rect {
    let scale = plan.scale as f32;
    let px_per_col = f32::from(plan.cell.0) / scale;
    let px_per_row = f32::from(plan.cell.1) / scale;
    let area = compose::playlist_track_area(pl.height_px);

    // Text columns/rows that lie fully inside the track area (webamp pads 3px).
    let col0 = ((area.x as f32 + 2.0) / px_per_col).ceil() as u16;
    let col1 = (((area.x + area.w as i32) as f32 - 1.0) / px_per_col).floor() as u16;
    let top = area.y as f32 + 2.0;
    let bottom = (area.y + area.h as i32) as f32 - 1.0;
    ui.pl_rows.clear();
    let mut first_row = None;
    for r in 0..pl.cells.height {
        let y0 = f32::from(r) * px_per_row;
        let y1 = y0 + px_per_row;
        if y0 >= top && y1 <= bottom {
            first_row.get_or_insert(r);
            ui.pl_rows.push(RowSpan {
                y0: y0.floor() as i32,
                y1: y1.ceil() as i32,
                track: None,
            });
        }
    }
    let visible = ui.pl_rows.len();
    let first = ui.sync_playlist_scroll(visible, view);
    // While browsing, the provider's rows are drawn over the track area.
    let shown = if ui.browsing { 0 } else { view.playlist.len() };
    for (i, row) in ui.pl_rows.iter_mut().enumerate() {
        let idx = first + i;
        row.track = (idx < shown).then_some(idx);
    }
    let text = match first_row {
        Some(r0) => Rect::new(
            pl.at.0 + col0,
            pl.at.1 + r0,
            col1.saturating_sub(col0),
            visible as u16,
        )
        .intersection(buf.area),
        None => Rect::default(),
    };
    if !text.is_empty() {
        ui.push_region(Region::Tracks { cells: text, first });
    }

    ui.compose_playlist(skin, view, pl.height_px);
    put_window(buf, gfx, ui, plan, pl, graphics);

    for i in 0..text.height {
        let idx = first + usize::from(i);
        if idx < shown {
            draw_track_row(buf, text.x, text.y + i, text.width, skin, view, idx);
        }
    }
    gfx.overlay(text);
    text
}

/// "N. Artist - Title" on the left, "m:ss" on the right, PLEDIT.TXT colours.
fn draw_track_row(
    buf: &mut Buffer,
    x: u16,
    y: u16,
    width: u16,
    skin: &WinampSkin,
    view: &PlayerView,
    idx: usize,
) {
    let (fg, bg) = compose::row_colors(skin, view, idx);
    let style = Style::default().fg(rgba_color(fg)).bg(rgba_color(bg));
    fill(buf, Rect::new(x, y, width, 1), style);
    let item = &view.playlist[idx];
    let dur = item.duration.map(mmss);
    let dur_w = dur.as_ref().map_or(0, |d| d.len as u16);
    if let Some(d) = &dur {
        if dur_w + 1 < width {
            buf.set_stringn(
                x + width - dur_w - 1,
                y,
                d.as_str(),
                usize::from(dur_w),
                style,
            );
        }
    }
    let end = x + width.saturating_sub(dur_w + 2);
    let mut num = StackStr::new();
    let _ = write!(num, "{}. ", idx + 1);
    let mut cx = x;
    for part in [
        num.as_str(),
        item.artist.as_deref().unwrap_or(""),
        if item.artist.is_some() { " - " } else { "" },
        item.title.as_str(),
    ] {
        if cx >= end {
            break;
        }
        cx = buf.set_stringn(cx, y, part, usize::from(end - cx), style).0;
    }
}

fn fill(buf: &mut Buffer, r: Rect, style: Style) {
    let r = r.intersection(buf.area);
    for y in r.top()..r.bottom() {
        for x in r.left()..r.right() {
            if let Some(c) = buf.cell_mut(Position::new(x, y)) {
                c.set_symbol(" ").set_style(style);
            }
        }
    }
}

/// Write `s` at (x, y) clipped to `right`; returns the next column.
fn put(buf: &mut Buffer, x: u16, y: u16, right: u16, s: &str, style: Style) -> u16 {
    if x >= right {
        return x;
    }
    buf.set_stringn(x, y, s, usize::from(right - x), style).0
}

// ─── Text layout (terminal too small, or skin without bitmaps) ─────────

fn render_text(
    buf: &mut Buffer,
    area: Rect,
    sc: &SkinColors,
    view: &PlayerView,
    ui: &mut WinampUiState,
) -> WinampRects {
    let mut rects = WinampRects::default();
    let area = area.intersection(buf.area);
    if area.is_empty() {
        return rects;
    }
    let right = area.right();
    let x0 = area.x;
    let body = Style::default().fg(sc.chrome_light).bg(sc.body_bg);
    let mut y = area.y;
    let mut row = |rows_left: &mut u16| -> Option<u16> {
        if *rows_left == 0 {
            None
        } else {
            *rows_left -= 1;
            let r = y;
            y += 1;
            Some(r)
        }
    };
    let mut left = area.height;

    // Title bar.
    if let Some(r) = row(&mut left) {
        let st = Style::default()
            .fg(sc.chrome_dark)
            .bg(sc.titlebar_bg)
            .add_modifier(Modifier::BOLD);
        fill(buf, Rect::new(x0, r, area.width, 1), st);
        let title = " WINAMP ";
        let cx = x0 + area.width.saturating_sub(title.len() as u16) / 2;
        put(buf, cx, r, right, title, st);
    }

    // Status: indicator, time, kbps/kHz, mono/stereo, spectrum.
    if let Some(r) = row(&mut left) {
        fill(buf, Rect::new(x0, r, area.width, 1), body);
        let (sym, col) = match view.status {
            PlayStatus::Playing => (" ▶ ", sc.play_indicator),
            PlayStatus::Paused => (" ❚❚", sc.pause_indicator),
            PlayStatus::Stopped => (" ■ ", sc.indicator_off),
        };
        let mut cx = put(buf, x0, r, right, sym, body.fg(col));
        let led = Style::default().fg(sc.led_on).bg(sc.chrome_dark);
        let t = if view.status == PlayStatus::Stopped {
            let mut s = StackStr::new();
            let _ = s.write_str("  :  ");
            s
        } else {
            let secs = view.elapsed.as_secs();
            match view.duration {
                Some(d) if ui.time_remaining => {
                    let mut s = StackStr::new();
                    let rem = d.as_secs().saturating_sub(secs);
                    let _ = write!(s, "-{}:{:02}", rem / 60, rem % 60);
                    s
                }
                _ => mmss(secs),
            }
        };
        cx = put(buf, cx + 1, r, right, " ", led);
        let time_x = cx;
        cx = put(
            buf,
            cx,
            r,
            right,
            t.as_str(),
            led.add_modifier(Modifier::BOLD),
        );
        cx = put(buf, cx, r, right, " ", led);
        ui.push_region(Region::Element {
            element: Element::Time,
            cells: Rect::new(time_x, r, cx.saturating_sub(time_x), 1),
        });
        if view.status != PlayStatus::Stopped {
            let mut info = StackStr::new();
            if let Some(k) = view.format.kbps {
                let _ = write!(info, " {k}kbps");
            }
            if let Some(sr) = view.format.sample_rate {
                let _ = write!(info, " {}kHz", (sr + 500) / 1000);
            }
            match view.format.channels {
                Some(1) => {
                    let _ = info.write_str(" mono");
                }
                Some(_) => {
                    let _ = info.write_str(" stereo");
                }
                None => {}
            }
            cx = put(buf, cx, r, right, info.as_str(), body.fg(sc.text_fg));
        }
        // Spectrum on the right.
        let mut bars = [0.0f32; BARS];
        if view.status == PlayStatus::Playing {
            vis::bucket(view.spectrum, &mut bars);
        }
        let n = (right.saturating_sub(cx + 2)).min(BARS as u16);
        let vx = right - n;
        for (i, h) in bars.iter().take(usize::from(n)).enumerate() {
            let level = ((h / 16.0) * 8.0).round().clamp(0.0, 8.0) as usize;
            let ch = [" ", "▁", "▂", "▃", "▄", "▅", "▆", "▇", "█"][level];
            let color = sc.vis_colors[(17 - level * 2).max(2)];
            put(
                buf,
                vx + i as u16,
                r,
                right,
                ch,
                Style::default().fg(color).bg(sc.vis_colors[0]),
            );
        }
        if n > 0 {
            ui.push_region(Region::Element {
                element: Element::Vis,
                cells: Rect::new(vx, r, n, 1),
            });
        }
    }

    // Marquee.
    if let Some(r) = row(&mut left) {
        let st = Style::default().fg(sc.text_fg).bg(sc.text_bg);
        fill(buf, Rect::new(x0, r, area.width, 1), st);
        let width = usize::from(area.width.saturating_sub(2));
        let step = (ui.clock().as_millis() / 220) as usize;
        let text = ui.marquee_for(view);
        let len = text.chars().count();
        let mut cx = x0 + 1;
        if len > width && view.status != PlayStatus::Stopped {
            let sep = "  ***  ";
            let cycle = len + sep.len();
            let offset = step % cycle;
            for ch in text
                .chars()
                .chain(sep.chars())
                .cycle()
                .skip(offset)
                .take(width)
            {
                if let Some(c) = buf.cell_mut(Position::new(cx, r)) {
                    c.set_char(ch).set_style(st);
                }
                cx += 1;
            }
        } else {
            put(buf, cx, r, right.saturating_sub(1), text, st);
        }
    }

    // Seek bar.
    if let Some(r) = row(&mut left) {
        fill(buf, Rect::new(x0, r, area.width, 1), body);
        let bar = Rect::new(x0 + 1, r, area.width.saturating_sub(2), 1);
        let frac = view
            .duration
            .map(|d| d.as_secs_f32())
            .filter(|d| *d > 0.0)
            .map(|d| (view.elapsed.as_secs_f32() / d).clamp(0.0, 1.0));
        let thumb =
            frac.map(|f| bar.x + (f * f32::from(bar.width.saturating_sub(1))).round() as u16);
        for cx in bar.left()..bar.right() {
            let (ch, color) = match thumb {
                Some(t) if cx == t => ("●", sc.chrome_light),
                Some(t) if cx < t => ("━", sc.led_on),
                _ => ("─", sc.chrome_mid),
            };
            put(buf, cx, r, right, ch, body.fg(color));
        }
        ui.push_region(Region::Element {
            element: Element::Position,
            cells: bar,
        });
    }

    // Transport + volume + balance.
    if let Some(r) = row(&mut left) {
        fill(buf, Rect::new(x0, r, area.width, 1), body);
        let btn = Style::default().fg(sc.btn_text).bg(sc.btn_normal);
        let pressed = Style::default().fg(sc.btn_text).bg(sc.btn_pressed);
        let mut cx = x0 + 1;
        for (label, el) in [
            ("|◀", Element::Prev),
            (" ▶", Element::Play),
            ("❚❚", Element::Pause),
            (" ■", Element::Stop),
            ("▶|", Element::Next),
            (" ⏏", Element::Eject),
        ] {
            let st = if ui.pressed() == Some(el) {
                pressed
            } else {
                btn
            };
            let start = cx;
            cx = put(buf, cx, r, right, " ", st);
            cx = put(buf, cx, r, right, label, st);
            cx = put(buf, cx, r, right, " ", st);
            ui.push_region(Region::Element {
                element: el,
                cells: Rect::new(start, r, cx.saturating_sub(start), 1),
            });
            cx += 1;
        }
        cx = put(buf, cx + 1, r, right, "VOL", body);
        let vol_x = cx + 1;
        for i in 0..8u16 {
            let on = f32::from(i) < view.volume.clamp(0.0, 1.0) * 8.0;
            let st = body.fg(if on { sc.led_on } else { sc.chrome_mid });
            put(buf, vol_x + i, r, right, if on { "█" } else { "░" }, st);
        }
        ui.push_region(Region::Element {
            element: Element::Volume,
            cells: Rect::new(vol_x, r, 8, 1).intersection(area),
        });
        cx = put(buf, vol_x + 9, r, right, "BAL", body);
        let bal_x = cx + 1;
        let pos = ((view.balance.clamp(-1.0, 1.0) + 1.0) / 2.0 * 6.0).round() as u16;
        for i in 0..7u16 {
            let st = body.fg(if i == pos { sc.led_on } else { sc.chrome_mid });
            put(
                buf,
                bal_x + i,
                r,
                right,
                if i == pos { "◆" } else { "─" },
                st,
            );
        }
        ui.push_region(Region::Element {
            element: Element::Balance,
            cells: Rect::new(bal_x, r, 7, 1).intersection(area),
        });
    }

    // Toggles.
    if let Some(r) = row(&mut left) {
        fill(buf, Rect::new(x0, r, area.width, 1), body);
        let mut cx = x0 + 1;
        for (label, on, el) in [
            ("SHUFFLE", view.shuffle, Element::Shuffle),
            (
                match view.repeat {
                    RepeatMode::One => "REPEAT 1",
                    _ => "REPEAT",
                },
                view.repeat != RepeatMode::Off,
                Element::Repeat,
            ),
            ("EQ", ui.show_eq, Element::EqButton),
            ("PL", ui.show_playlist, Element::PlButton),
        ] {
            let st = Style::default()
                .fg(if on {
                    sc.indicator_on
                } else {
                    sc.indicator_off
                })
                .bg(sc.chrome_dark);
            let start = cx;
            cx = put(buf, cx, r, right, "[", st);
            cx = put(buf, cx, r, right, label, st);
            cx = put(buf, cx, r, right, "]", st);
            ui.push_region(Region::Element {
                element: el,
                cells: Rect::new(start, r, cx.saturating_sub(start), 1),
            });
            cx += 1;
        }
    }

    // Playlist.
    if left > 0 {
        let start = y;
        let list = Rect::new(x0, start, area.width, left);
        fill(
            buf,
            list,
            Style::default().fg(sc.plist_normal).bg(sc.plist_normal_bg),
        );
        let first = ui.sync_playlist_scroll(usize::from(left), view);
        let shown = if ui.browsing { 0 } else { view.playlist.len() };
        for i in 0..left {
            let idx = first + usize::from(i);
            if idx >= shown {
                break;
            }
            draw_text_track_row(buf, x0, start + i, area.width, sc, view, idx);
        }
        ui.push_region(Region::Tracks { cells: list, first });
        rects.playlist = list;
    }
    rects
}

fn draw_text_track_row(
    buf: &mut Buffer,
    x: u16,
    y: u16,
    width: u16,
    sc: &SkinColors,
    view: &PlayerView,
    idx: usize,
) {
    let fg = if Some(idx) == view.current {
        sc.plist_current
    } else {
        sc.plist_normal
    };
    let bg = if idx == view.selected {
        sc.plist_selected_bg
    } else {
        sc.plist_normal_bg
    };
    let style = Style::default().fg(fg).bg(bg);
    fill(buf, Rect::new(x, y, width, 1), style);
    let item = &view.playlist[idx];
    let right = x + width;
    let dur = item.duration.map(mmss);
    let dur_w = dur.as_ref().map_or(0, |d| d.len as u16);
    if let Some(d) = &dur {
        put(
            buf,
            right.saturating_sub(dur_w + 1),
            y,
            right,
            d.as_str(),
            style,
        );
    }
    let end = right.saturating_sub(dur_w + 2);
    let mut num = StackStr::new();
    let _ = write!(num, "{:>3}. ", idx + 1);
    let mut cx = put(buf, x, y, end, num.as_str(), style);
    if let Some(a) = item.artist.as_deref() {
        cx = put(buf, cx, y, end, a, style);
        cx = put(buf, cx, y, end, " - ", style);
    }
    put(buf, cx, y, end, &item.title, style);
}

// ─── Full screen ────────────────────────────────────────────────────────

/// The Winamp theme's screen: the skin windows above a two-row footer
/// (status line and key hints). `skin` is `None` while the skin is still
/// loading. Returns the list rows the playlist editor shows (page keys).
pub fn draw(
    frame: &mut Frame,
    app: &App,
    skin: Option<&WinampSkin>,
    view: &PlayerView,
    ui: &mut WinampUiState,
    gfx: &mut GfxPainter,
) -> usize {
    let area = frame.area();
    let footer_h = area.height.min(2);
    let body = Rect {
        height: area.height - footer_h,
        ..area
    };
    let footer = Rect {
        y: body.bottom(),
        height: footer_h,
        ..area
    };
    let browser = app.winamp_browser();
    ui.browsing = browser.is_some();
    let mut rows = 0;
    match skin {
        Some(skin) => {
            let list = render_view(frame, body, skin, view, ui, gfx).playlist;
            let buf = frame.buffer_mut();
            rows = usize::from(list.height);
            match browser {
                Some(browser) => rows = draw_browser(buf, list, &skin.colors, browser, ui),
                None if view.playlist.is_empty() => draw_empty(buf, list, &skin.colors, app),
                None => {}
            }
        }
        None => {
            let sc = &SkinColors::DEFAULT;
            let style = Style::default().fg(sc.text_fg).bg(sc.text_bg);
            let buf = frame.buffer_mut();
            fill(buf, body, style);
            put(
                buf,
                body.x + 1,
                body.y,
                body.right(),
                "Loading skin…",
                style,
            );
        }
    }
    render_footer(frame.buffer_mut(), footer, app);
    rows
}

/// The provider's current level inside the playlist editor's track area:
/// a breadcrumb row, then the entries in PLEDIT colours. Returns the
/// number of entry rows.
fn draw_browser(
    buf: &mut Buffer,
    r: Rect,
    sc: &SkinColors,
    browser: &Browser,
    ui: &mut WinampUiState,
) -> usize {
    if r.is_empty() {
        ui.set_list_region(Rect::default(), 0);
        return 0;
    }
    let normal = Style::default().fg(sc.plist_normal).bg(sc.plist_normal_bg);
    fill(buf, r, normal);
    let right = r.right();
    let title = normal.fg(sc.plist_current).add_modifier(Modifier::BOLD);
    let mut x = r.x + 1;
    for (i, crumb) in browser.crumbs().enumerate() {
        if i > 0 {
            x = put(buf, x, r.y, right, " › ", title);
        }
        x = put(buf, x, r.y, right, crumb, title);
    }
    let body = Rect {
        y: r.y + 1,
        height: r.height - 1,
        ..r
    };
    let visible = usize::from(body.height);
    let notice = match browser.state() {
        LevelState::Loading => Some("Loading…"),
        LevelState::Error(e) => Some(e.as_str()),
        LevelState::Ready if browser.is_empty() => Some("Nothing here"),
        LevelState::Ready => None,
    };
    if let Some(notice) = notice {
        if !body.is_empty() {
            put(buf, body.x + 1, body.y, right, notice, normal);
        }
        ui.set_list_region(Rect::default(), 0);
        return visible;
    }
    let selected = browser.selected();
    let first = ui.sync_browse_scroll(visible, selected, browser.len());
    for (i, entry) in browser
        .entries()
        .iter()
        .enumerate()
        .skip(first)
        .take(visible)
    {
        let y = body.y + (i - first) as u16;
        let style = if i == selected {
            normal.bg(sc.plist_selected_bg)
        } else {
            normal
        };
        fill(buf, Rect::new(r.x, y, r.width, 1), style);
        let mut cx = r.x + 1;
        if browser.subscribed(entry) {
            cx = put(buf, cx, y, right, "★ ", style);
        }
        cx = put(buf, cx, y, right, entry.label(), style);
        if !matches!(entry, Entry::Track { .. }) {
            cx = put(buf, cx, y, right, " ›", style);
        }
        if let Some(detail) = entry.detail() {
            cx = put(buf, cx, y, right, "  ", style);
            put(buf, cx, y, right, detail, style.fg(sc.plist_current));
        }
    }
    ui.set_list_region(body, first);
    visible
}

/// `key` of `action` in `ctx` (empty when unbound), without allocating.
fn key_text(ctx: Ctx, action: Action) -> StackStr {
    let mut s = StackStr::new();
    if let Some(k) = keymap::first_key(ctx, action) {
        let _ = write!(s, "{k}");
    }
    s
}

/// What to do with an empty playlist, one key per row, from the key table.
fn draw_empty(buf: &mut Buffer, r: Rect, sc: &SkinColors, app: &App) {
    if r.is_empty() {
        return;
    }
    let normal = Style::default().fg(sc.plist_normal).bg(sc.plist_normal_bg);
    let key = normal.fg(sc.plist_current).add_modifier(Modifier::BOLD);
    let right = r.right();
    let x = r.x + 1;
    let busy = match &app.status {
        Status::Searching(t)
        | Status::Scanning(t)
        | Status::Downloading(t)
        | Status::Loading(t)
        | Status::Error(t) => Some(t.as_str()),
        Status::Idle | Status::Info(_) => None,
    };
    if let Some(msg) = busy {
        put(buf, x, r.y, right, msg, normal);
        return;
    }
    put(buf, x, r.y, right, "The playlist is empty.", key);
    let ctx = app.active_ctx();
    let source = app.tab.label();
    let rows: [(Action, &str, &str); 5] = [
        (Action::Open, "open files, folders, playlists or URLs", ""),
        (Action::Search, "search ", source),
        (Action::NextTab, "change the source (now ", source),
        (Action::BrowseSource, "browse ", source),
        (Action::Palette, "all keys", ""),
    ];
    let mut y = r.y + 1;
    for (action, label, arg) in rows {
        let k = key_text(ctx, action);
        if k.len == 0 {
            continue;
        }
        if y >= r.bottom() {
            break;
        }
        let mut cx = put(buf, x, y, right, k.as_str(), key);
        cx = put(buf, cx + 2, y, right, label, normal);
        cx = put(buf, cx, y, right, arg, normal);
        if action == Action::NextTab {
            put(buf, cx, y, right, ")", normal);
        }
        y += 1;
    }
}

/// Two rows in the terminal's own colours (never the skin's, which can be
/// unreadable): where you are and what happened, then the keys.
fn render_footer(buf: &mut Buffer, area: Rect, app: &App) {
    if area.height == 0 {
        return;
    }
    fill(buf, area, Style::reset());
    render_status(buf, Rect { height: 1, ..area }, app);
    if area.height >= 2 {
        render_hints(
            buf,
            Rect {
                y: area.y + 1,
                height: 1,
                ..area
            },
            app,
        );
    }
}

/// Status line: the prompt with its cursor while typing; otherwise the
/// list shown (`Playlist · 9 tracks`, `Radio › Top voted`), the source and
/// the latest message.
fn render_status(buf: &mut Buffer, area: Rect, app: &App) {
    let base = Style::reset();
    let bold = base.add_modifier(Modifier::BOLD);
    let (y, right) = (area.y, area.right());
    let mut x = area.x + 1;
    if app.mode == Mode::Input {
        match app.input_purpose {
            InputPurpose::Search if app.input_text.starts_with(':') => {
                x = put(buf, x, y, right, "Command", bold);
            }
            InputPurpose::Search => {
                x = put(buf, x, y, right, "Search ", bold);
                x = put(buf, x, y, right, app.tab.label(), bold);
            }
            InputPurpose::ProviderSearch => x = put(buf, x, y, right, app.prompt_label, bold),
            InputPurpose::SavePlaylist => x = put(buf, x, y, right, "Save the queue as", bold),
            InputPurpose::Open => x = put(buf, x, y, right, "Open a file, folder or URL", bold),
        }
        x = put(buf, x, y, right, ": ", bold);
        let cursor = base.add_modifier(Modifier::REVERSED);
        let text = app.input_text.as_str();
        let mut at_end = true;
        for (i, (idx, c)) in text.char_indices().enumerate() {
            let style = if i == app.input_cursor {
                at_end = false;
                cursor
            } else {
                base
            };
            x = put(buf, x, y, right, &text[idx..idx + c.len_utf8()], style);
        }
        if at_end {
            put(buf, x, y, right, " ", cursor);
        }
        return;
    }
    match app.winamp_browser() {
        Some(browser) => {
            for (i, crumb) in browser.crumbs().enumerate() {
                if i > 0 {
                    x = put(buf, x, y, right, " › ", bold);
                }
                x = put(buf, x, y, right, crumb, bold);
            }
        }
        None => {
            x = put(buf, x, y, right, "Playlist", bold);
            let n = app.queue.len();
            let mut count = StackStr::new();
            let _ = write!(count, " · {n} track{}", if n == 1 { "" } else { "s" });
            x = put(buf, x, y, right, count.as_str(), base);
            x = put(buf, x, y, right, "  ·  source ", base);
            x = put(buf, x, y, right, app.tab.label(), bold);
        }
    }
    let message = match &app.status {
        Status::Idle => None,
        Status::Error(t) => Some((t, bold.fg(Color::Red))),
        Status::Info(t)
        | Status::Searching(t)
        | Status::Loading(t)
        | Status::Scanning(t)
        | Status::Downloading(t) => Some((t, base)),
    };
    if let Some((text, style)) = message {
        x = put(buf, x, y, right, "  ·  ", base);
        put(buf, x, y, right, text, style);
    }
}

/// One hint: the keys of one or two actions (`j/k`) and a label.
type Hint = (Action, Option<Action>, &'static str);

/// The queue in the playlist editor, most important first.
const QUEUE_HINTS: &[Hint] = &[
    (Action::Down, Some(Action::Up), "move"),
    (Action::Activate, None, "play"),
    (Action::Search, None, "search"),
    (Action::NextTab, None, "source"),
    (Action::BrowseSource, None, "browse"),
    (Action::TogglePause, None, "pause"),
    (Action::Remove, None, "remove"),
    (Action::MoveDown, Some(Action::MoveUp), "reorder"),
    (Action::Open, None, "open"),
    (Action::EqPanel, None, "EQ"),
    (Action::PlaylistWindow, None, "playlist"),
    (Action::Quit, None, "quit"),
];

const EMPTY_HINTS: &[Hint] = &[
    (Action::Open, None, "open"),
    (Action::Search, None, "search"),
    (Action::NextTab, None, "source"),
    (Action::BrowseSource, None, "browse"),
    (Action::Quit, None, "quit"),
];

/// A provider's menus in the playlist editor. `Enter` reads "play" on tracks.
const BROWSER_HINTS: &[Hint] = &[
    (Action::Down, Some(Action::Up), "move"),
    (Action::Activate, None, "open"),
    (Action::Back, None, "back"),
    (Action::ProviderSearch, None, "search"),
    (Action::NextTab, None, "source"),
    (Action::Enqueue, None, "queue"),
    (Action::Subscribe, None, "subscribe"),
    (Action::Reload, None, "reload"),
    (Action::TogglePause, None, "pause"),
    (Action::Quit, None, "quit"),
];

const PROMPT_HINTS: &[Hint] = &[
    (Action::Submit, None, "ok"),
    (Action::Cancel, None, "cancel"),
    (Action::HistoryPrev, None, "history"),
    (Action::ClearLine, None, "clear"),
];

/// A row of key chips (bold reverse video in the terminal's colours, so
/// they read on any skin and terminal theme), each followed by its label.
struct Chips<'a> {
    buf: &'a mut Buffer,
    y: u16,
    start: u16,
    x: u16,
    right: u16,
}

impl Chips<'_> {
    fn width(key: &str, label: &str) -> u16 {
        (key.chars().count() + label.chars().count() + 3) as u16
    }

    fn draw(&mut self, x: u16, key: &str, label: &str) -> u16 {
        let base = Style::reset();
        let chip = base.add_modifier(Modifier::BOLD | Modifier::REVERSED);
        let right = self.buf.area.right();
        let mut x = put(self.buf, x, self.y, right, " ", chip);
        x = put(self.buf, x, self.y, right, key, chip);
        x = put(self.buf, x, self.y, right, " ", chip);
        put(self.buf, x + 1, self.y, right, label, base)
    }

    /// Adds a hint when it fits; `false` once the row is full.
    fn push(&mut self, key: &str, label: &str) -> bool {
        let gap = if self.x > self.start { 2 } else { 0 };
        if self.x + gap + Self::width(key, label) > self.right {
            return false;
        }
        self.x = self.draw(self.x + gap, key, label);
        true
    }

    /// Draws a hint flush right and keeps the others clear of it.
    fn reserve_right(&mut self, key: &str, label: &str) {
        let w = Self::width(key, label);
        if self.start + w > self.right {
            return;
        }
        let x = self.right - w;
        self.draw(x, key, label);
        self.right = x.saturating_sub(2);
    }
}

/// Key hints for what the Winamp view does right now; the rightmost slot
/// always offers the palette (`? all keys`) where it can open.
pub(super) fn render_hints(buf: &mut Buffer, area: Rect, app: &App) {
    if area.is_empty() {
        return;
    }
    fill(buf, area, Style::reset());
    let mut chips = Chips {
        y: area.y,
        start: area.x + 1,
        x: area.x + 1,
        right: area.right().saturating_sub(1),
        buf,
    };
    if app.palette.is_some() {
        for (keys, hint) in keymap::hints(Ctx::PALETTE) {
            if !chips.push(keys, hint) {
                break;
            }
        }
        return;
    }
    let (ctx, hints) = if app.mode == Mode::Input {
        (Ctx::PROMPT, PROMPT_HINTS)
    } else {
        let ctx = app.active_ctx();
        let palette = key_text(ctx, Action::Palette);
        if palette.len > 0 {
            chips.reserve_right(palette.as_str(), "all keys");
        }
        let hints = if app.winamp_browser().is_some() {
            BROWSER_HINTS
        } else if app.queue.is_empty() {
            EMPTY_HINTS
        } else {
            QUEUE_HINTS
        };
        (ctx, hints)
    };
    let on_track = app
        .winamp_browser()
        .is_some_and(|b| matches!(b.entries().get(b.selected()), Some(Entry::Track { .. })));
    let search = app.input_purpose == InputPurpose::Search;
    for &(action, also, label) in hints {
        let mut keys = key_text(ctx, action);
        if keys.len == 0 {
            continue;
        }
        if let Some(k) = also.and_then(|a| keymap::first_key(ctx, a)) {
            let _ = write!(keys, "/{k}");
        }
        let label = match action {
            Action::Activate if on_track => "play",
            Action::Submit if search => "search",
            _ => label,
        };
        if !chips.push(keys.as_str(), label) {
            break;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;
    use crate::providers::ProviderKind;
    use crate::skin::tests::{build_wsz, solid_bmp};
    use crate::theme::Theme;
    use crate::winamp::tests::{sample_playlist, sample_view};
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;

    fn winamp_app(tracks: usize) -> App {
        let mut app = App::new(Config::default());
        app.mode = Mode::Browse;
        app.theme = Theme::from_name("Winamp");
        app.queue
            .replace(sample_playlist()[..tracks].to_vec(), None);
        app
    }

    /// Draws the whole Winamp screen (text layout: the terminal is narrower
    /// than the skin) and returns the buffer and the list rows.
    fn draw_app(app: &App, w: u16, h: u16, ui: &mut WinampUiState) -> (Buffer, usize) {
        let skin = skin_with_bitmaps();
        let mut gfx = GfxPainter::with_cell_size(Protocol::HalfBlock, None);
        let mut terminal = Terminal::new(TestBackend::new(w, h)).unwrap();
        let mut rows = 0;
        terminal
            .draw(|f| {
                let view = app.player_view();
                rows = draw(f, app, Some(&skin), &view, ui, &mut gfx);
            })
            .unwrap();
        (terminal.backend().buffer().clone(), rows)
    }

    fn row_text(buf: &Buffer, y: u16) -> String {
        (0..buf.area.width).map(|x| buf[(x, y)].symbol()).collect()
    }

    fn all_rows(buf: &Buffer) -> Vec<String> {
        (0..buf.area.height).map(|y| row_text(buf, y)).collect()
    }

    fn chip(buf: &Buffer, y: u16, key: &str) -> Position {
        let row = row_text(buf, y);
        let col = row
            .find(&format!(" {key} "))
            .unwrap_or_else(|| panic!("no {key} chip in {row:?}"));
        Position::new(row[..col].chars().count() as u16 + 1, y)
    }

    #[test]
    fn footer_uses_terminal_colours_and_key_chips() {
        let app = winamp_app(9);
        let mut ui = WinampUiState::new();
        let (buf, rows) = draw_app(&app, 120, 30, &mut ui);
        assert!(rows > 0);
        let status = row_text(&buf, 28);
        assert!(
            status.starts_with(" Playlist · 9 tracks  ·  source Local"),
            "{status:?}"
        );
        let hints = row_text(&buf, 29);
        assert!(
            hints.starts_with("  j/k  move   Enter  play   /  search"),
            "{hints:?}"
        );
        assert!(hints.trim_end().ends_with(" ?  all keys"), "{hints:?}");
        assert!(!hints.contains("browse"), "Local has no menus: {hints:?}");
        // Keys: bold reverse video in the terminal's default colours.
        let chip_style = Modifier::BOLD | Modifier::REVERSED;
        for key in ["j/k", "Enter", "/", "?"] {
            let cell = &buf[chip(&buf, 29, key)];
            assert!(cell.modifier.contains(chip_style), "{key}: {cell:?}");
            assert_eq!((cell.fg, cell.bg), (Color::Reset, Color::Reset), "{key}");
        }
        // Labels and the status line: plain terminal colours.
        let label = &buf[(hints.find("move").unwrap() as u16, 29)];
        assert!(!label.modifier.contains(Modifier::REVERSED));
        assert_eq!((label.fg, label.bg), (Color::Reset, Color::Reset));
        let place = &buf[(1, 28)];
        assert_eq!((place.fg, place.bg), (Color::Reset, Color::Reset));
        assert!(place.modifier.contains(Modifier::BOLD));

        // Narrow terminals keep the most important hints and `? all keys`.
        let (buf, _) = draw_app(&app, 40, 20, &mut ui);
        let hints = row_text(&buf, 19);
        assert!(hints.starts_with("  j/k  move"), "{hints:?}");
        assert!(hints.trim_end().ends_with(" ?  all keys"), "{hints:?}");
    }

    #[test]
    fn status_line_follows_the_state() {
        let mut app = winamp_app(1);
        let mut ui = WinampUiState::new();
        let (buf, _) = draw_app(&app, 100, 30, &mut ui);
        assert!(row_text(&buf, 28).starts_with(" Playlist · 1 track  ·"));

        app.error("No audio output");
        let (buf, _) = draw_app(&app, 100, 30, &mut ui);
        let status = row_text(&buf, 28);
        let at = status.find("No audio output").expect("error shown");
        let at = status[..at].chars().count() as u16;
        assert_eq!(buf[(at, 28)].fg, Color::Red);

        // Typing: the prompt names what is searched and shows a cursor.
        app.open_prompt(InputPurpose::Search);
        app.insert_text("abc");
        let (buf, _) = draw_app(&app, 100, 30, &mut ui);
        let status = row_text(&buf, 28);
        assert!(status.starts_with(" Search Local: abc "), "{status:?}");
        let cursor = &buf[(" Search Local: abc".len() as u16, 28)];
        assert!(cursor.modifier.contains(Modifier::REVERSED));
        let hints = row_text(&buf, 29);
        assert!(
            hints.contains(" Enter  search") && hints.contains(" Esc  cancel"),
            "{hints:?}"
        );
        assert!(!hints.contains("all keys"), "`?` types in the prompt");
    }

    #[test]
    fn provider_menus_draw_inside_the_playlist_editor() {
        let mut app = winamp_app(3);
        app.tab = crate::app::Tab::Provider(ProviderKind::Radio);
        let mut ui = WinampUiState::new();
        let (buf, _) = draw_app(&app, 100, 30, &mut ui);
        assert!(row_text(&buf, 29).contains(" B  browse"), "menus on offer");

        app.winamp_browse = true;
        let (buf, rows) = draw_app(&app, 100, 30, &mut ui);
        let lines = all_rows(&buf);
        assert!(
            lines[28].starts_with(" Radio ") && !lines[28].contains("Playlist"),
            "{:?}",
            lines[28]
        );
        let y = lines
            .iter()
            .position(|l| l.starts_with(" Top clicked ›"))
            .unwrap_or_else(|| panic!("{lines:#?}")) as u16;
        assert!(
            lines[usize::from(y) - 2].starts_with(" Radio "),
            "breadcrumb row"
        );
        assert!(lines[usize::from(y) - 1].starts_with(" Top voted ›"));
        assert_eq!(rows, usize::from(29 - y), "entry rows reported");
        let hints = row_text(&buf, 29);
        assert!(hints.contains(" Enter  open") && hints.contains(" Backspace  back"));
        // The first row is highlighted; clicks map through the browser rows.
        let skin = skin_with_bitmaps();
        assert_eq!(buf[(0, y - 1)].bg, skin.colors.plist_selected_bg);
        let click = crossterm::event::MouseEvent {
            kind: crossterm::event::MouseEventKind::Down(crossterm::event::MouseButton::Left),
            column: 3,
            row: y,
            modifiers: crossterm::event::KeyModifiers::NONE,
        };
        assert_eq!(ui.list_press(&click, 6), Some((1, false)));
    }

    #[test]
    fn empty_playlist_names_keys_from_the_key_table() {
        let app = winamp_app(0);
        let mut ui = WinampUiState::new();
        let (buf, _) = draw_app(&app, 100, 30, &mut ui);
        let lines = all_rows(&buf);
        let ctx = app.active_ctx();
        let key = |a| keymap::first_key(ctx, a).unwrap().to_string();
        for expected in [
            "The playlist is empty.".to_string(),
            format!("{}  open files", key(Action::Open)),
            format!("{}  search Local", key(Action::Search)),
            format!("{}  change the source (now Local)", key(Action::NextTab)),
            format!("{}  all keys", key(Action::Palette)),
        ] {
            assert!(
                lines.iter().any(|l| l.contains(&expected)),
                "{expected:?} in {lines:#?}"
            );
        }
        assert!(!lines.iter().any(|l| l.contains("for the library")));
        assert!(!lines.iter().any(|l| l.contains('—')));
        let hints = row_text(&buf, 29);
        assert!(hints.starts_with("  l  open   /  search"), "{hints:?}");
    }

    fn skin_with_bitmaps() -> WinampSkin {
        let wsz = build_wsz(&[
            ("MAIN.BMP", solid_bmp(275, 116, [40, 40, 60])),
            ("EQMAIN.BMP", solid_bmp(275, 315, [20, 60, 20])),
            ("PLEDIT.BMP", solid_bmp(280, 186, [60, 20, 20])),
            ("TEXT.BMP", solid_bmp(155, 18, [0, 0, 0])),
        ]);
        WinampSkin::from_wsz_bytes(&wsz, "t").unwrap()
    }

    #[test]
    fn halfblock_render_records_regions_and_draws_tracks() {
        let skin = skin_with_bitmaps();
        let view = sample_view(PlayStatus::Playing);
        let mut ui = WinampUiState::new();
        let mut gfx = GfxPainter::with_cell_size(Protocol::HalfBlock, None);
        let screen = Rect::new(0, 0, 280, 180);
        let mut buf = Buffer::empty(screen);
        gfx.begin_frame(screen);
        ui.begin_layout();
        let rects = render_windows(&mut buf, screen, screen, &skin, &view, &mut ui, &mut gfx);
        // Main window pixels: MAIN.BMP colour as half-blocks.
        let c = buf.cell(Position::new(2 + 50, 30)).unwrap();
        assert_eq!(c.bg, Color::Rgb(40, 40, 60));
        // Track rows: the selected track (index 2) uses SelectedBG.
        assert!(!rects.playlist.is_empty());
        let row2 = rects.playlist.y + 2;
        let cell = buf.cell(Position::new(rects.playlist.x, row2)).unwrap();
        assert_eq!(cell.symbol(), "3");
        assert_eq!(cell.bg, rgba_color(skin.pledit.selected_bg));
        // Clicking a track selects it.
        let ev = crossterm::event::MouseEvent {
            kind: crossterm::event::MouseEventKind::Down(crossterm::event::MouseButton::Left),
            column: rects.playlist.x + 3,
            row: rects.playlist.y + 1,
            modifiers: crossterm::event::KeyModifiers::NONE,
        };
        assert_eq!(
            ui.on_mouse(ev, &view),
            Some(crate::command::Command::Select(1))
        );
    }

    #[test]
    fn falls_back_to_text_layout_when_too_small() {
        let skin = skin_with_bitmaps();
        let view = sample_view(PlayStatus::Paused);
        let mut ui = WinampUiState::new();
        let mut gfx = GfxPainter::with_cell_size(Protocol::HalfBlock, None);
        let screen = Rect::new(0, 0, 80, 24);
        let mut buf = Buffer::empty(screen);
        let rects = render_windows(&mut buf, screen, screen, &skin, &view, &mut ui, &mut gfx);
        assert_eq!(rects.playlist.y, 6);
        assert_eq!(buf.cell(Position::new(37, 0)).unwrap().symbol(), "W");
    }

    #[test]
    fn graphics_render_places_images_and_skips_cells() {
        let skin = skin_with_bitmaps();
        let view = sample_view(PlayStatus::Playing);
        let mut ui = WinampUiState::new();
        let mut gfx = GfxPainter::with_cell_size(Protocol::Sixel, Some((10, 20)));
        let screen = Rect::new(0, 0, 120, 50);
        let mut buf = Buffer::empty(screen);
        gfx.begin_frame(screen);
        ui.begin_layout();
        let rects = render_windows(&mut buf, screen, screen, &skin, &view, &mut ui, &mut gfx);
        gfx.end_frame(&buf);
        // Image cells are skipped by ratatui; the seek bar maps inside the main window.
        assert!(!rects.playlist.is_empty());
        assert!(buf.content.iter().any(|c| c.skip));
        let mut out = Vec::new();
        gfx.flush(&mut out).unwrap();
        let s = String::from_utf8_lossy(&out);
        assert_eq!(s.matches("\x1bP0;1;0q").count(), 3, "main, eq and playlist");
    }
}
