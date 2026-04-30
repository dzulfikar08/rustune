/// Winamp-styled skin browser — mirrors the Winamp 2.x layout from winamp.rs.
///
/// Uses ALL colors from the loaded .wsz skin's BMP palettes:
///   MAIN.BMP     → chrome_dark, chrome_mid, chrome_light, body_bg, titlebar_bg
///   NUMBERS.BMP  → led_on, led_off
///   TEXT.BMP     → text_fg, text_bg
///   PLEDIT.TXT   → plist_normal, plist_current, plist_normal_bg, plist_selected_bg
///   VISCOLOR.TXT → vis_colors (24 spectrum colors)
///   CBUTTONS.BMP → btn_normal, btn_pressed, btn_text
///   POSBAR.BMP   → seek_track, seek_filled
///   MONOSTER.BMP → indicator_on, indicator_off
///
/// Layout (mirrors the Winamp main window + playlist):
///   ┌──── MAIN WINDOW ────┐
///   │ Title bar            │  row 0
///   │ LED count + vis bars │  row 1
///   │ search query marquee │  row 2
///   │ scroll position bar  │  row 3
///   │ action buttons       │  row 4
///   │ mode indicators      │  row 5
///   ├── SKIN LIST ─────────┤
///   │ skin entries...      │
///   ├──────────────────────┤
///   │ search input         │  footer 0
///   │ key hints            │  footer 1
///   └──────────────────────┘
use ratatui::{
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, HighlightSpacing, LineGauge, List, ListItem, Padding, Paragraph},
    Frame,
};

use crate::app::{App, SkinBrowserSource};
use crate::skin::WinampSkin;

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
    indicator_on: Color,
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
            indicator_on: s.indicator_on,
        }
    }

    fn defaults() -> Self {
        Self {
            chrome_dark: Color::Rgb(8, 8, 16),
            chrome_mid: Color::Rgb(123, 140, 156),
            chrome_light: Color::Rgb(189, 206, 214),
            body_bg: Color::Rgb(57, 57, 90),
            titlebar_bg: Color::Rgb(0, 198, 255),
            led_on: Color::Rgb(0, 255, 0),
            led_off: Color::Rgb(0, 80, 0),
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
            indicator_on: Color::Rgb(0, 255, 0),
        }
    }
}

pub fn render(frame: &mut Frame, area: Rect, app: &App) {
    let sc = SC::from_app(app);

    let main_rows = 6u16;
    let footer_rows = 2u16;

    let vertical = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(main_rows),
            Constraint::Length(1),
            Constraint::Min(3),
            Constraint::Length(footer_rows),
        ])
        .split(area);

    render_main_panel(frame, vertical[0], app, &sc);
    render_list_titlebar(frame, vertical[1], app, &sc);
    render_skinlist(frame, vertical[2], app, &sc);
    render_footer(frame, vertical[3], app, &sc);
}

// ─── Main panel (6 rows) ──────────────────────────────────────────────

fn render_main_panel(frame: &mut Frame, area: Rect, app: &App, sc: &SC) {
    let bg = Block::default().style(Style::default().bg(sc.body_bg));
    frame.render_widget(bg, area);

    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1), // title bar
            Constraint::Length(1), // LED count + vis bars
            Constraint::Length(1), // search query marquee
            Constraint::Length(1), // scroll position gauge
            Constraint::Length(1), // action buttons
            Constraint::Length(1), // mode / status indicators
        ])
        .split(area);

    render_titlebar(frame, rows[0], app, sc);
    render_count_and_vis(frame, rows[1], app, sc);
    render_search_marquee(frame, rows[2], app, sc);
    render_scroll_gauge(frame, rows[3], app, sc);
    render_action_buttons(frame, rows[4], app, sc);
    render_status_row(frame, rows[5], app, sc);
}

fn render_titlebar(frame: &mut Frame, area: Rect, app: &App, sc: &SC) {
    let skin_name = app
        .winamp_skin
        .as_ref()
        .map(|s| s.name.as_str())
        .unwrap_or("WINAMP");

    let w = area.width as usize;
    let title = format!(" {skin_name} \u{2014} SKIN BROWSER ");
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

fn render_count_and_vis(frame: &mut Frame, area: Rect, app: &App, sc: &SC) {
    let count_str = if app.skin_browser_loading {
        " loading... ".to_string()
    } else if app.skin_entries.is_empty() {
        "  0/0  ".to_string()
    } else {
        let shown = app.skin_entries.len();
        let total = app.skin_total_count;
        format!(" {shown:>4}/{total} ")
    };

    let led_count = Span::styled(
        count_str,
        Style::default()
            .fg(sc.led_on)
            .bg(sc.led_bg)
            .add_modifier(Modifier::BOLD),
    );

    // Decorative vis bars
    let vis_width = (area.width as usize).saturating_sub(14);
    let bar_count = vis_width / 2;
    let bar_chars = [
        '\u{2581}', '\u{2582}', '\u{2583}', '\u{2584}',
        '\u{2585}', '\u{2586}', '\u{2587}', '\u{2588}',
    ];

    let mut vis_spans: Vec<Span> = Vec::with_capacity(bar_count + 1);
    vis_spans.push(Span::styled("  ", Style::default().bg(sc.body_bg)));
    for i in 0..bar_count {
        let h = ((i * 13 + 7) % 5) + 1;
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

    let mut spans = vec![led_count];
    spans.extend(vis_spans);
    frame.render_widget(
        Paragraph::new(Line::from(spans)).style(Style::default().bg(sc.body_bg)),
        area,
    );
}

fn render_search_marquee(frame: &mut Frame, area: Rect, app: &App, sc: &SC) {
    let source = match app.skin_browser_source {
        SkinBrowserSource::Local => "LOCAL",
        SkinBrowserSource::Online => "ONLINE",
    };

    let text = if app.skin_search_query.is_empty() {
        format!("  ***  {source} SKINS  ***  ")
    } else {
        format!("  {source} SKINS — /{}/  ", app.skin_search_query)
    };

    let w = area.width as usize;
    let display = if text.len() > w.saturating_sub(2) {
        let mut t: String = text.chars().take(w.saturating_sub(3)).collect();
        t.push('\u{2026}');
        t
    } else {
        text
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

fn render_scroll_gauge(frame: &mut Frame, area: Rect, app: &App, sc: &SC) {
    let total = app.skin_entries.len();
    let selected = app.skin_list_state.selected().unwrap_or(0);

    let ratio = if total > 1 {
        selected as f64 / (total - 1) as f64
    } else {
        0.0
    };

    let label = format!("{selected:>4}/{total}");

    let gauge = LineGauge::default()
        .ratio(ratio)
        .label(Span::styled(label, Style::default().fg(sc.led_on)))
        .filled_style(Style::default().fg(sc.seek_filled).bg(sc.seek_track))
        .unfilled_style(Style::default().fg(sc.seek_track).bg(sc.body_bg))
        .line_set(ratatui::symbols::line::THICK);

    frame.render_widget(gauge, area);
}

fn render_action_buttons(frame: &mut Frame, area: Rect, app: &App, sc: &SC) {
    let btn_style = Style::default().fg(sc.btn_text).bg(sc.btn_normal);
    let btn_active = Style::default().fg(Color::Black).bg(sc.chrome_light);
    let sep = Span::styled(" ", Style::default().bg(sc.body_bg));

    let search_active = app.skin_search_active;
    let search_style = if search_active { btn_active } else { btn_style };

    let line = Line::from(vec![
        Span::styled("  / ", search_style),           // search
        sep.clone(),
        Span::styled(" j/k ", btn_style),             // navigate
        sep.clone(),
        Span::styled(" Enter ", btn_active),           // apply/download
        sep.clone(),
        Span::styled(" n ", btn_style),                // load more
        sep.clone(),
        Span::styled(" Tab ", btn_style),              // switch local/online
        sep.clone(),
        Span::styled(" Esc ", btn_style),              // back
    ]);
    frame.render_widget(
        Paragraph::new(line).style(Style::default().bg(sc.body_bg)),
        area,
    );
}

fn render_status_row(frame: &mut Frame, area: Rect, app: &App, sc: &SC) {
    let mode_label = match app.skin_browser_source {
        SkinBrowserSource::Local => "LOCAL",
        SkinBrowserSource::Online => "ONLINE",
    };

    let loading_span = if app.skin_browser_loading {
        Span::styled(
            " LOADING ",
            Style::default()
                .fg(sc.led_on)
                .bg(sc.led_bg)
                .add_modifier(Modifier::BOLD),
        )
    } else {
        Span::styled("", Style::default())
    };

    let error_span = if let Some(ref err) = app.skin_browser_error {
        let short = if err.len() > 30 {
            format!("{}…", &err[..29])
        } else {
            err.clone()
        };
        Span::styled(
            format!(" {short} "),
            Style::default().fg(Color::Rgb(255, 40, 51)).bg(sc.chrome_dark),
        )
    } else {
        Span::styled("", Style::default())
    };

    let line = Line::from(vec![
        Span::styled(
            format!(" {mode_label} "),
            Style::default()
                .fg(sc.titlebar_bg)
                .bg(sc.chrome_dark)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled("  ", Style::default().bg(sc.body_bg)),
        Span::styled(
            format!(" {} skins ", app.skin_entries.len()),
            Style::default().fg(sc.indicator_on).bg(sc.chrome_dark),
        ),
        Span::styled("  ", Style::default().bg(sc.body_bg)),
        loading_span,
        error_span,
    ]);
    frame.render_widget(
        Paragraph::new(line).style(Style::default().bg(sc.body_bg)),
        area,
    );
}

// ─── Skin list ─────────────────────────────────────────────────────────

fn render_list_titlebar(frame: &mut Frame, area: Rect, _app: &App, sc: &SC) {
    let w = area.width as usize;
    let title = " SKIN BROWSER ";
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

fn render_skinlist(frame: &mut Frame, area: Rect, app: &App, sc: &SC) {
    if app.skin_entries.is_empty() {
        let msg = if app.skin_browser_loading {
            "Loading skins..."
        } else if let Some(ref err) = app.skin_browser_error {
            err
        } else {
            "No skins found."
        };

        let block = Block::default()
            .style(Style::default().bg(sc.plist_normal_bg))
            .padding(Padding::horizontal(1));
        let inner = block.inner(area);
        frame.render_widget(block, area);

        frame.render_widget(
            Paragraph::new(Line::from(Span::styled(
                msg.to_string(),
                Style::default().fg(sc.plist_normal),
            ))),
            inner,
        );
        return;
    }

    let items: Vec<ListItem> = app
        .skin_entries
        .iter()
        .enumerate()
        .map(|(i, entry)| {
            let local_tag = if entry.is_local {
                Span::styled("L", Style::default().fg(sc.indicator_on))
            } else {
                Span::styled(" ", Style::default().fg(sc.plist_normal))
            };

            let downloading = app
                .skin_downloading_md5
                .as_ref() == Some(&entry.md5);

            let status_tag = if downloading {
                Span::styled(
                    " \u{25B6}\u{25B6}",
                    Style::default().fg(sc.led_on),
                )
            } else {
                Span::raw("")
            };

            let swatch = parse_average_color(&entry.average_color)
                .map(|(r, g, b)| {
                    Span::styled(
                        "\u{2588}\u{2588}",
                        Style::default().fg(Color::Rgb(r, g, b)),
                    )
                })
                .unwrap_or_else(|| Span::raw("  "));

            let name = if entry.display_name.is_empty() {
                &entry.filename
            } else {
                &entry.display_name
            };

            ListItem::new(Line::from(vec![
                Span::styled(
                    format!("{:>3}.", i + 1),
                    Style::default().fg(sc.plist_normal),
                ),
                local_tag,
                Span::styled(" ", Style::default()),
                Span::styled(name.clone(), Style::default().fg(sc.plist_normal)),
                status_tag,
                Span::styled(" ", Style::default()),
                swatch,
            ]))
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

    frame.render_stateful_widget(list, area, &mut app.skin_list_state.clone());
}

// ─── Footer ────────────────────────────────────────────────────────────

fn render_footer(frame: &mut Frame, area: Rect, app: &App, sc: &SC) {
    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(1), Constraint::Length(1)])
        .split(area);

    // Input bar
    let prompt = if app.skin_search_active { " / " } else { " > " };
    let mut input_spans = vec![
        Span::styled(
            prompt,
            Style::default()
                .fg(sc.text_fg)
                .bg(sc.text_bg)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(
            &app.skin_search_query,
            Style::default().fg(sc.text_fg).bg(sc.text_bg),
        ),
    ];
    if app.skin_search_active {
        input_spans.push(Span::styled(
            "\u{2588}",
            Style::default().fg(sc.text_fg).bg(sc.text_bg),
        ));
    }
    frame.render_widget(
        Paragraph::new(Line::from(input_spans)).style(Style::default().bg(sc.text_bg)),
        rows[0],
    );

    // Key hints
    let hints = if app.skin_search_active {
        vec![
            Span::styled(" Enter", Style::default().fg(sc.led_on).bg(sc.chrome_dark)),
            Span::styled(" search ", Style::default().fg(sc.chrome_mid).bg(sc.chrome_dark)),
            Span::styled("Esc", Style::default().fg(sc.led_on).bg(sc.chrome_dark)),
            Span::styled(" cancel", Style::default().fg(sc.chrome_mid).bg(sc.chrome_dark)),
        ]
    } else {
        let mut h = vec![
            Span::styled(" /", Style::default().fg(sc.led_on).bg(sc.chrome_dark)),
            Span::styled(" search ", Style::default().fg(sc.chrome_mid).bg(sc.chrome_dark)),
            Span::styled("j/k", Style::default().fg(sc.led_on).bg(sc.chrome_dark)),
            Span::styled(" nav ", Style::default().fg(sc.chrome_mid).bg(sc.chrome_dark)),
            Span::styled("Enter", Style::default().fg(sc.led_on).bg(sc.chrome_dark)),
        ];
        match app.skin_browser_source {
            SkinBrowserSource::Local => {
                h.push(Span::styled(" apply ", Style::default().fg(sc.chrome_mid).bg(sc.chrome_dark)));
            }
            SkinBrowserSource::Online => {
                h.push(Span::styled(" download ", Style::default().fg(sc.chrome_mid).bg(sc.chrome_dark)));
                h.push(Span::styled("n", Style::default().fg(sc.led_on).bg(sc.chrome_dark)));
                h.push(Span::styled(" more ", Style::default().fg(sc.chrome_mid).bg(sc.chrome_dark)));
            }
        }
        h.push(Span::styled("Esc", Style::default().fg(sc.led_on).bg(sc.chrome_dark)));
        h.push(Span::styled(" back", Style::default().fg(sc.chrome_mid).bg(sc.chrome_dark)));
        h
    };
    frame.render_widget(
        Paragraph::new(Line::from(hints)).style(Style::default().bg(sc.chrome_dark)),
        rows[1],
    );
}

// ─── Helpers ───────────────────────────────────────────────────────────

fn parse_average_color(s: &Option<String>) -> Option<(u8, u8, u8)> {
    let s = s.as_ref()?;

    if let Some(inner) = s.strip_prefix("rgb(").and_then(|v| v.strip_suffix(')')) {
        let parts: Vec<&str> = inner.split(',').collect();
        if parts.len() == 3 {
            return Some((
                parts[0].trim().parse().ok()?,
                parts[1].trim().parse().ok()?,
                parts[2].trim().parse().ok()?,
            ));
        }
    }

    if let Some(hex) = s.strip_prefix('#') {
        if hex.len() == 6 {
            return Some((
                u8::from_str_radix(&hex[0..2], 16).ok()?,
                u8::from_str_radix(&hex[2..4], 16).ok()?,
                u8::from_str_radix(&hex[4..6], 16).ok()?,
            ));
        }
    }

    None
}
