/// Winamp 2.x–faithful renderer.
///
/// Reproduces the classic three-panel Winamp layout as closely as a TUI allows:
///
///  ┌─────────────────── MAIN WINDOW ───────────────────┐
///  │ [Title Bar]          skin name          [_][S][X]  │  row 0
///  │ [Clutterbar]  [LED TIME]   [VIS area ··········]   │  row 1
///  │ [ scrolling track title marquee ···············]   │  row 2
///  │ [=== seek / position bar =======================]  │  row 3
///  │ [|◀][◀◀][▶][■][▶▶][▶|]  VOL ████░░  BAL ██░░░░  │  row 4
///  │ [SHUF][REP]  [EQ][PL]   mono/stereo  kbps  kHz   │  row 5
///  ├───────────────── PLAYLIST EDITOR ─────────────────┤  row 6
///  │  1. Artist - Title                        3:45    │
///  │▸ 2. Artist - Title (playing)              4:12    │
///  │  3. Artist - Title                        2:58    │
///  │  ···                                              │
///  ├───────────────────────────────────────────────────┤
///  │ / search prompt █                                 │  footer 0
///  │ /search  j/k nav  Enter play  Tab src  q quit    │  footer 1
///  └──────────────────────────────────────────────────┘
use ratatui::{
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, HighlightSpacing, LineGauge, List, ListItem, Padding, Paragraph},
    Frame,
};

use crate::app::{App, LayoutRects, Mode, Status};
use crate::skin::WinampSkin;
use crate::ui::skin_bitmap;

// ─── Resolved skin colors ──────────────────────────────────────────────

struct SC {
    chrome_dark: Color,
    chrome_mid: Color,
    chrome_light: Color,
    body_bg: Color,
    titlebar_bg: Color,
    led_on: Color,
    led_off: Color,
    led_bg: Color,
    text_fg: Color,
    text_bg: Color,
    plist_normal: Color,
    plist_current: Color,
    plist_normal_bg: Color,
    plist_selected_bg: Color,
    vis_colors: Vec<Color>,
    btn_normal: Color,
    btn_text: Color,
    seek_track: Color,
    seek_filled: Color,
    play_indicator: Color,
    pause_indicator: Color,
    indicator_on: Color,
    indicator_off: Color,
}

impl SC {
    fn from_app(app: &App) -> Self {
        match &app.winamp_skin {
            Some(skin) => Self::from_skin(skin),
            None => Self::defaults(),
        }
    }

    fn from_skin(s: &WinampSkin) -> Self {
        Self {
            chrome_dark: s.chrome_dark,
            chrome_mid: s.chrome_mid,
            chrome_light: s.chrome_light,
            body_bg: s.body_bg,
            titlebar_bg: s.titlebar_bg,
            led_on: s.led_on,
            led_off: s.led_off,
            led_bg: Color::Rgb(0, 20, 0),
            text_fg: s.text_fg,
            text_bg: s.text_bg,
            plist_normal: s.plist_normal,
            plist_current: s.plist_current,
            plist_normal_bg: s.plist_normal_bg,
            plist_selected_bg: s.plist_selected_bg,
            vis_colors: s.vis_colors.clone(),
            btn_normal: s.btn_normal,
            btn_text: s.btn_text,
            seek_track: s.seek_track,
            seek_filled: s.seek_filled,
            play_indicator: s.play_indicator,
            pause_indicator: s.pause_indicator,
            indicator_on: s.indicator_on,
            indicator_off: s.indicator_off,
        }
    }

    fn defaults() -> Self {
        Self {
            chrome_dark: Color::Rgb(8, 8, 16),
            chrome_mid: Color::Rgb(123, 140, 156),
            chrome_light: Color::Rgb(189, 206, 214),
            body_bg: Color::Rgb(57, 57, 90),
            titlebar_bg: Color::Rgb(0, 198, 255),
            led_on: Color::Rgb(0, 248, 0),
            led_off: Color::Rgb(24, 33, 41),
            led_bg: Color::Rgb(0, 20, 0),
            text_fg: Color::Rgb(0, 226, 0),
            text_bg: Color::Rgb(0, 0, 165),
            plist_normal: Color::Rgb(0, 255, 0),
            plist_current: Color::White,
            plist_normal_bg: Color::Black,
            plist_selected_bg: Color::Rgb(0, 0, 198),
            vis_colors: vec![
                Color::Rgb(0, 0, 0),       Color::Rgb(24, 33, 41),
                Color::Rgb(239, 49, 16),    Color::Rgb(206, 41, 16),
                Color::Rgb(214, 90, 0),     Color::Rgb(214, 102, 0),
                Color::Rgb(214, 115, 0),    Color::Rgb(198, 123, 8),
                Color::Rgb(222, 165, 24),   Color::Rgb(214, 181, 33),
                Color::Rgb(189, 222, 41),   Color::Rgb(148, 222, 33),
                Color::Rgb(41, 206, 16),    Color::Rgb(50, 190, 16),
                Color::Rgb(57, 181, 16),    Color::Rgb(49, 156, 8),
                Color::Rgb(41, 148, 0),     Color::Rgb(24, 132, 8),
            ],
            btn_normal: Color::Rgb(34, 33, 51),
            btn_text: Color::White,
            seek_track: Color::Rgb(16, 15, 24),
            seek_filled: Color::Rgb(22, 21, 33),
            play_indicator: Color::Rgb(0, 232, 0),
            pause_indicator: Color::Rgb(255, 40, 51),
            indicator_on: Color::Rgb(0, 255, 0),
            indicator_off: Color::Rgb(52, 52, 82),
        }
    }
}

// ─── Public entry point ────────────────────────────────────────────────

pub fn render(frame: &mut Frame, app: &mut App) {
    if app.skin_layout.is_some() {
        render_bitmap_mode(frame, app);
    } else {
        render_text_mode(frame, app);
    }
}

// ─── Bitmap mode (Pass 1: MAIN.BMP background, Pass 2: text overlays) ──

fn render_bitmap_mode(frame: &mut Frame, app: &mut App) {
    let sc = SC::from_app(app);
    let area = frame.area();
    let skin = app.winamp_skin.as_ref().expect("winamp_skin must be Some when layout is Some");
    let main_bmp = skin.main_bitmap.as_ref().expect("main_bitmap required for bitmap mode");

    // Layout: 6-row main window, 1-row playlist title, flexible playlist body, 2-row footer
    let main_rows: u16 = 6;
    let footer_rows: u16 = 2;

    let vertical = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(main_rows),
            Constraint::Length(1),
            Constraint::Min(3),
            Constraint::Length(footer_rows),
        ])
        .split(area);

    let main_area = vertical[0];
    let pl_title_area = vertical[1];
    let pl_body_area = vertical[2];
    let footer_area = vertical[3];

    // Pass 1: Paint MAIN.BMP as full background for the main window
    skin_bitmap::render_scaled_bitmap(frame, main_area, main_bmp);

    // Pass 2: Overlay dynamic content on each row
    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1),
            Constraint::Length(1),
            Constraint::Length(1),
            Constraint::Length(1),
            Constraint::Length(1),
            Constraint::Length(1),
        ])
        .split(main_area);

    render_bitmap_titlebar(frame, rows[0], app, &sc);
    render_bitmap_time_vis(frame, rows[1], app, &sc);
    render_bitmap_marquee(frame, rows[2], app, &sc);
    render_bitmap_seekbar(frame, rows[3], app, &sc);
    render_bitmap_transport(frame, rows[4], app, &sc);
    render_bitmap_status(frame, rows[5], app, &sc);

    // Playlist and footer — styled text (same as text mode)
    render_playlist_titlebar(frame, pl_title_area, app, &sc);
    render_playlist_body(frame, pl_body_area, app, &sc);
    render_footer(frame, footer_area, app, &sc);

    // Store layout rects for mouse hit-testing
    let seek_rect = Rect::new(main_area.x, main_area.y + 3, main_area.width, 1);
    let transport_rect = Rect::new(main_area.x, main_area.y + 4, main_area.width, 1);
    let pause_button = Rect::new(transport_rect.x + 10, transport_rect.y, 4, 1);

    app.layout_rects = LayoutRects {
        results: pl_body_area,
        player_info: Rect::new(main_area.x, main_area.y, main_area.width, 1),
        player_bar: seek_rect,
        input: Rect::new(footer_area.x, footer_area.y, footer_area.width, 1),
        help: Rect::new(footer_area.x, footer_area.y + 1, footer_area.width, 1),
        pause_button,
        prev_page: Rect::new(
            pl_body_area.x + pl_body_area.width.saturating_sub(6),
            pl_body_area.y, 3, 1,
        ),
        next_page: Rect::new(
            pl_body_area.x + pl_body_area.width.saturating_sub(3),
            pl_body_area.y, 3, 1,
        ),
    };
}

// Row 0 — Title bar overlay: only paint skin name centered, leave bitmap chrome visible
fn render_bitmap_titlebar(frame: &mut Frame, area: Rect, app: &App, _sc: &SC) {
    let skin_name = app
        .winamp_skin
        .as_ref()
        .map(|s| s.name.as_str())
        .unwrap_or("WINAMP");

    // Center the skin name text on the row — no bg to let bitmap show through
    let title = format!(" {skin_name} ");
    let title_len = title.len() as u16;
    let x = area.x + area.width.saturating_sub(title_len) / 2;

    let buf = frame.buffer_mut();
    buf.set_string(x, area.y, title, Style::default()
        .fg(Color::White)
        .add_modifier(Modifier::BOLD));
}

// Row 1 — LED time + vis overlay: paint time and vis bars, leave bitmap visible
fn render_bitmap_time_vis(frame: &mut Frame, area: Rect, app: &App, sc: &SC) {
    let elapsed = match &app.playback {
        Some(pb) => pb.elapsed_secs,
        None => 0,
    };

    let is_playing = app.playback.is_some();
    let is_paused = app.playback.as_ref().is_some_and(|p| p.paused);

    let buf = frame.buffer_mut();
    let mut x = area.x;

    // State icon — no bg to keep bitmap visible
    let (state_str, state_fg) = if !is_playing {
        (" ■ ", sc.indicator_off)
    } else if is_paused {
        (" ‖ ", sc.pause_indicator)
    } else {
        (" ▶ ", sc.play_indicator)
    };
    buf.set_string(x, area.y, state_str, Style::default().fg(state_fg));
    x += 3;

    // LED time
    let time_str = format!(" {}:{:02} ", elapsed / 60, elapsed % 60);
    buf.set_string(x, area.y, &time_str, Style::default()
        .fg(sc.led_on)
        .add_modifier(Modifier::BOLD));
    x += time_str.len() as u16;

    // Visualization bars (only in the right portion of the row)
    let vis_start = area.width.saturating_sub(area.width * 2 / 3);
    x = area.x + vis_start;

    if is_playing && !is_paused {
        let bar_chars = ['▁', '▂', '▃', '▄', '▅', '▆', '▇', '█'];
        let bar_count = (area.width - vis_start) / 2;
        for i in 0..bar_count {
            let h = ((elapsed.wrapping_mul(7).wrapping_add(i as u64 * 13)) % 6) as usize + 1;
            let color = if sc.vis_colors.len() > 2 {
                let idx = (h * (sc.vis_colors.len() - 2) / 7).min(sc.vis_colors.len() - 1);
                sc.vis_colors.get(idx + 2).copied().unwrap_or(sc.led_off)
            } else {
                sc.led_off
            };
            let ch = bar_chars[h.min(bar_chars.len() - 1)];
            buf.set_string(x, area.y, &format!("{ch} "), Style::default().fg(color));
            x += 2;
        }
    }
}

// Row 2 — Marquee overlay: paint track title, leave bitmap visible
fn render_bitmap_marquee(frame: &mut Frame, area: Rect, app: &App, sc: &SC) {
    let title = match &app.playback {
        Some(pb) => pb.title.clone(),
        None => match &app.status {
            Status::Loading(t) | Status::Searching(t) | Status::Scanning(t) => t.clone(),
            Status::Error(t) => t.clone(),
            _ => "  ***  rustune  ***  ".into(),
        },
    };

    // Leave 1 cell padding on each side for bitmap chrome
    let inner_w = area.width.saturating_sub(2) as usize;
    let display = if title.len() > inner_w {
        let mut t: String = title.chars().take(inner_w.saturating_sub(1)).collect();
        t.push('…');
        t
    } else {
        title
    };

    let buf = frame.buffer_mut();
    buf.set_string(area.x + 1, area.y, &display, Style::default().fg(sc.text_fg));
}

// Row 3 — Seek bar overlay: paint progress gauge, leave bitmap visible
fn render_bitmap_seekbar(frame: &mut Frame, area: Rect, app: &App, sc: &SC) {
    let (elapsed, duration) = match &app.playback {
        Some(pb) => (pb.elapsed_secs, pb.duration_secs),
        None => (0, 0),
    };

    let ratio = if duration > 0 {
        elapsed as f64 / duration as f64
    } else {
        0.0
    };

    let elapsed_str = format_time(elapsed);
    let duration_str = format_time(duration);
    let label = format!("{elapsed_str}/{duration_str}");

    let gauge_width = area.width.saturating_sub(4);
    if gauge_width == 0 {
        return;
    }

    let filled = ((gauge_width as f64) * ratio) as u16;
    let unfilled = gauge_width.saturating_sub(filled);

    let buf = frame.buffer_mut();
    let gauge_x = area.x + 2;

    let label_x = gauge_x + gauge_width.saturating_sub(label.len() as u16) / 2;

    // Paint filled/unfilled — no bg to keep bitmap visible
    if filled > 0 {
        buf.set_string(gauge_x, area.y, &"█".repeat(filled as usize), Style::default().fg(sc.seek_filled));
    }
    if unfilled > 0 {
        buf.set_string(gauge_x + filled, area.y, &"░".repeat(unfilled as usize), Style::default().fg(sc.seek_track));
    }
    buf.set_string(label_x, area.y, &label, Style::default().fg(sc.led_on));
}

// Row 4 — Transport overlay: paint button states, leave bitmap visible
fn render_bitmap_transport(frame: &mut Frame, area: Rect, app: &App, sc: &SC) {
    let is_paused = app.playback.as_ref().is_some_and(|p| p.paused);
    let is_playing = app.playback.is_some();

    let buf = frame.buffer_mut();
    let mut x = area.x + 1;

    let btn = |label: &str, fg: Color, buf: &mut ratatui::buffer::Buffer, x: &mut u16| {
        buf.set_string(*x, area.y, label, Style::default().fg(fg));
        *x += label.len() as u16;
    };

    let btn_fg = sc.btn_text;
    let active_fg = Color::Black;

    let play_fg = if is_playing && !is_paused { sc.play_indicator } else { btn_fg };
    let pause_fg = if is_paused { sc.pause_indicator } else { btn_fg };

    btn(" ⏮ ", active_fg, buf, &mut x);
    btn(" ⏪ ", btn_fg, buf, &mut x);
    btn(" ▶ ", play_fg, buf, &mut x);
    btn(" ⏸ ", pause_fg, buf, &mut x);
    btn(" ⏹ ", btn_fg, buf, &mut x);
    btn(" ⏩ ", btn_fg, buf, &mut x);
    btn(" ⏭ ", active_fg, buf, &mut x);
}

// Row 5 — Status row overlay: paint indicators, leave bitmap visible
fn render_bitmap_status(frame: &mut Frame, area: Rect, app: &App, sc: &SC) {
    let is_playing = app.playback.is_some();

    let buf = frame.buffer_mut();
    let mut x = area.x + 1;

    let (mono_fg, _stereo_fg) = if is_playing {
        (sc.indicator_off, sc.indicator_on)
    } else {
        (sc.indicator_off, sc.indicator_off)
    };

    let source_label = match app.active_source {
        crate::media::SourceKind::Local => "LOCAL",
        crate::media::SourceKind::Extractor(_) => "ONLINE",
    };

    // No bg — keep bitmap visible
    buf.set_string(x, area.y, " SHUF ", Style::default().fg(sc.indicator_off));
    x += 6;
    buf.set_string(x, area.y, " REP ", Style::default().fg(sc.indicator_off));
    x += 5;

    let label = format!(" {source_label} ");
    buf.set_string(area.x + area.width.saturating_sub(label.len() as u16), area.y, &label,
        Style::default().fg(sc.titlebar_bg).add_modifier(Modifier::BOLD));

    buf.set_string(x + 3, area.y, "mono/stereo",
        Style::default().fg(mono_fg));
}

// ─── Text mode (original renderer, fallback when bitmaps unavailable) ───

fn render_text_mode(frame: &mut Frame, app: &mut App) {
    let sc = SC::from_app(app);
    let area = frame.area();

    // If the loaded WSZ contains MAIN.BMP pixels, use it as the actual background UI.
    if let Some(ref skin) = app.winamp_skin {
        if let Some(ref bmp) = skin.main_bitmap {
            skin_bitmap::render_scaled_bitmap(frame, area, bmp);
        }
    }

    // Main window chrome: 6 rows, playlist: fill, footer: 2 rows
    let main_rows = 6u16;
    let footer_rows = 2u16;

    let vertical = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(main_rows),  // main window panel
            Constraint::Length(1),          // separator (playlist title bar)
            Constraint::Min(3),            // playlist body
            Constraint::Length(footer_rows), // input + hints
        ])
        .split(area);

    let main_area = vertical[0];
    let pl_title_area = vertical[1];
    let pl_body_area = vertical[2];
    let footer_area = vertical[3];

    render_main_window(frame, main_area, app, &sc);
    render_playlist_titlebar(frame, pl_title_area, app, &sc);
    render_playlist_body(frame, pl_body_area, app, &sc);
    render_footer(frame, footer_area, app, &sc);

    // Store layout rects for mouse hit-testing
    let seek_rect = Rect::new(main_area.x, main_area.y + 3, main_area.width, 1);
    let transport_rect = Rect::new(main_area.x, main_area.y + 4, main_area.width, 1);
    let pause_button = Rect::new(transport_rect.x + 10, transport_rect.y, 4, 1);

    app.layout_rects = LayoutRects {
        results: pl_body_area,
        player_info: Rect::new(main_area.x, main_area.y, main_area.width, 1),
        player_bar: seek_rect,
        input: Rect::new(footer_area.x, footer_area.y, footer_area.width, 1),
        help: Rect::new(footer_area.x, footer_area.y + 1, footer_area.width, 1),
        pause_button,
        prev_page: Rect::new(
            pl_body_area.x + pl_body_area.width.saturating_sub(6),
            pl_body_area.y, 3, 1,
        ),
        next_page: Rect::new(
            pl_body_area.x + pl_body_area.width.saturating_sub(3),
            pl_body_area.y, 3, 1,
        ),
    };
}

// ─── Main window (6 rows) ─────────────────────────────────────────────

fn render_main_window(frame: &mut Frame, area: Rect, app: &App, sc: &SC) {
    // Fill background with body_bg from MAIN.BMP
    let bg = Block::default().style(Style::default().bg(sc.body_bg));
    frame.render_widget(bg, area);

    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1), // row 0: title bar
            Constraint::Length(1), // row 1: LED time + vis
            Constraint::Length(1), // row 2: track title marquee
            Constraint::Length(1), // row 3: seek bar
            Constraint::Length(1), // row 4: transport + vol + bal
            Constraint::Length(1), // row 5: shuf/rep + eq/pl + indicators
        ])
        .split(area);

    render_titlebar(frame, rows[0], app, sc);
    render_time_and_vis(frame, rows[1], app, sc);
    render_marquee(frame, rows[2], app, sc);
    render_seekbar(frame, rows[3], app, sc);
    render_transport(frame, rows[4], app, sc);
    render_status_row(frame, rows[5], app, sc);
}

// Row 0 — Title bar (TITLEBAR.BMP colors)
fn render_titlebar(frame: &mut Frame, area: Rect, app: &App, sc: &SC) {
    let skin_name = app
        .winamp_skin
        .as_ref()
        .map(|s| s.name.as_str())
        .unwrap_or("WINAMP");

    let w = area.width as usize;
    let title = format!(" {skin_name} ");
    let controls = " \u{2500}\u{25A1}\u{2715} ";
    let pad = w.saturating_sub(title.len() + controls.len());

    let line = Line::from(vec![
        Span::styled(
            title,
            Style::default()
                .fg(Color::White)
                .bg(sc.titlebar_bg)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(
            "\u{2500}".repeat(pad),
            Style::default().fg(sc.titlebar_bg).bg(sc.chrome_dark),
        ),
        Span::styled(
            controls.to_string(),
            Style::default().fg(sc.chrome_light).bg(sc.chrome_dark),
        ),
    ]);
    frame.render_widget(
        Paragraph::new(line).style(Style::default().bg(sc.chrome_dark)),
        area,
    );
}

// Row 1 — LED time display (NUMBERS.BMP) + visualization (VISCOLOR.TXT)
fn render_time_and_vis(frame: &mut Frame, area: Rect, app: &App, sc: &SC) {
    let elapsed = match &app.playback {
        Some(pb) => pb.elapsed_secs,
        None => 0,
    };

    let is_playing = app.playback.is_some();
    let is_paused = app.playback.as_ref().is_some_and(|p| p.paused);

    let state_icon = if !is_playing {
        Span::styled(" \u{25A0} ", Style::default().fg(sc.indicator_off).bg(sc.led_bg))
    } else if is_paused {
        Span::styled(" \u{2016} ", Style::default().fg(sc.pause_indicator).bg(sc.led_bg))
    } else {
        Span::styled(" \u{25B6} ", Style::default().fg(sc.play_indicator).bg(sc.led_bg))
    };

    // LED time from NUMBERS.BMP — format like "12:34"
    let time_str = format!(" {}:{:02} ", elapsed / 60, elapsed % 60);
    let led_time = Span::styled(
        time_str,
        Style::default()
            .fg(sc.led_on)
            .bg(sc.led_bg)
            .add_modifier(Modifier::BOLD),
    );

    // Visualization bars from VISCOLOR.TXT
    let vis_width = (area.width as usize).saturating_sub(14);
    let bar_count = vis_width / 2;
    let seed = elapsed;

    let mut vis_spans: Vec<Span> = Vec::with_capacity(bar_count + 1);
    vis_spans.push(Span::styled(" ", Style::default().bg(sc.body_bg)));

    if is_playing && !is_paused {
        let bar_chars = [
            '\u{2581}', '\u{2582}', '\u{2583}', '\u{2584}',
            '\u{2585}', '\u{2586}', '\u{2587}', '\u{2588}',
        ];
        for i in 0..bar_count {
            let h = ((seed.wrapping_mul(7).wrapping_add(i as u64 * 13)) % 6) as usize + 1;
            let color = if sc.vis_colors.len() > 2 {
                let idx = (h * (sc.vis_colors.len() - 2) / 7).min(sc.vis_colors.len() - 1);
                sc.vis_colors.get(idx + 2).copied().unwrap_or(sc.led_off)
            } else {
                sc.led_off
            };
            let ch = bar_chars[h.min(bar_chars.len() - 1)];
            vis_spans.push(Span::styled(
                format!("{ch} "),
                Style::default().fg(color).bg(sc.led_bg),
            ));
        }
    } else {
        // Idle visualizer — dim bars
        for _ in 0..bar_count {
            vis_spans.push(Span::styled(
                "\u{2581} ",
                Style::default().fg(sc.led_off).bg(sc.led_bg),
            ));
        }
    }

    let mut spans = vec![state_icon, led_time];
    spans.extend(vis_spans);

    frame.render_widget(
        Paragraph::new(Line::from(spans)).style(Style::default().bg(sc.body_bg)),
        area,
    );
}

// Row 2 — Scrolling track title marquee (TEXT.BMP colors)
fn render_marquee(frame: &mut Frame, area: Rect, app: &App, sc: &SC) {
    let title = match &app.playback {
        Some(pb) => pb.title.clone(),
        None => match &app.status {
            Status::Loading(t) | Status::Searching(t) | Status::Scanning(t) => t.clone(),
            Status::Error(t) => t.clone(),
            _ => "  ***  rustune  ***  ".into(),
        },
    };

    let w = area.width as usize;
    let display = if title.len() > w.saturating_sub(2) {
        let mut t: String = title.chars().take(w.saturating_sub(3)).collect();
        t.push('\u{2026}');
        t
    } else {
        title
    };

    let line = Line::from(vec![
        Span::styled(" ", Style::default().bg(sc.text_bg)),
        Span::styled(
            display,
            Style::default().fg(sc.text_fg).bg(sc.text_bg),
        ),
        Span::styled(
            " ".repeat(w.saturating_sub(1)),
            Style::default().bg(sc.text_bg),
        ),
    ]);
    frame.render_widget(Paragraph::new(line), area);
}

// Row 3 — Seek / position bar (POSBAR.BMP)
fn render_seekbar(frame: &mut Frame, area: Rect, app: &App, sc: &SC) {
    let (elapsed, duration) = match &app.playback {
        Some(pb) => (pb.elapsed_secs, pb.duration_secs),
        None => (0, 0),
    };

    let ratio = if duration > 0 {
        elapsed as f64 / duration as f64
    } else {
        0.0
    };

    let elapsed_str = format_time(elapsed);
    let duration_str = format_time(duration);
    let label = format!("{elapsed_str} / {duration_str}");

    let gauge = LineGauge::default()
        .ratio(ratio)
        .label(Span::styled(label, Style::default().fg(sc.led_on)))
        .filled_style(Style::default().fg(sc.seek_filled).bg(sc.seek_track))
        .unfilled_style(Style::default().fg(sc.seek_track).bg(sc.body_bg))
        .line_set(ratatui::symbols::line::THICK);

    frame.render_widget(gauge, area);
}

// Row 4 — Transport buttons (CBUTTONS.BMP) + Volume + Balance
fn render_transport(frame: &mut Frame, area: Rect, app: &App, sc: &SC) {
    let is_paused = app.playback.as_ref().is_some_and(|p| p.paused);
    let is_playing = app.playback.is_some();

    let btn_style = Style::default().fg(sc.btn_text).bg(sc.btn_normal);
    let btn_active = Style::default().fg(Color::Black).bg(sc.chrome_light);
    let sep = Span::styled(" ", Style::default().bg(sc.body_bg));

    let play_style = if is_playing && !is_paused {
        Style::default().fg(Color::Black).bg(sc.play_indicator)
    } else {
        btn_style
    };
    let pause_style = if is_paused {
        Style::default().fg(Color::Black).bg(sc.pause_indicator)
    } else {
        btn_style
    };

    // Volume bar
    let vol_filled = 7;
    let vol_empty = 3;
    let vol_bar = format!(
        "{}{}",
        "\u{2588}".repeat(vol_filled),
        "\u{2591}".repeat(vol_empty),
    );

    // Balance bar
    let bal_filled = 5;
    let bal_empty = 5;
    let bal_bar = format!(
        "{}{}",
        "\u{2588}".repeat(bal_filled),
        "\u{2591}".repeat(bal_empty),
    );

    let line = Line::from(vec![
        Span::styled(" \u{23EE} ", btn_active),      // prev
        sep.clone(),
        Span::styled(" \u{23EA} ", btn_style),        // rewind
        sep.clone(),
        Span::styled(" \u{25B6} ", play_style),       // play
        sep.clone(),
        Span::styled(" \u{23F8} ", pause_style),      // pause
        sep.clone(),
        Span::styled(" \u{23F9} ", btn_style),        // stop
        sep.clone(),
        Span::styled(" \u{23E9} ", btn_style),        // fwd
        sep.clone(),
        Span::styled(" \u{23ED} ", btn_active),       // next
        Span::styled("  ", Style::default().bg(sc.body_bg)),
        Span::styled("VOL", Style::default().fg(sc.chrome_light).bg(sc.body_bg)),
        Span::styled(
            vol_bar,
            Style::default().fg(sc.led_on).bg(sc.body_bg),
        ),
        Span::styled(" ", Style::default().bg(sc.body_bg)),
        Span::styled("BAL", Style::default().fg(sc.chrome_light).bg(sc.body_bg)),
        Span::styled(
            bal_bar,
            Style::default().fg(sc.led_on).bg(sc.body_bg),
        ),
    ]);
    frame.render_widget(
        Paragraph::new(line).style(Style::default().bg(sc.body_bg)),
        area,
    );
}

// Row 5 — Shuffle/Repeat (SHUFREP.BMP), EQ/PL toggles, Mono/Stereo (MONOSTER.BMP), bitrate/kHz
fn render_status_row(frame: &mut Frame, area: Rect, app: &App, sc: &SC) {
    let is_playing = app.playback.is_some();

    // Shuffle/Repeat from SHUFREP.BMP
    let shuf_style = Style::default().fg(sc.indicator_off).bg(sc.chrome_dark);
    let rep_style = Style::default().fg(sc.indicator_off).bg(sc.chrome_dark);

    // EQ/PL toggle buttons
    let eq_style = Style::default().fg(sc.indicator_off).bg(sc.chrome_dark);
    let pl_style = Style::default().fg(sc.indicator_on).bg(sc.chrome_dark);

    // Mono/Stereo indicator from MONOSTER.BMP
    let (mono_fg, stereo_fg) = if is_playing {
        (sc.indicator_off, sc.indicator_on)
    } else {
        (sc.indicator_off, sc.indicator_off)
    };

    let source_label = match app.active_source {
        crate::media::SourceKind::Local => "LOCAL",
        crate::media::SourceKind::Extractor(_) => "ONLINE",
    };

    let line = Line::from(vec![
        Span::styled(" SHUF ", shuf_style),
        Span::styled(" ", Style::default().bg(sc.body_bg)),
        Span::styled(" REP ", rep_style),
        Span::styled("  ", Style::default().bg(sc.body_bg)),
        Span::styled(" EQ ", eq_style),
        Span::styled(" ", Style::default().bg(sc.body_bg)),
        Span::styled(" PL ", pl_style),
        Span::styled("   ", Style::default().bg(sc.body_bg)),
        Span::styled("mono", Style::default().fg(mono_fg).bg(sc.body_bg)),
        Span::styled("/", Style::default().fg(sc.chrome_mid).bg(sc.body_bg)),
        Span::styled("stereo", Style::default().fg(stereo_fg).bg(sc.body_bg)),
        Span::styled("   ", Style::default().bg(sc.body_bg)),
        Span::styled(
            format!(" {source_label} "),
            Style::default()
                .fg(sc.titlebar_bg)
                .bg(sc.chrome_dark)
                .add_modifier(Modifier::BOLD),
        ),
    ]);
    frame.render_widget(
        Paragraph::new(line).style(Style::default().bg(sc.body_bg)),
        area,
    );
}

// ─── Playlist Editor ───────────────────────────────────────────────────

// Playlist title bar — mirrors the real Winamp "WINAMP PLAYLIST EDITOR" bar
fn render_playlist_titlebar(frame: &mut Frame, area: Rect, _app: &App, sc: &SC) {
    let w = area.width as usize;
    let title = " PLAYLIST EDITOR ";
    let pad_total = w.saturating_sub(title.len());
    let pad_l = pad_total / 2;
    let pad_r = pad_total - pad_l;

    let line = Line::from(vec![
        Span::styled(
            "\u{2500}".repeat(pad_l),
            Style::default().fg(sc.plist_normal).bg(sc.plist_normal_bg),
        ),
        Span::styled(
            title.to_string(),
            Style::default()
                .fg(sc.plist_current)
                .bg(sc.plist_normal_bg)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(
            "\u{2500}".repeat(pad_r),
            Style::default().fg(sc.plist_normal).bg(sc.plist_normal_bg),
        ),
    ]);
    frame.render_widget(Paragraph::new(line), area);
}

// Playlist body — uses PLEDIT.TXT colors
fn render_playlist_body(frame: &mut Frame, area: Rect, app: &mut App, sc: &SC) {
    if app.results.is_empty() {
        let msg = match &app.status {
            Status::Searching(t) | Status::Scanning(t) => t.clone(),
            Status::Error(t) => t.clone(),
            _ => "No tracks loaded — press / to search".into(),
        };

        let block = Block::default()
            .style(Style::default().bg(sc.plist_normal_bg))
            .padding(Padding::horizontal(1));
        let inner = block.inner(area);
        frame.render_widget(block, area);

        frame.render_widget(
            Paragraph::new(Line::from(Span::styled(
                msg,
                Style::default().fg(sc.plist_normal),
            ))),
            inner,
        );
        return;
    }

    let playing_idx = app.playback.as_ref().and_then(|pb| {
        app.results.iter().position(|r| r.title == pb.title)
    });

    let items: Vec<ListItem> = app
        .results
        .iter()
        .enumerate()
        .map(|(i, result)| {
            let duration = result
                .duration
                .map(format_time)
                .unwrap_or_else(|| "--:--".into());

            let subtitle = result
                .subtitle
                .as_deref()
                .map(|c| format!(" - {c}"))
                .unwrap_or_default();

            let is_current = playing_idx == Some(i);

            let num_style = if is_current {
                Style::default().fg(sc.plist_current)
            } else {
                Style::default().fg(sc.plist_normal)
            };
            let title_style = if is_current {
                Style::default()
                    .fg(sc.plist_current)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(sc.plist_normal)
            };

            let line = Line::from(vec![
                Span::styled(format!("{:>3}. ", i + 1), num_style),
                Span::styled(
                    format!("{}{subtitle}", result.title),
                    title_style,
                ),
                Span::styled(
                    format!("  {duration}"),
                    Style::default().fg(sc.indicator_off),
                ),
            ]);
            ListItem::new(line)
        })
        .collect();

    let block = Block::default().style(Style::default().bg(sc.plist_normal_bg));

    let list = List::new(items)
        .block(block)
        .highlight_style(
            Style::default()
                .fg(sc.plist_current)
                .bg(sc.plist_selected_bg)
                .add_modifier(Modifier::BOLD),
        )
        .highlight_symbol("\u{25B6} ")
        .highlight_spacing(HighlightSpacing::Always);

    frame.render_stateful_widget(list, area, &mut app.list_state);
}

// ─── Footer (input + hints) ───────────────────────────────────────────

fn render_footer(frame: &mut Frame, area: Rect, app: &App, sc: &SC) {
    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(1), Constraint::Length(1)])
        .split(area);

    // Input bar
    let prompt = match app.mode {
        Mode::Browse => " > ",
        Mode::Input => " / ",
        _ => " ? ",
    };
    let mut input_spans = vec![
        Span::styled(
            prompt,
            Style::default()
                .fg(sc.text_fg)
                .bg(sc.text_bg)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(
            &app.input_text,
            Style::default().fg(sc.text_fg).bg(sc.text_bg),
        ),
    ];
    if app.mode == Mode::Input {
        input_spans.push(Span::styled(
            "\u{2588}",
            Style::default().fg(sc.text_fg).bg(sc.text_bg),
        ));
    }
    // Fill rest of line with text_bg
    frame.render_widget(
        Paragraph::new(Line::from(input_spans)).style(Style::default().bg(sc.text_bg)),
        rows[0],
    );

    // Key hints bar
    let hints = match app.mode {
        Mode::Browse => vec![
            Span::styled(" /", Style::default().fg(sc.led_on).bg(sc.chrome_dark)),
            Span::styled(" search ", Style::default().fg(sc.chrome_mid).bg(sc.chrome_dark)),
            Span::styled("j/k", Style::default().fg(sc.led_on).bg(sc.chrome_dark)),
            Span::styled(" nav ", Style::default().fg(sc.chrome_mid).bg(sc.chrome_dark)),
            Span::styled("Enter", Style::default().fg(sc.led_on).bg(sc.chrome_dark)),
            Span::styled(" play ", Style::default().fg(sc.chrome_mid).bg(sc.chrome_dark)),
            Span::styled("Space", Style::default().fg(sc.led_on).bg(sc.chrome_dark)),
            Span::styled(" pause ", Style::default().fg(sc.chrome_mid).bg(sc.chrome_dark)),
            Span::styled("Tab", Style::default().fg(sc.led_on).bg(sc.chrome_dark)),
            Span::styled(" src ", Style::default().fg(sc.chrome_mid).bg(sc.chrome_dark)),
            Span::styled("S", Style::default().fg(sc.led_on).bg(sc.chrome_dark)),
            Span::styled(" settings ", Style::default().fg(sc.chrome_mid).bg(sc.chrome_dark)),
            Span::styled("q", Style::default().fg(sc.led_on).bg(sc.chrome_dark)),
            Span::styled(" quit", Style::default().fg(sc.chrome_mid).bg(sc.chrome_dark)),
        ],
        Mode::Input => vec![
            Span::styled(" Enter", Style::default().fg(sc.led_on).bg(sc.chrome_dark)),
            Span::styled(" submit ", Style::default().fg(sc.chrome_mid).bg(sc.chrome_dark)),
            Span::styled("Esc", Style::default().fg(sc.led_on).bg(sc.chrome_dark)),
            Span::styled(" cancel", Style::default().fg(sc.chrome_mid).bg(sc.chrome_dark)),
        ],
        _ => vec![],
    };
    frame.render_widget(
        Paragraph::new(Line::from(hints)).style(Style::default().bg(sc.chrome_dark)),
        rows[1],
    );
}

// ─── Helpers ───────────────────────────────────────────────────────────

fn format_time(secs: u64) -> String {
    let m = secs / 60;
    let s = secs % 60;
    format!("{m}:{s:02}")
}
