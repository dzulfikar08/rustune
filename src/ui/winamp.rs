/// Winamp-style renderer — completely different layout from the standard theme.
///
/// When `app.winamp_skin` is set (loaded from a .wsz file), all colors come from
/// the skin's BMP palettes and TXT configs. Otherwise falls back to built-in
/// default Winamp 2.x colors.
///
/// Layout (top to bottom):
///   Line 0: Header bar  — "WINAMP" logo + track title + LED time display
///   Line 1: Seek bar    — progress gauge + time stamps
///   Line 2: Transport   — [⏮][⏪][▶ ][⏩][⏭][■] + volume bar
///   Line 3: Viz bars    — fake spectrum analyzer bars (uses VISCOLOR.TXT)
///   Line 4: Border top  — ─────────────────────
///   N lines: Playlist   — track list (uses PLEDIT.TXT colors)
///   Last-2: Input bar   — prompt + text
///   Last-1: Status bar  — key hints

use ratatui::{
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, HighlightSpacing, List, ListItem, LineGauge, Padding, Paragraph},
    Frame,
};

use crate::app::{App, LayoutRects, Mode, Status};
use crate::skin::WinampSkin;

// Resolved skin colors — either from loaded .wsz or defaults
#[allow(dead_code)]
struct SkinColors {
    led_on: Color,
    led_off: Color,
    led_bg: Color,
    chrome: Color,
    chrome_light: Color,
    body_bg: Color,
    titlebar_bg: Color,
    text_fg: Color,
    text_bg: Color,
    plist_normal: Color,
    plist_current: Color,
    plist_normal_bg: Color,
    plist_selected_bg: Color,
    vis_colors: Vec<Color>,
    btn_normal: Color,
    btn_pressed: Color,
    btn_text: Color,
    seek_track: Color,
    seek_thumb: Color,
    seek_filled: Color,
    play_indicator: Color,
    pause_indicator: Color,
}

impl SkinColors {
    fn from_app(app: &App) -> Self {
        match &app.winamp_skin {
            Some(skin) => Self::from_skin(skin),
            None => Self::default_colors(),
        }
    }

    fn from_skin(s: &WinampSkin) -> Self {
        Self {
            led_on: s.led_on,
            led_off: s.led_off,
            led_bg: Color::Rgb(0, 20, 0),
            chrome: s.chrome_mid,
            chrome_light: s.chrome_light,
            body_bg: s.body_bg,
            titlebar_bg: s.titlebar_bg,
            text_fg: s.text_fg,
            text_bg: s.text_bg,
            plist_normal: s.plist_normal,
            plist_current: s.plist_current,
            plist_normal_bg: s.plist_normal_bg,
            plist_selected_bg: s.plist_selected_bg,
            vis_colors: s.vis_colors.clone(),
            btn_normal: s.btn_normal,
            btn_pressed: s.btn_pressed,
            btn_text: s.btn_text,
            seek_track: s.seek_track,
            seek_thumb: s.seek_thumb,
            seek_filled: s.seek_filled,
            play_indicator: s.play_indicator,
            pause_indicator: s.pause_indicator,
        }
    }

    fn default_colors() -> Self {
        Self {
            led_on: Color::Rgb(0, 255, 0),
            led_off: Color::Rgb(0, 80, 0),
            led_bg: Color::Rgb(0, 20, 0),
            chrome: Color::Rgb(80, 80, 80),
            chrome_light: Color::Rgb(120, 120, 120),
            body_bg: Color::Rgb(57, 57, 90),
            titlebar_bg: Color::Rgb(0, 198, 255),
            text_fg: Color::Rgb(0, 226, 0),
            text_bg: Color::Rgb(0, 0, 165),
            plist_normal: Color::Rgb(0, 255, 0),
            plist_current: Color::White,
            plist_normal_bg: Color::Black,
            plist_selected_bg: Color::Rgb(0, 0, 198),
            vis_colors: vec![
                Color::Rgb(0, 0, 0), Color::Rgb(24, 33, 41),
                Color::Rgb(239, 49, 16), Color::Rgb(206, 41, 16),
                Color::Rgb(214, 90, 0), Color::Rgb(214, 102, 0),
                Color::Rgb(214, 115, 0), Color::Rgb(198, 123, 8),
                Color::Rgb(222, 165, 24), Color::Rgb(214, 181, 33),
                Color::Rgb(189, 222, 41), Color::Rgb(148, 222, 33),
                Color::Rgb(41, 206, 16), Color::Rgb(50, 190, 16),
                Color::Rgb(57, 181, 16), Color::Rgb(49, 156, 8),
                Color::Rgb(41, 148, 0), Color::Rgb(24, 132, 8),
            ],
            btn_normal: Color::Rgb(34, 33, 51),
            btn_pressed: Color::Rgb(48, 47, 76),
            btn_text: Color::White,
            seek_track: Color::Rgb(16, 15, 24),
            seek_thumb: Color::Rgb(20, 19, 31),
            seek_filled: Color::Rgb(22, 21, 33),
            play_indicator: Color::Rgb(0, 232, 0),
            pause_indicator: Color::Rgb(255, 40, 51),
        }
    }
}

pub fn render(frame: &mut Frame, app: &mut App) {
    let sc = SkinColors::from_app(app);
    let area = frame.area();

    let player_lines = 5;
    let footer_lines = 2;

    let vertical = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(player_lines),
            Constraint::Min(3),
            Constraint::Length(footer_lines),
        ])
        .split(area);

    render_header(frame, vertical[0], app, &sc);
    render_playlist(frame, vertical[1], app, &sc);
    render_footer(frame, vertical[2], app, &sc);

    // Store layout rects for mouse hit-testing
    let header_y = vertical[0].y;
    let seek_rect = Rect::new(vertical[0].x, header_y + 1, vertical[0].width, 1);
    let transport_rect = Rect::new(vertical[0].x, header_y + 2, vertical[0].width, 1);
    let playlist_area = vertical[1];

    let pause_button = Rect::new(transport_rect.x + 10, transport_rect.y, 4, 1);
    let prev_page = Rect::new(
        playlist_area.x + playlist_area.width.saturating_sub(6),
        playlist_area.y, 3, 1,
    );
    let next_page = Rect::new(
        playlist_area.x + playlist_area.width.saturating_sub(3),
        playlist_area.y, 3, 1,
    );

    app.layout_rects = LayoutRects {
        results: playlist_area,
        player_info: Rect::new(vertical[0].x, header_y, vertical[0].width, 1),
        player_bar: seek_rect,
        input: Rect::new(vertical[2].x, vertical[2].y, vertical[2].width, 1),
        help: Rect::new(vertical[2].x, vertical[2].y + 1, vertical[2].width, 1),
        pause_button,
        prev_page,
        next_page,
    };
}

fn render_header(frame: &mut Frame, area: Rect, app: &App, sc: &SkinColors) {
    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1),
            Constraint::Length(1),
            Constraint::Length(1),
            Constraint::Length(1),
            Constraint::Length(1),
        ])
        .split(area);

    render_title_row(frame, rows[0], app, sc);
    render_seek_row(frame, rows[1], app, sc);
    render_transport_row(frame, rows[2], app, sc);
    render_viz_row(frame, rows[3], app, sc);

    let sep = Line::from(Span::styled(
        "\u{2500}".repeat(area.width as usize),
        Style::default().fg(sc.chrome_light),
    ));
    frame.render_widget(Paragraph::new(sep), rows[4]);
}

fn render_title_row(frame: &mut Frame, area: Rect, app: &App, sc: &SkinColors) {
    let (title_text, elapsed_str, duration_str) = match &app.playback {
        Some(pb) => (
            pb.title.clone(),
            format_duration_led(pb.elapsed_secs),
            format_duration_led(pb.duration_secs),
        ),
        None => match &app.status {
            Status::Loading(t) => (t.clone(), "  0:00".into(), "  0:00".into()),
            Status::Searching(t) | Status::Scanning(t) => (t.clone(), "  0:00".into(), "  0:00".into()),
            Status::Error(t) => (t.clone(), "  --:--".into(), "  --:--".into()),
            _ => ("rustune".into(), "  0:00".into(), "  0:00".into()),
        },
    };

    let max_title = (area.width as usize).saturating_sub(28);
    let display_title = truncate_str(&title_text, max_title);
    let led_time = format!("{elapsed_str}/{duration_str}");

    let skin_name = app
        .winamp_skin
        .as_ref()
        .map(|s| s.name.as_str())
        .unwrap_or("WINAMP");

    let line = Line::from(vec![
        Span::styled(
            format!(" {skin_name} "),
            Style::default().fg(Color::Black).bg(sc.led_on).add_modifier(Modifier::BOLD),
        ),
        Span::styled(" ", Style::default()),
        Span::styled(display_title, Style::default().fg(sc.led_on)),
        Span::styled(" ".repeat(area.width as usize / 2), Style::default()),
        Span::styled(
            format!(" {led_time} "),
            Style::default().fg(sc.led_on).bg(sc.led_bg).add_modifier(Modifier::BOLD),
        ),
    ]);
    frame.render_widget(Paragraph::new(line), area);
}

fn render_seek_row(frame: &mut Frame, area: Rect, app: &App, sc: &SkinColors) {
    let (elapsed_secs, duration_secs) = match &app.playback {
        Some(pb) => (pb.elapsed_secs, pb.duration_secs),
        None => (0, 0),
    };

    let ratio = if duration_secs > 0 {
        elapsed_secs as f64 / duration_secs as f64
    } else {
        0.0
    };

    let elapsed_str = format_duration_compact(elapsed_secs);
    let duration_str = format_duration_compact(duration_secs);
    let label = format!("{elapsed_str}  {duration_str}");

    let gauge = LineGauge::default()
        .ratio(ratio)
        .label(Span::styled(label, Style::default().fg(sc.led_on)))
        .filled_style(Style::default().fg(sc.led_on).bg(sc.led_off))
        .unfilled_style(Style::default().fg(sc.led_off))
        .line_set(ratatui::symbols::line::THICK);

    frame.render_widget(gauge, area);
}

fn render_transport_row(frame: &mut Frame, area: Rect, app: &App, sc: &SkinColors) {
    let is_paused = app.playback.as_ref().map_or(false, |p| p.paused);
    let is_playing = app.playback.is_some();

    let play_label = if !is_playing || is_paused {
        " \u{25B6} "
    } else {
        " \u{23F8} "
    };
    let play_bg = if !is_playing {
        sc.led_off
    } else if is_paused {
        sc.led_on
    } else {
        Color::Rgb(200, 255, 0)
    };

    let btn = |text: &str, active: bool| -> Span<'_> {
        if active {
            Span::styled(text.to_string(), Style::default().fg(Color::Black).bg(sc.chrome_light))
        } else {
            Span::styled(text.to_string(), Style::default().fg(sc.chrome).bg(Color::Rgb(30, 30, 30)))
        }
    };

    let vol_pct = 0.8;
    let vol_width = 12;
    let vol_filled = (vol_width as f64 * vol_pct) as usize;
    let vol_empty = vol_width - vol_filled;
    let vol_bar = format!(
        "VOL {}{}",
        "\u{2588}".repeat(vol_filled),
        "\u{2591}".repeat(vol_empty),
    );

    let line = Line::from(vec![
        Span::styled(" ", Style::default()),
        btn(" \u{23EE} ", true),
        btn(" \u{23EA} ", true),
        Span::styled(play_label.to_string(), Style::default().fg(Color::Black).bg(play_bg)),
        btn(" \u{23E9} ", true),
        btn(" \u{23ED} ", true),
        btn(" \u{23F9} ", true),
        Span::styled("  ", Style::default()),
        Span::styled(" SHUF ", Style::default().fg(sc.led_off).bg(sc.led_bg)),
        Span::styled(" REP ", Style::default().fg(sc.led_off).bg(sc.led_bg)),
        Span::styled("  ", Style::default()),
        Span::styled(vol_bar, Style::default().fg(sc.led_on)),
    ]);
    frame.render_widget(Paragraph::new(line), area);
}

fn render_viz_row(frame: &mut Frame, area: Rect, app: &App, sc: &SkinColors) {
    let seed = app.playback.as_ref().map(|p| p.elapsed_secs).unwrap_or(0);
    let bar_count = (area.width as usize) / 3;

    // Use skin vis colors if available
    let viz = |i: usize, h: usize| -> Span<'_> {
        let color = if sc.vis_colors.len() > 2 {
            // Map bar height to vis color gradient
            let idx = (h * (sc.vis_colors.len() - 2) / 7).min(sc.vis_colors.len() - 1);
            sc.vis_colors.get(idx + 2).copied().unwrap_or(sc.led_off)
        } else {
            sc.led_off
        };
        let chars = ['\u{2581}', '\u{2582}', '\u{2583}', '\u{2584}', '\u{2585}', '\u{2586}', '\u{2587}', '\u{2588}'];
        let ch = chars[h.min(chars.len() - 1)];
        let _ = i; // used in parent closure
        Span::styled(format!("{ch}{ch} "), Style::default().fg(color).bg(sc.led_bg))
    };

    let spans: Vec<Span> = (0..bar_count)
        .map(|i| {
            let h = ((seed * 7 + i as u64 * 13) % 5) as usize + 1;
            viz(i, h)
        })
        .collect();

    frame.render_widget(Paragraph::new(Line::from(spans)), area);
}

fn render_playlist(frame: &mut Frame, area: Rect, app: &mut App, sc: &SkinColors) {
    if app.results.is_empty() {
        let msg = match &app.status {
            Status::Searching(t) | Status::Scanning(t) => t.clone(),
            Status::Error(t) => t.clone(),
            _ => "No tracks loaded.".into(),
        };

        let block = Block::default()
            .style(Style::default().bg(sc.plist_normal_bg))
            .padding(Padding::horizontal(1));
        let inner = block.inner(area);
        frame.render_widget(block, area);

        let line = Line::from(Span::styled(
            format!("  {msg}"),
            Style::default().fg(sc.led_off),
        ));
        frame.render_widget(Paragraph::new(line), inner);
        return;
    }

    let items: Vec<ListItem> = app
        .results
        .iter()
        .enumerate()
        .map(|(i, result)| {
            let duration = result
                .duration
                .map(|d| format_duration_compact(d))
                .unwrap_or_else(|| " --:-- ".into());

            let subtitle = result
                .subtitle
                .as_deref()
                .map(|c| format!(" - {c}"))
                .unwrap_or_default();

            let line = Line::from(vec![
                Span::styled(format!("{:>3}. ", i + 1), Style::default().fg(sc.plist_normal)),
                Span::styled(
                    format!("{}{subtitle}", result.title),
                    Style::default().fg(sc.plist_normal),
                ),
                Span::styled(format!("  {duration}"), Style::default().fg(sc.led_off)),
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

fn render_footer(frame: &mut Frame, area: Rect, app: &App, sc: &SkinColors) {
    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(1), Constraint::Length(1)])
        .split(area);

    let prompt = match app.mode {
        Mode::Browse => " > ",
        Mode::Input => " / ",
        _ => " ? ",
    };
    let mut input_spans = vec![
        Span::styled(prompt, Style::default().fg(sc.led_on).add_modifier(Modifier::BOLD)),
        Span::styled(&app.input_text, Style::default().fg(sc.led_on)),
    ];
    if app.mode == Mode::Input {
        input_spans.push(Span::styled("\u{2588}", Style::default().fg(sc.led_on)));
    }
    frame.render_widget(
        Paragraph::new(Line::from(input_spans)).style(Style::default().bg(sc.led_bg)),
        rows[0],
    );

    let hints = match app.mode {
        Mode::Browse => vec![
            Span::styled(" /", Style::default().fg(sc.led_on)),
            Span::styled(" search ", Style::default().fg(sc.led_off)),
            Span::styled("j/k", Style::default().fg(sc.led_on)),
            Span::styled(" nav ", Style::default().fg(sc.led_off)),
            Span::styled("Enter", Style::default().fg(sc.led_on)),
            Span::styled(" play ", Style::default().fg(sc.led_off)),
            Span::styled("Tab", Style::default().fg(sc.led_on)),
            Span::styled(" source ", Style::default().fg(sc.led_off)),
            Span::styled("S", Style::default().fg(sc.led_on)),
            Span::styled(" settings ", Style::default().fg(sc.led_off)),
            Span::styled("q", Style::default().fg(sc.led_on)),
            Span::styled(" quit", Style::default().fg(sc.led_off)),
        ],
        Mode::Input => vec![
            Span::styled(" Enter", Style::default().fg(sc.led_on)),
            Span::styled(" submit ", Style::default().fg(sc.led_off)),
            Span::styled("Esc", Style::default().fg(sc.led_on)),
            Span::styled(" cancel", Style::default().fg(sc.led_off)),
        ],
        _ => vec![],
    };
    frame.render_widget(
        Paragraph::new(Line::from(hints)).style(Style::default().bg(sc.led_bg)),
        rows[1],
    );
}

// Helpers

fn format_duration_led(secs: u64) -> String {
    let m = secs / 60;
    let s = secs % 60;
    format!("{m:>3}:{s:02}")
}

fn format_duration_compact(secs: u64) -> String {
    let m = secs / 60;
    let s = secs % 60;
    format!("{m}:{s:02}")
}

fn truncate_str(s: &str, max: usize) -> String {
    if s.len() <= max {
        s.to_string()
    } else {
        let mut t: String = s.chars().take(max.saturating_sub(1)).collect();
        t.push('\u{2026}');
        t
    }
}
