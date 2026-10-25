//! Winamp skin view: the classic main, equalizer and playlist windows drawn
//! from the skin's sprites — through a pixel graphics protocol when the
//! terminal has one, as 1:1 half-blocks when it is big enough, and as a
//! skin-coloured text layout otherwise.

use std::fmt::Write as _;

use ratatui::buffer::Buffer;
use ratatui::layout::{Position, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::Frame;

use crate::app::{App, LayoutRects, Mode, Status};
use crate::gfx::{GfxPainter, Protocol};
use crate::media::SourceKind;
use crate::skin::{rgba_color, SkinColors, WinampSkin};
use crate::ui::skin_bitmap::draw_halfblock;
use crate::ui::skin_layout::{self, Plan, WindowPlan};
use crate::view::{EqSettings, PlayStatus, PlayerView, RepeatMode, StreamFormat};
use crate::winamp::compose::{self, RowSpan};
use crate::winamp::sprites::main_pos;
use crate::winamp::vis::{self, BARS};
use crate::winamp::{Element, Region, WinampUiState, WindowGeom};

/// Cells of interest after rendering.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct WinampRects {
    /// Playlist text rows.
    pub playlist: Rect,
    /// Position (seek) bar.
    pub seek: Rect,
    /// Pause button.
    pub pause: Rect,
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
    let mut rects = WinampRects {
        playlist: Rect::default(),
        seek: plan.cells_of(&plan.main, main_pos::POSITION),
        pause: plan.cells_of(&plan.main, main_pos::PAUSE),
    };
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
    for (i, row) in ui.pl_rows.iter_mut().enumerate() {
        let idx = first + i;
        row.track = (idx < view.playlist.len()).then_some(idx);
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
        if let Some(idx) =
            (first + usize::from(i) < view.playlist.len()).then_some(first + usize::from(i))
        {
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
        rects.seek = bar;
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
            if el == Element::Pause {
                rects.pause = Rect::new(start, r, cx.saturating_sub(start), 1);
            }
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
        for i in 0..left {
            let idx = first + usize::from(i);
            if idx >= view.playlist.len() {
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

// ─── Legacy entry point (until the app passes a PlayerView) ────────────

/// Compatibility adapter for the current `ui::draw`: builds a `PlayerView`
/// from `App` fields and renders it (half-block or text), plus the
/// search/help footer, and records `layout_rects` for the legacy mouse code.
pub fn render(frame: &mut Frame, app: &mut App) {
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

    let (title, status, elapsed, duration) = match app.playback.as_ref() {
        Some(pb) => (
            pb.title.as_str(),
            if pb.paused {
                PlayStatus::Paused
            } else {
                PlayStatus::Playing
            },
            std::time::Duration::from_secs(pb.elapsed_secs),
            (pb.duration_secs > 0).then(|| std::time::Duration::from_secs(pb.duration_secs)),
        ),
        None => ("", PlayStatus::Stopped, std::time::Duration::ZERO, None),
    };
    let current = app
        .playback
        .as_ref()
        .and_then(|pb| app.results.iter().position(|r| r.title == pb.title));
    let view = PlayerView {
        title,
        status,
        elapsed,
        duration,
        volume: 1.0,
        balance: 0.0,
        shuffle: false,
        repeat: RepeatMode::Off,
        eq: EqSettings::default(),
        format: StreamFormat::default(),
        spectrum: &[],
        scope: &[],
        playlist: &app.results,
        current,
        selected: app.list_state.selected().unwrap_or(0),
    };

    let rects = match (app.winamp_skin.as_ref(), app.skin_layout.as_mut()) {
        (Some(skin), Some(layout)) => render_view(
            frame,
            body,
            skin,
            &view,
            &mut layout.ui,
            &mut layout.painter,
        ),
        (skin, _) => {
            let sc = skin.map_or(&SkinColors::DEFAULT, |s| &s.colors);
            let mut ui = WinampUiState::new();
            render_text(frame.buffer_mut(), body, sc, &view, &mut ui)
        }
    };

    let sc = app
        .winamp_skin
        .as_ref()
        .map_or(&SkinColors::DEFAULT, |s| &s.colors);
    if app.results.is_empty() && !rects.playlist.is_empty() {
        let msg = match &app.status {
            Status::Searching(t)
            | Status::Scanning(t)
            | Status::Downloading(t)
            | Status::Error(t) => t.as_str(),
            _ => "No tracks loaded — press / to search",
        };
        let r = rects.playlist;
        put(
            frame.buffer_mut(),
            r.x + 1,
            r.y,
            r.right(),
            msg,
            Style::default().fg(sc.plist_normal).bg(sc.plist_normal_bg),
        );
    }
    render_footer(frame.buffer_mut(), footer, app, sc);

    // Legacy click mapping expects one header row above the results.
    let pl = rects.playlist;
    let results = if pl.is_empty() {
        pl
    } else {
        Rect::new(pl.x, pl.y.saturating_sub(1), pl.width, pl.height + 1)
    };
    app.layout_rects = LayoutRects {
        results,
        player_info: Rect::new(body.x, body.y, body.width, body.height.min(1)),
        player_bar: rects.seek,
        input: Rect::new(footer.x, footer.y, footer.width, footer.height.min(1)),
        help: Rect::new(
            footer.x,
            footer.y + 1,
            footer.width,
            footer.height.saturating_sub(1),
        ),
        pause_button: rects.pause,
        prev_page: Rect::default(),
        next_page: Rect::default(),
    };
}

fn render_footer(buf: &mut Buffer, area: Rect, app: &App, sc: &SkinColors) {
    if area.height == 0 {
        return;
    }
    let right = area.right();
    let input = Style::default().fg(sc.text_fg).bg(sc.text_bg);
    fill(buf, Rect::new(area.x, area.y, area.width, 1), input);
    let prompt = match app.mode {
        Mode::Browse => " > ",
        Mode::Input => " / ",
        _ => " ? ",
    };
    let mut cx = put(
        buf,
        area.x,
        area.y,
        right,
        prompt,
        input.add_modifier(Modifier::BOLD),
    );
    cx = put(buf, cx, area.y, right, &app.input_text, input);
    if app.mode == Mode::Input {
        put(buf, cx, area.y, right, "█", input);
    }
    if area.height < 2 {
        return;
    }
    let y = area.y + 1;
    let key = Style::default().fg(sc.led_on).bg(sc.chrome_dark);
    let label = Style::default().fg(sc.chrome_mid).bg(sc.chrome_dark);
    fill(buf, Rect::new(area.x, y, area.width, 1), label);
    let hints: &[(&str, &str)] = match app.mode {
        Mode::Browse if matches!(app.active_source, SourceKind::Extractor(_)) => &[
            (" /", " search "),
            ("j/k", " nav "),
            ("Enter", " play "),
            ("d", " dl "),
            ("Space", " pause "),
            ("Tab", " src "),
            ("S", " settings "),
            ("q", " quit"),
        ],
        Mode::Browse => &[
            (" /", " search "),
            ("j/k", " nav "),
            ("Enter", " play "),
            ("Space", " pause "),
            ("Tab", " src "),
            ("S", " settings "),
            ("q", " quit"),
        ],
        Mode::Input => &[(" Enter", " submit "), ("Esc", " cancel")],
        _ => &[],
    };
    let mut cx = area.x;
    for (k, l) in hints {
        cx = put(buf, cx, y, right, k, key);
        cx = put(buf, cx, y, right, l, label);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::skin::tests::{build_wsz, solid_bmp};
    use crate::winamp::tests::sample_view;

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
        let mut buf = Buffer::empty(Rect::new(0, 0, 280, 180));
        gfx.begin_frame(buf.area);
        ui.begin_layout();
        let rects = render_windows(
            &mut buf, buf.area, buf.area, &skin, &view, &mut ui, &mut gfx,
        );
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
        let mut buf = Buffer::empty(Rect::new(0, 0, 80, 24));
        let rects = render_windows(
            &mut buf, buf.area, buf.area, &skin, &view, &mut ui, &mut gfx,
        );
        assert_eq!(rects.playlist.y, 6);
        assert_eq!(buf.cell(Position::new(37, 0)).unwrap().symbol(), "W");
        // Pressing the text "play" button returns Play on release.
        let p = rects.pause;
        assert!(p.width > 0);
    }

    #[test]
    fn graphics_render_places_images_and_skips_cells() {
        let skin = skin_with_bitmaps();
        let view = sample_view(PlayStatus::Playing);
        let mut ui = WinampUiState::new();
        let mut gfx = GfxPainter::with_cell_size(Protocol::Sixel, Some((10, 20)));
        let mut buf = Buffer::empty(Rect::new(0, 0, 120, 50));
        gfx.begin_frame(buf.area);
        ui.begin_layout();
        let rects = render_windows(
            &mut buf, buf.area, buf.area, &skin, &view, &mut ui, &mut gfx,
        );
        gfx.end_frame(&buf);
        // Image cells are skipped by ratatui; the seek bar maps inside the main window.
        assert!(
            buf.cell(Position::new(rects.seek.x, rects.seek.y))
                .unwrap()
                .skip
        );
        let mut out = Vec::new();
        gfx.flush(&mut out).unwrap();
        let s = String::from_utf8_lossy(&out);
        assert_eq!(s.matches("\x1bP0;1;0q").count(), 3, "main, eq and playlist");
    }
}
