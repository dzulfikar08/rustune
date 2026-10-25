//! Pixel compositors for the main, equalizer and playlist windows.
//! Element placement and state→sprite rules mirror webamp's components
//! (MainWindow/*, EqualizerWindow/*, PlaylistWindow/*).

use std::fmt::Write as _;

use crate::skin::{Bitmap, Rgba, Sheet, WinampSkin};
use crate::view::{EqSettings, PlayStatus, PlayerView, RepeatMode, EQ_BANDS, EQ_MAX_DB};

use super::canvas::Canvas;
use super::sprites::{self as sp, eq_pos, main_pos, pl_pos, px, Px};
use super::vis::{self, VisMode, VisState};
use super::Element;

/// Draw `text` with the TEXT.BMP font at (x, y), clipped to `clip`.
pub fn draw_text(
    c: &mut Canvas,
    font: Option<&Bitmap>,
    text: &str,
    x: i32,
    y: i32,
    clip: Option<Px>,
) {
    if font.is_none() {
        return;
    }
    let right = clip.map_or(i32::MAX, |r| r.x + r.w as i32);
    let left = clip.map_or(i32::MIN, |r| r.x);
    let mut cx = x;
    for ch in text.chars() {
        if cx >= right {
            break;
        }
        if cx + sp::CHAR_W as i32 > left {
            let (row, col) = sp::glyph_cell(ch);
            let src = px(
                (col * sp::CHAR_W) as i32,
                (row * sp::CHAR_H) as i32,
                sp::CHAR_W,
                sp::CHAR_H,
            );
            c.blit_clipped(font, src, cx, y, clip);
        }
        cx += sp::CHAR_W as i32;
    }
}

/// "m:ss" (minutes unpadded), webamp `getTimeStr`.
pub fn write_time(out: &mut String, secs: u64) {
    let _ = write!(out, "{}:{:02}", secs / 60, secs % 60);
}

/// Winamp's marquee: "N. Title (m:ss)"; texts of 31+ chars loop with "  ***  ".
pub fn marquee_text(out: &mut String, view: &PlayerView) {
    out.clear();
    if let Some(i) = view.current {
        let _ = write!(out, "{}. ", i + 1);
    }
    out.push_str(view.title);
    if let Some(d) = view.duration {
        out.push_str(" (");
        write_time(out, d.as_secs());
        out.push(')');
    }
}

pub const MARQUEE_MAX_CHARS: usize = 31;
const MARQUEE_SEPARATOR: &str = "  ***  ";

/// Pixel offset of the scrolling marquee for `step` (one char per step).
pub fn marquee_offset(chars: usize, step: u64) -> i32 {
    if chars < MARQUEE_MAX_CHARS {
        return 0;
    }
    let cycle = (chars + MARQUEE_SEPARATOR.len()) as u64 * u64::from(sp::CHAR_W);
    ((step * u64::from(sp::CHAR_W)) % cycle) as i32
}

/// Map dB to the 0..=1 slider fraction (0 = bottom, -12 dB).
pub fn db_to_frac(db: f32) -> f32 {
    ((db / EQ_MAX_DB + 1.0) / 2.0).clamp(0.0, 1.0)
}

/// Per-frame state for the main window.
pub struct MainCtx<'a> {
    pub focused: bool,
    pub pressed: Option<Element>,
    /// Position slider being dragged (fraction), overrides playback position.
    pub seek_preview: Option<f32>,
    pub time_remaining: bool,
    /// Paused time display blinks; false = digits hidden this frame.
    pub blink_on: bool,
    pub marquee: &'a str,
    pub marquee_step: u64,
    pub vis_mode: VisMode,
    pub vis: &'a VisState,
    pub eq_shown: bool,
    pub pl_shown: bool,
}

fn digits_sheet(skin: &WinampSkin) -> (Option<&Bitmap>, bool) {
    match skin.sheet(Sheet::NumsEx) {
        Some(b) => (Some(b), true),
        None => (skin.sheet(Sheet::Numbers), false),
    }
}

/// Right-align a number into `width` TEXT.BMP chars.
fn draw_number(c: &mut Canvas, font: Option<&Bitmap>, n: u32, width: usize, x: i32, y: i32) {
    let mut buf = [b' '; 10];
    let mut v = n;
    let mut i = buf.len();
    loop {
        i -= 1;
        buf[i] = b'0' + (v % 10) as u8;
        v /= 10;
        if v == 0 || i == 0 {
            break;
        }
    }
    let digits = &buf[i..];
    let shown = &digits[digits.len().saturating_sub(width)..];
    let pad = width.saturating_sub(shown.len()) as i32;
    if let Ok(s) = std::str::from_utf8(shown) {
        draw_text(c, font, s, x + pad * sp::CHAR_W as i32, y, None);
    }
}

pub fn compose_main(c: &mut Canvas, skin: &WinampSkin, view: &PlayerView, ctx: &MainCtx<'_>) {
    c.resize(sp::WINDOW_W, sp::WINDOW_H);
    c.fill([0, 0, 0, 255]);
    let main = skin.sheet(Sheet::Main);
    let title = skin.sheet(Sheet::Titlebar);
    let cbuttons = skin.sheet(Sheet::Cbuttons);
    let shufrep = skin.sheet(Sheet::Shufrep);
    let font = skin.sheet(Sheet::Text);
    let stopped = view.status == PlayStatus::Stopped;
    let pressed = |e: Element| ctx.pressed == Some(e);

    c.blit(main, sp::MAIN_BG, 0, 0);

    // Title bar + depressed title buttons.
    let tb = if ctx.focused {
        sp::TITLE_BAR_SELECTED
    } else {
        sp::TITLE_BAR
    };
    c.blit(title, tb, 0, 0);
    for (el, sprite, pos) in [
        (Element::Options, sp::OPTIONS_DEPRESSED, main_pos::OPTIONS),
        (
            Element::Minimize,
            sp::MINIMIZE_DEPRESSED,
            main_pos::MINIMIZE,
        ),
        (Element::Shade, sp::SHADE_DEPRESSED, main_pos::SHADE),
        (Element::Close, sp::CLOSE_DEPRESSED, main_pos::CLOSE),
    ] {
        if pressed(el) {
            c.blit(title, sprite, pos.x, pos.y);
        }
    }
    c.blit(
        title,
        sp::CLUTTER_BAR,
        main_pos::CLUTTER_BAR.x,
        main_pos::CLUTTER_BAR.y,
    );

    // Play state indicator.
    let playpaus = skin.sheet(Sheet::Playpaus);
    let indicator = match view.status {
        PlayStatus::Playing => sp::PLAYING_INDICATOR,
        PlayStatus::Paused => sp::PAUSED_INDICATOR,
        PlayStatus::Stopped => sp::STOPPED_INDICATOR,
    };
    c.blit(
        playpaus,
        indicator,
        main_pos::PLAY_PAUSE.x,
        main_pos::PLAY_PAUSE.y,
    );
    if view.status == PlayStatus::Playing {
        c.blit(
            playpaus,
            sp::NOT_WORKING_INDICATOR,
            main_pos::WORK_INDICATOR.x,
            main_pos::WORK_INDICATOR.y,
        );
    }

    // Time (hidden when stopped, blinking when paused).
    if !stopped && ctx.blink_on {
        let elapsed = view.elapsed.as_secs();
        let remaining = ctx.time_remaining && view.duration.is_some();
        let secs = match view.duration {
            Some(d) if remaining => d.as_secs().saturating_sub(elapsed),
            _ => elapsed,
        };
        let minutes = (secs / 60).min(99);
        let seconds = secs % 60;
        let digits = [minutes / 10, minutes % 10, seconds / 10, seconds % 10];
        let (sheet, ex) = digits_sheet(skin);
        let t = main_pos::TIME;
        if remaining {
            if ex {
                c.blit(sheet, sp::MINUS_SIGN_EX, t.x - 1, t.y);
            } else {
                c.blit(sheet, sp::MINUS_SIGN, t.x - 1, t.y + 6);
            }
        }
        for (d, dx) in digits.iter().zip(main_pos::DIGIT_X) {
            let src = px(*d as i32 * sp::DIGIT_W as i32, 0, sp::DIGIT_W, sp::DIGIT_H);
            c.blit(sheet, src, t.x + dx, t.y);
        }
    }

    // Visualizer (absent while stopped; frozen while paused).
    if !stopped && ctx.vis_mode != VisMode::Off {
        let v = main_pos::VIS;
        vis::draw_background(c, v.x, v.y, &skin.viscolor);
        match ctx.vis_mode {
            VisMode::Analyzer => vis::draw_analyzer(c, v.x, v.y, &skin.viscolor, ctx.vis),
            VisMode::Oscilloscope => vis::draw_scope(c, v.x, v.y, &skin.viscolor, view.scope),
            VisMode::Off => {}
        }
    }

    // Marquee.
    let m = main_pos::MARQUEE;
    let chars = ctx.marquee.chars().count();
    let offset = marquee_offset(chars, ctx.marquee_step);
    let clip = Some(px(m.x, m.y, m.w, m.h));
    let y = main_pos::MARQUEE_TEXT_Y;
    if chars >= MARQUEE_MAX_CHARS {
        // text + separator + text, scrolled left by `offset`.
        let mut x = m.x - offset;
        for part in [ctx.marquee, MARQUEE_SEPARATOR, ctx.marquee] {
            draw_text(c, font, part, x, y, clip);
            x += (part.chars().count() as u32 * sp::CHAR_W) as i32;
        }
    } else {
        draw_text(c, font, ctx.marquee, m.x, y, clip);
    }

    // kbps / kHz (hidden when stopped).
    if !stopped {
        if let Some(k) = view.format.kbps {
            draw_number(c, font, k.min(999), 3, main_pos::KBPS.x, main_pos::KBPS.y);
        }
        if let Some(sr) = view.format.sample_rate {
            let khz = ((sr + 500) / 1000).min(99);
            draw_number(c, font, khz, 2, main_pos::KHZ.x, main_pos::KHZ.y);
        }
    }

    // Mono / stereo.
    let monoster = skin.sheet(Sheet::Monoster);
    let ch = if stopped {
        0
    } else {
        view.format.channels.unwrap_or(0)
    };
    let mono = if ch == 1 { sp::MONO_SELECTED } else { sp::MONO };
    let stereo = if ch >= 2 {
        sp::STEREO_SELECTED
    } else {
        sp::STEREO
    };
    c.blit(monoster, mono, main_pos::MONO.x, main_pos::MONO.y);
    c.blit(monoster, stereo, main_pos::STEREO.x, main_pos::STEREO.y);

    // Volume: background frame round(v*28)-1 (webamp MainVolume), thumb travel 51.
    let volume_sheet = skin.sheet(Sheet::Volume);
    let vol = view.volume.clamp(0.0, 1.0);
    let frame = ((vol * 28.0).round() as i32 - 1).max(0);
    let vp = main_pos::VOLUME;
    c.blit(
        volume_sheet,
        sp::VOLUME_FRAME.offset(0, frame * sp::SLIDER_FRAME_DY),
        vp.x,
        vp.y,
    );
    let thumb = if pressed(Element::Volume) {
        sp::SLIDER_THUMB_ACTIVE
    } else {
        sp::SLIDER_THUMB
    };
    c.blit(
        volume_sheet,
        thumb,
        vp.x + (vol * sp::VOLUME_TRAVEL).round() as i32,
        vp.y + 1,
    );

    // Balance: frame floor(|b|*27) (webamp offsetFromBalance); BALANCE falls back to VOLUME.
    let balance_sheet = skin.sheet(Sheet::Balance).or(volume_sheet);
    let bal = view.balance.clamp(-1.0, 1.0);
    let frame = (bal.abs() * 27.0).floor() as i32;
    let bp = main_pos::BALANCE;
    c.blit(
        balance_sheet,
        sp::BALANCE_FRAME.offset(0, frame * sp::SLIDER_FRAME_DY),
        bp.x,
        bp.y,
    );
    let thumb = if pressed(Element::Balance) {
        sp::SLIDER_THUMB_ACTIVE
    } else {
        sp::SLIDER_THUMB
    };
    c.blit(
        balance_sheet,
        thumb,
        bp.x + ((bal + 1.0) / 2.0 * sp::BALANCE_TRAVEL).round() as i32,
        bp.y + 1,
    );

    // EQ / PL window toggles.
    for (el, sprite, pos, on) in [
        (
            Element::EqButton,
            sp::EQ_BUTTON,
            main_pos::EQ_BUTTON,
            ctx.eq_shown,
        ),
        (
            Element::PlButton,
            sp::PL_BUTTON,
            main_pos::PL_BUTTON,
            ctx.pl_shown,
        ),
    ] {
        let mut s = sprite;
        if on {
            s = s.offset(0, sp::WINDOW_BUTTON_SELECTED_DY);
        }
        if pressed(el) {
            s = s.offset(sp::WINDOW_BUTTON_DEPRESSED_DX, 0);
        }
        c.blit(shufrep, s, pos.x, pos.y);
    }

    // Position slider: thumb hidden while stopped or for streams without length.
    let pos = main_pos::POSITION;
    let posbar = skin.sheet(Sheet::Posbar);
    c.blit(posbar, sp::POSBAR_BG, pos.x, pos.y);
    let frac = ctx.seek_preview.or_else(|| {
        let d = view.duration?.as_secs_f32();
        (d > 0.0).then(|| view.elapsed.as_secs_f32() / d)
    });
    if let (false, Some(f)) = (stopped, frac) {
        let thumb = if ctx.seek_preview.is_some() {
            sp::POSBAR_THUMB_SELECTED
        } else {
            sp::POSBAR_THUMB
        };
        let x = pos.x + (f.clamp(0.0, 1.0) * sp::POSBAR_TRAVEL).round() as i32;
        c.blit(posbar, thumb, x, pos.y);
    }

    // Transport buttons.
    for (el, sprite, at) in [
        (Element::Prev, sp::PREVIOUS, main_pos::PREVIOUS),
        (Element::Play, sp::PLAY, main_pos::PLAY),
        (Element::Pause, sp::PAUSE, main_pos::PAUSE),
        (Element::Stop, sp::STOP, main_pos::STOP),
        (Element::Next, sp::NEXT, main_pos::NEXT),
    ] {
        let s = if pressed(el) {
            sprite.offset(0, sp::CBUTTON_PRESSED_DY)
        } else {
            sprite
        };
        c.blit(cbuttons, s, at.x, at.y);
    }
    let eject = if pressed(Element::Eject) {
        sp::EJECT.offset(0, sp::EJECT_PRESSED_DY)
    } else {
        sp::EJECT
    };
    c.blit(cbuttons, eject, main_pos::EJECT.x, main_pos::EJECT.y);

    // Shuffle / repeat.
    let row = |on: bool, down: bool| sp::SHUFREP_ROWS[usize::from(on) * 2 + usize::from(down)];
    c.blit(
        shufrep,
        sp::SHUFFLE.offset(0, row(view.shuffle, pressed(Element::Shuffle))),
        main_pos::SHUFFLE.x,
        main_pos::SHUFFLE.y,
    );
    c.blit(
        shufrep,
        sp::REPEAT.offset(
            0,
            row(view.repeat != RepeatMode::Off, pressed(Element::Repeat)),
        ),
        main_pos::REPEAT.x,
        main_pos::REPEAT.y,
    );
}

/// Natural cubic spline through (xs, ys) evaluated at every integer x in
/// `0..=xs[last]` (webamp `spline.js`, solved as a tridiagonal system).
pub fn spline(xs: &[f32; EQ_BANDS], ys: &[f32; EQ_BANDS], out: &mut [f32]) {
    let n = EQ_BANDS;
    // ks = first derivatives at the knots; tridiagonal system a·k[i-1] + b·k[i] + c·k[i+1] = r.
    let mut a = [0.0f32; EQ_BANDS];
    let mut b = [0.0f32; EQ_BANDS];
    let mut cc = [0.0f32; EQ_BANDS];
    let mut r = [0.0f32; EQ_BANDS];
    for i in 0..n {
        if i > 0 {
            let h = xs[i] - xs[i - 1];
            a[i] = 1.0 / h;
            b[i] += 2.0 / h;
            r[i] += 3.0 * (ys[i] - ys[i - 1]) / (h * h);
        }
        if i + 1 < n {
            let h = xs[i + 1] - xs[i];
            cc[i] = 1.0 / h;
            b[i] += 2.0 / h;
            r[i] += 3.0 * (ys[i + 1] - ys[i]) / (h * h);
        }
    }
    // Thomas algorithm.
    for i in 1..n {
        let m = a[i] / b[i - 1];
        b[i] -= m * cc[i - 1];
        r[i] -= m * r[i - 1];
    }
    let mut k = [0.0f32; EQ_BANDS];
    k[n - 1] = r[n - 1] / b[n - 1];
    for i in (0..n - 1).rev() {
        k[i] = (r[i] - cc[i] * k[i + 1]) / b[i];
    }
    let mut seg = 1;
    for (x, o) in out.iter_mut().enumerate() {
        let x = x as f32;
        while seg < n - 1 && xs[seg] < x {
            seg += 1;
        }
        let (x0, x1, y0, y1) = (xs[seg - 1], xs[seg], ys[seg - 1], ys[seg]);
        let h = x1 - x0;
        let t = (x - x0) / h;
        let aa = k[seg - 1] * h - (y1 - y0);
        let bb = -k[seg] * h + (y1 - y0);
        *o = (1.0 - t) * y0 + t * y1 + t * (1.0 - t) * (aa * (1.0 - t) + bb * t);
    }
}

pub struct EqCtx {
    pub focused: bool,
    pub pressed: Option<Element>,
}

pub fn compose_eq(c: &mut Canvas, skin: &WinampSkin, eq: &EqSettings, ctx: &EqCtx) {
    c.resize(sp::WINDOW_W, sp::WINDOW_H);
    c.fill([0, 0, 0, 255]);
    let s = skin.sheet(Sheet::Eqmain);
    let pressed = |e: Element| ctx.pressed == Some(e);
    c.blit(s, sp::EQ_BG, 0, 0);
    let title = if ctx.focused {
        sp::EQ_TITLE_BAR_SELECTED
    } else {
        sp::EQ_TITLE_BAR
    };
    c.blit(s, title, 0, 0);
    if pressed(Element::EqClose) {
        c.blit(s, sp::EQ_CLOSE_ACTIVE, eq_pos::CLOSE.x, eq_pos::CLOSE.y);
    }

    let on = match (eq.enabled, pressed(Element::EqOn)) {
        (false, false) => sp::EQ_ON,
        (false, true) => sp::EQ_ON_DEPRESSED,
        (true, false) => sp::EQ_ON_SELECTED,
        (true, true) => sp::EQ_ON_SELECTED_DEPRESSED,
    };
    c.blit(s, on, eq_pos::ON.x, eq_pos::ON.y);
    let auto = match (eq.auto, pressed(Element::EqAuto)) {
        (false, false) => sp::EQ_AUTO,
        (false, true) => sp::EQ_AUTO_DEPRESSED,
        (true, false) => sp::EQ_AUTO_SELECTED,
        (true, true) => sp::EQ_AUTO_SELECTED_DEPRESSED,
    };
    c.blit(s, auto, eq_pos::AUTO.x, eq_pos::AUTO.y);
    let presets = if pressed(Element::EqPresets) {
        sp::EQ_PRESETS_SELECTED
    } else {
        sp::EQ_PRESETS
    };
    c.blit(s, presets, eq_pos::PRESETS.x, eq_pos::PRESETS.y);

    // Response graph.
    let g = eq_pos::GRAPH;
    c.blit(s, sp::EQ_GRAPH_BG, g.x, g.y);
    let gh = sp::EQ_GRAPH_BG.h as i32;
    let max_y = (gh - 1) as f32;
    let preamp_y = ((1.0 - db_to_frac(eq.preamp_db)) * max_y).round() as i32;
    c.blit(s, sp::EQ_PREAMP_LINE, g.x, g.y + preamp_y);
    let mut line_colors = [[0u8; 4]; 19];
    let lc = sp::EQ_GRAPH_LINE_COLORS;
    let have_line_colors = s.is_some_and(|b| b.get(lc.x as u32, (lc.y + gh - 1) as u32).is_some());
    if let (Some(b), true) = (s, have_line_colors) {
        for (i, col) in line_colors.iter_mut().enumerate() {
            *col = b
                .get(lc.x as u32, lc.y as u32 + i as u32)
                .unwrap_or([0, 0, 0, 255]);
        }
        let mut xs = [0.0f32; EQ_BANDS];
        let mut ys = [0.0f32; EQ_BANDS];
        for i in 0..EQ_BANDS {
            xs[i] = (i * 12) as f32;
            ys[i] = (1.0 - db_to_frac(eq.bands_db[i])) * max_y;
        }
        let mut curve = [0.0f32; 109];
        spline(&xs, &ys, &mut curve);
        let mut last = ys[0].round() as i32;
        for (x, v) in curve.iter().enumerate() {
            let y = (v.round() as i32).clamp(0, gh - 1);
            let top = y.min(last);
            let bottom = y.max(last);
            for yy in top..=bottom {
                c.set(g.x + 2 + x as i32, g.y + yy, line_colors[yy as usize]);
            }
            last = y;
        }
    }

    // Sliders: preamp + 10 bands.
    let thumb_src = |el: Element| {
        if pressed(el) {
            sp::EQ_SLIDER_THUMB_SELECTED
        } else {
            sp::EQ_SLIDER_THUMB
        }
    };
    let draw_slider = |c: &mut Canvas, at: Px, db: f32, el: Element| {
        let v = db_to_frac(db);
        let n = (v * 27.0).round() as i32;
        let (fx, fy) = (n % 14, n / 14);
        let frame = px(
            sp::EQ_SLIDER_BG.x + fx * 15,
            sp::EQ_SLIDER_BG.y + fy * 65,
            sp::EQ_BAND_FRAME_W,
            sp::EQ_BAND_FRAME_H,
        );
        c.blit(s, frame, at.x, at.y);
        let ty = at.y + ((1.0 - v) * sp::EQ_THUMB_TRAVEL).round() as i32;
        c.blit(s, thumb_src(el), at.x + 1, ty);
    };
    draw_slider(c, eq_pos::PREAMP, eq.preamp_db, Element::Preamp);
    for (i, db) in eq.bands_db.iter().enumerate() {
        let at = eq_pos::BAND0.offset(i as i32 * eq_pos::BAND_DX, 0);
        draw_slider(c, at, *db, Element::EqBand(i as u8));
    }
}

/// One terminal text row over the playlist, in window pixels.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RowSpan {
    pub y0: i32,
    pub y1: i32,
    pub track: Option<usize>,
}

pub struct PlaylistCtx<'a> {
    pub focused: bool,
    pub pressed: Option<Element>,
    pub height: u32,
    pub rows: &'a [RowSpan],
    /// 0..=1 scroll position for the handle.
    pub scroll_frac: f32,
    pub scratch: &'a mut String,
}

/// Playlist height for `rows` 29px middle segments (Winamp resizes in 29px steps).
pub fn playlist_height(segments: u32) -> u32 {
    sp::PL_TOP_H + sp::PL_ROW_STEP * segments.max(1) + sp::PL_BOTTOM_H
}

/// Inner track area (x, y, w, h) for a playlist of height `h`.
pub fn playlist_track_area(h: u32) -> Px {
    px(
        sp::PL_LEFT_W as i32,
        sp::PL_TOP_H as i32,
        sp::WINDOW_W - sp::PL_LEFT_W - sp::PL_RIGHT_W,
        h.saturating_sub(sp::PL_TOP_H + sp::PL_BOTTOM_H),
    )
}

pub fn compose_playlist(
    c: &mut Canvas,
    skin: &WinampSkin,
    view: &PlayerView,
    ctx: PlaylistCtx<'_>,
) {
    let h = ctx.height;
    let w = sp::WINDOW_W as i32;
    c.resize(sp::WINDOW_W, h);
    c.fill([0, 0, 0, 255]);
    let s = skin.sheet(Sheet::Pledit);
    let font = skin.sheet(Sheet::Text);

    // Top bar.
    let (tile, left, title, right) = if ctx.focused {
        (
            sp::PL_TOP_TILE_SELECTED,
            sp::PL_TOP_LEFT_SELECTED,
            sp::PL_TITLE_SELECTED,
            sp::PL_TOP_RIGHT_SELECTED,
        )
    } else {
        (
            sp::PL_TOP_TILE,
            sp::PL_TOP_LEFT,
            sp::PL_TITLE,
            sp::PL_TOP_RIGHT,
        )
    };
    c.tile_h(s, tile, 25, w - 25, 0);
    c.blit(s, left, 0, 0);
    c.blit(s, title, pl_pos::TITLE_X, 0);
    c.blit(s, right, w - 25, 0);
    if ctx.pressed == Some(Element::PlClose) {
        c.blit(s, sp::PL_CLOSE_SELECTED, pl_pos::CLOSE.x, pl_pos::CLOSE.y);
    }

    // Middle: side tiles + track area.
    let bottom = pl_pos::bottom_y(h);
    c.tile_v(s, sp::PL_LEFT_TILE, 0, sp::PL_TOP_H as i32, bottom);
    c.tile_v(
        s,
        sp::PL_RIGHT_TILE,
        w - sp::PL_RIGHT_W as i32,
        sp::PL_TOP_H as i32,
        bottom,
    );
    let area = playlist_track_area(h);
    c.fill_rect(area, skin.pledit.normal_bg);
    for row in ctx.rows {
        if row.track.is_some() && row.track == Some(view.selected) {
            let y0 = row.y0.max(area.y);
            let y1 = row.y1.min(area.y + area.h as i32);
            if y1 > y0 {
                c.fill_rect(
                    px(area.x, y0, area.w, (y1 - y0) as u32),
                    skin.pledit.selected_bg,
                );
            }
        }
    }
    let travel = (area.h as i32 - sp::PL_SCROLL_HANDLE.h as i32).max(0);
    let handle_y = area.y + (ctx.scroll_frac.clamp(0.0, 1.0) * travel as f32).round() as i32;
    let handle = if ctx.pressed == Some(Element::PlScroll) {
        sp::PL_SCROLL_HANDLE_SELECTED
    } else {
        sp::PL_SCROLL_HANDLE
    };
    c.blit(s, handle, pl_pos::SCROLL_X, handle_y);

    // Bottom bar.
    c.tile_h(s, sp::PL_BOTTOM_TILE, 125, w - 150, bottom);
    c.blit(s, sp::PL_BOTTOM_LEFT, 0, bottom);
    c.blit(s, sp::PL_BOTTOM_RIGHT, w - 150, bottom);

    // Running time "selected/total" and mini time, in the TEXT.BMP font.
    let sel = view
        .playlist
        .get(view.selected)
        .and_then(|m| m.duration)
        .unwrap_or(0);
    let total: u64 = view.playlist.iter().filter_map(|m| m.duration).sum();
    ctx.scratch.clear();
    write_time(ctx.scratch, sel);
    ctx.scratch.push('/');
    write_time(ctx.scratch, total);
    let (rx, ry) = pl_pos::running_time(h);
    draw_text(c, font, ctx.scratch, rx, ry, Some(px(rx, ry, 70, 6)));
    if view.status != PlayStatus::Stopped {
        ctx.scratch.clear();
        write_time(ctx.scratch, view.elapsed.as_secs());
        let (mx, my) = pl_pos::mini_time(h);
        draw_text(c, font, ctx.scratch, mx, my, Some(px(mx, my, 25, 6)));
    }
}

/// Simple text colour helper for callers that draw playlist text cells.
pub fn row_colors(skin: &WinampSkin, view: &PlayerView, track: usize) -> (Rgba, Rgba) {
    let fg = if Some(track) == view.current {
        skin.pledit.current
    } else {
        skin.pledit.normal
    };
    let bg = if track == view.selected {
        skin.pledit.selected_bg
    } else {
        skin.pledit.normal_bg
    };
    (fg, bg)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn spline_passes_through_knots_and_is_flat_for_flat_input() {
        let mut xs = [0.0; EQ_BANDS];
        for (i, x) in xs.iter_mut().enumerate() {
            *x = (i * 12) as f32;
        }
        let ys = [9.0; EQ_BANDS];
        let mut out = [0.0; 109];
        spline(&xs, &ys, &mut out);
        assert!(out.iter().all(|v| (*v - 9.0).abs() < 1e-3));

        let ys = [0.0, 18.0, 0.0, 18.0, 0.0, 18.0, 0.0, 18.0, 0.0, 18.0];
        spline(&xs, &ys, &mut out);
        for (i, y) in ys.iter().enumerate() {
            assert!((out[i * 12] - y).abs() < 1e-3, "knot {i}");
        }
    }

    #[test]
    fn marquee_offsets_loop() {
        assert_eq!(marquee_offset(10, 5), 0);
        // 31 chars + 7 separator = 38 chars = 190 px cycle.
        assert_eq!(marquee_offset(31, 1), 5);
        assert_eq!(marquee_offset(31, 38), 0);
        assert_eq!(marquee_offset(31, 39), 5);
    }

    #[test]
    fn db_fraction_bounds() {
        assert_eq!(db_to_frac(-12.0), 0.0);
        assert_eq!(db_to_frac(0.0), 0.5);
        assert_eq!(db_to_frac(40.0), 1.0);
    }

    #[test]
    fn playlist_geometry() {
        assert_eq!(playlist_height(2), 116);
        assert_eq!(playlist_height(0), 87);
        let a = playlist_track_area(116);
        assert_eq!((a.x, a.y, a.w, a.h), (12, 20, 243, 58));
    }
}
