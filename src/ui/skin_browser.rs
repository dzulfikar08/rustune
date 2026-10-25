//! Winamp-styled skin browser: installed skins or the whole webamp skin
//! museum on the left, a live preview of the highlighted skin (its real main
//! and equalizer windows) on the right. Colours come from the loaded skin.
//!
//! ```text
//!   ┌──── header (6 rows) ─────────────────────────────┐
//!   │ title bar · LED count · marquee · gauge · tabs · status
//!   ├── SKIN BROWSER ──────────┬── PREVIEW ────────────┤
//!   │ skin list                │ main window           │
//!   │                          │ equalizer             │
//!   │                          │ caption               │
//!   ├──────────────────────────┴───────────────────────┤
//!   │ search input · key hints                         │
//!   └──────────────────────────────────────────────────┘
//! ```

use std::time::Duration;

use ratatui::{
    buffer::Buffer,
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, HighlightSpacing, LineGauge, List, ListItem, ListState, Padding, Paragraph},
    Frame,
};

use crate::app::App;
use crate::gfx::{GfxPainter, Protocol};
use crate::keymap::{self, Action, Ctx};
use crate::net::SkinListing;
use crate::skin::{SkinColors, WinampSkin};
use crate::skin_browser::{Preview, SkinBrowser, Source};
use crate::ui::skin_bitmap::draw_halfblock_scaled;
use crate::ui::skin_layout;
use crate::view::{EqSettings, PlayStatus, PlayerView, RepeatMode, StreamFormat};
use crate::winamp::sprites::{WINDOW_H, WINDOW_W};
use crate::winamp::{WinampUiState, Window};

/// Narrower than this, the list takes the whole width.
const MIN_PREVIEW_WIDTH: u16 = 70;
/// Image slots of the preview's main and equalizer windows.
const SLOT_MAIN: usize = 0;
const SLOT_EQ: usize = 1;

/// Colours of the loaded skin (classic defaults when none is loaded).
fn colors(app: &App) -> &SkinColors {
    app.winamp_skin
        .as_ref()
        .map_or(&SkinColors::DEFAULT, |s| &s.colors)
}

/// Compositor state of the preview pane: a static "playing" frame.
pub struct PreviewUi {
    ui: WinampUiState,
    /// The visualizer has settled on the sample spectrum.
    primed: bool,
}

impl Default for PreviewUi {
    fn default() -> Self {
        let mut ui = WinampUiState::new();
        ui.show_playlist = false;
        Self { ui, primed: false }
    }
}

/// Fixed animation time of the preview: one second into playback, so the
/// analyzer bars stand at the sample spectrum and never move.
const PREVIEW_T: Duration = Duration::from_secs(1);

impl PreviewUi {
    fn compose(&mut self, skin: &WinampSkin, eq: bool) {
        let view = sample_view();
        if !self.primed {
            self.ui.compose_main(skin, &view, Duration::ZERO);
            self.primed = true;
        }
        self.ui.compose_main(skin, &view, PREVIEW_T);
        if eq {
            self.ui.compose_eq(skin, &view);
        }
    }
}

/// The player state every preview shows.
fn sample_view() -> PlayerView<'static> {
    static SPECTRUM: [f32; 19] = [
        0.55, 0.8, 0.95, 0.85, 0.7, 0.75, 0.6, 0.65, 0.5, 0.55, 0.45, 0.5, 0.4, 0.35, 0.3, 0.25,
        0.2, 0.15, 0.1,
    ];
    PlayerView {
        title: "rustune - skin preview",
        status: PlayStatus::Playing,
        elapsed: Duration::from_secs(154),
        duration: Some(Duration::from_secs(251)),
        volume: 0.8,
        balance: 0.0,
        shuffle: false,
        repeat: RepeatMode::All,
        eq: EqSettings {
            enabled: true,
            auto: false,
            preamp_db: 2.0,
            bands_db: [6.0, 4.5, 2.0, 0.0, -1.5, -3.0, -1.5, 1.0, 3.5, 5.0],
        },
        format: StreamFormat {
            kbps: Some(192),
            sample_rate: Some(44_100),
            channels: Some(2),
        },
        spectrum: &SPECTRUM,
        scope: &[],
        playlist: &[],
        current: None,
        selected: 0,
    }
}

/// Draw the browser; returns whether the frame uses the graphics painter
/// (its images then need `gfx.flush`, and `gfx.clear` once the browser closes).
pub fn render(
    frame: &mut Frame,
    area: Rect,
    app: &App,
    preview: &mut PreviewUi,
    gfx: &mut GfxPainter,
) -> bool {
    let sc = colors(app);
    let skins = &app.skins;
    let vertical = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(6),
            Constraint::Min(3),
            Constraint::Length(2),
        ])
        .split(area);
    let body = vertical[1];
    let preview_w = if body.width >= MIN_PREVIEW_WIDTH {
        body.width * 11 / 20
    } else {
        0
    };
    let columns = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Min(20), Constraint::Length(preview_w)])
        .split(body);

    render_header(frame, vertical[0], skins, app, sc);
    render_list(frame, columns[0], skins, sc);
    render_footer(frame, vertical[2], skins, sc);

    let graphics = gfx.protocol() != Protocol::HalfBlock;
    gfx.begin_frame(frame.area());
    if columns[1].width > 0 {
        render_preview(frame.buffer_mut(), columns[1], skins, sc, preview, gfx);
    }
    gfx.end_frame(frame.buffer_mut());
    graphics
}

// ─── Header (6 rows) ───────────────────────────────────────────────────

fn render_header(frame: &mut Frame, area: Rect, skins: &SkinBrowser, app: &App, sc: &SkinColors) {
    frame.render_widget(
        Block::default().style(Style::default().bg(sc.body_bg)),
        area,
    );
    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(1); 6])
        .split(area);
    render_titlebar(frame, rows[0], app, sc);
    render_count_and_vis(frame, rows[1], skins, sc);
    render_marquee(frame, rows[2], skins, sc);
    render_scroll_gauge(frame, rows[3], skins, sc);
    render_tabs(frame, rows[4], skins, sc);
    render_status_row(frame, rows[5], skins, sc);
}

fn render_titlebar(frame: &mut Frame, area: Rect, app: &App, sc: &SkinColors) {
    let skin_name = app
        .winamp_skin
        .as_ref()
        .map_or("WINAMP", |s| s.name.as_str());
    let title = format!(" {skin_name} \u{2014} SKIN BROWSER ");
    let controls = " \u{2500}\u{25A1}\u{2715} ";
    let pad =
        (area.width as usize).saturating_sub(title.chars().count() + controls.chars().count());
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
            controls,
            Style::default().fg(sc.chrome_light).bg(sc.chrome_dark),
        ),
    ]);
    frame.render_widget(
        Paragraph::new(line).style(Style::default().bg(sc.chrome_dark)),
        area,
    );
}

/// `103909` → `103,909`.
fn thousands(n: usize) -> String {
    let digits = n.to_string();
    let mut out = String::with_capacity(digits.len() + digits.len() / 3);
    for (i, c) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(c);
    }
    out
}

/// "103,909 skins", "12 found", "40+ found".
fn count_label(skins: &SkinBrowser) -> String {
    match skins.total {
        Some(total) => format!("{} skins", thousands(total)),
        None if skins.has_more => format!("{}+ found", thousands(skins.entries.len())),
        None => format!("{} found", thousands(skins.entries.len())),
    }
}

fn render_count_and_vis(frame: &mut Frame, area: Rect, skins: &SkinBrowser, sc: &SkinColors) {
    let led = format!(" {} ", count_label(skins));
    let led_w = led.chars().count();
    let mut spans = Vec::with_capacity(area.width as usize / 2 + 3);
    spans.push(Span::styled(
        led,
        Style::default()
            .fg(sc.led_on)
            .bg(sc.led_bg)
            .add_modifier(Modifier::BOLD),
    ));
    spans.push(Span::styled("  ", Style::default().bg(sc.body_bg)));
    // Decorative analyzer bars in the skin's VISCOLOR colours.
    const BARS: [&str; 8] = [
        "\u{2581} ",
        "\u{2582} ",
        "\u{2583} ",
        "\u{2584} ",
        "\u{2585} ",
        "\u{2586} ",
        "\u{2587} ",
        "\u{2588} ",
    ];
    let bar_count = (area.width as usize).saturating_sub(led_w + 4) / 2;
    for i in 0..bar_count {
        let h = ((i * 13 + 7) % 5) + 1;
        let color = sc.vis_colors[2 + (7 - h.min(7)) * 2];
        spans.push(Span::styled(
            BARS[h],
            Style::default().fg(color).bg(sc.led_bg),
        ));
    }
    frame.render_widget(
        Paragraph::new(Line::from(spans)).style(Style::default().bg(sc.body_bg)),
        area,
    );
}

fn listing_label(skins: &SkinBrowser) -> &'static str {
    match (skins.source, skins.listing) {
        (Source::Local, _) => "INSTALLED SKINS",
        (Source::Online, SkinListing::Museum) => "WINAMP SKIN MUSEUM",
        (Source::Online, SkinListing::Curated) => "CURATED MUSEUM SKINS",
    }
}

fn render_marquee(frame: &mut Frame, area: Rect, skins: &SkinBrowser, sc: &SkinColors) {
    let text = if skins.query.is_empty() {
        format!("  ***  {}  ***", listing_label(skins))
    } else {
        format!("  MUSEUM SEARCH  /{}/", skins.query)
    };
    let style = Style::default().fg(sc.text_fg).bg(sc.text_bg);
    frame.render_widget(
        Paragraph::new(Line::from(Span::styled(text, style))).style(style),
        area,
    );
}

fn render_scroll_gauge(frame: &mut Frame, area: Rect, skins: &SkinBrowser, sc: &SkinColors) {
    let total = skins.entries.len();
    let selected = skins.list_state.selected().unwrap_or(0);
    let ratio = if total > 1 {
        selected as f64 / (total - 1) as f64
    } else {
        0.0
    };
    let label = if total == 0 {
        "   0/0".to_string()
    } else {
        format!("{:>4}/{}", selected + 1, total)
    };
    let gauge = LineGauge::default()
        .ratio(ratio.clamp(0.0, 1.0))
        .label(Span::styled(label, Style::default().fg(sc.led_on)))
        .filled_style(Style::default().fg(sc.seek_filled).bg(sc.seek_track))
        .unfilled_style(Style::default().fg(sc.seek_track).bg(sc.body_bg))
        .line_set(ratatui::symbols::line::THICK);
    frame.render_widget(gauge, area);
}

/// Source and listing switches as Winamp buttons, the active one pressed.
fn render_tabs(frame: &mut Frame, area: Rect, skins: &SkinBrowser, sc: &SkinColors) {
    let normal = Style::default().fg(sc.btn_text).bg(sc.btn_normal);
    let active = Style::default()
        .fg(Color::Black)
        .bg(sc.chrome_light)
        .add_modifier(Modifier::BOLD);
    let gap = Span::styled(" ", Style::default().bg(sc.body_bg));
    let online = skins.source == Source::Online;
    let pick = |on: bool| if on { active } else { normal };
    let tab_keys = keymap::hint_for(Ctx::SKINS, Action::NextTab).map_or("", |(k, _)| k);
    let filter_keys =
        keymap::hint_for(Ctx::SKINS_ONLINE, Action::SkinFilter).map_or("", |(k, _)| k);
    let line = Line::from(vec![
        gap.clone(),
        Span::styled(" Installed ", pick(!online)),
        gap.clone(),
        Span::styled(
            " Museum: all ",
            pick(online && skins.listing == SkinListing::Museum),
        ),
        gap,
        Span::styled(
            " Curated ",
            pick(online && skins.listing == SkinListing::Curated),
        ),
        Span::styled(
            format!("   {tab_keys} source · {filter_keys} all/curated"),
            Style::default().fg(sc.chrome_mid).bg(sc.body_bg),
        ),
    ]);
    frame.render_widget(
        Paragraph::new(line).style(Style::default().bg(sc.body_bg)),
        area,
    );
}

/// The first `max` characters of `s`, with an ellipsis when cut.
fn clip(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    let mut out: String = s.chars().take(max.saturating_sub(1)).collect();
    out.push('\u{2026}');
    out
}

fn render_status_row(frame: &mut Frame, area: Rect, skins: &SkinBrowser, sc: &SkinColors) {
    let mode = if skins.source == Source::Online {
        " ONLINE "
    } else {
        " LOCAL "
    };
    let mut spans = vec![
        Span::styled(
            mode,
            Style::default()
                .fg(sc.titlebar_bg)
                .bg(sc.chrome_dark)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(" ", Style::default().bg(sc.body_bg)),
    ];
    let room = (area.width as usize).saturating_sub(12);
    if skins.loading {
        spans.push(Span::styled(
            " LOADING ",
            Style::default()
                .fg(sc.led_on)
                .bg(sc.led_bg)
                .add_modifier(Modifier::BOLD),
        ));
    }
    if skins.installing.is_some() {
        spans.push(Span::styled(
            " INSTALLING ",
            Style::default()
                .fg(sc.led_on)
                .bg(sc.led_bg)
                .add_modifier(Modifier::BOLD),
        ));
    }
    if let Some(err) = &skins.error {
        spans.push(Span::styled(
            format!(" {} ", clip(err, room)),
            Style::default()
                .fg(Color::Rgb(255, 40, 51))
                .bg(sc.chrome_dark),
        ));
    } else if let Some(notice) = &skins.notice {
        spans.push(Span::styled(
            format!(" {} ", clip(notice, room)),
            Style::default().fg(sc.indicator_on).bg(sc.chrome_dark),
        ));
    }
    frame.render_widget(
        Paragraph::new(Line::from(spans)).style(Style::default().bg(sc.body_bg)),
        area,
    );
}

// ─── Skin list ─────────────────────────────────────────────────────────

fn render_list(frame: &mut Frame, area: Rect, skins: &SkinBrowser, sc: &SkinColors) {
    if area.height == 0 {
        return;
    }
    let title = Rect { height: 1, ..area };
    render_bar_title(frame.buffer_mut(), title, " SKIN BROWSER ", sc);
    let area = Rect {
        y: area.y + 1,
        height: area.height - 1,
        ..area
    };
    if skins.entries.is_empty() {
        let msg: &str = if skins.loading {
            "Loading skins..."
        } else if let Some(err) = &skins.error {
            err
        } else if skins.source == Source::Local {
            "No skins installed yet. Tab: browse the museum."
        } else {
            "No skins found."
        };
        let block = Block::default()
            .style(Style::default().bg(sc.plist_normal_bg))
            .padding(Padding::horizontal(1));
        let inner = block.inner(area);
        frame.render_widget(block, area);
        frame.render_widget(
            Paragraph::new(Span::styled(msg, Style::default().fg(sc.plist_normal))),
            inner,
        );
        return;
    }

    // Only the visible rows are built: the museum list can be thousands long.
    let rows = area.height as usize;
    let selected = skins.list_state.selected().unwrap_or(0);
    let first = (selected + 1).saturating_sub(rows);
    let items: Vec<ListItem> = skins
        .entries
        .iter()
        .enumerate()
        .skip(first)
        .take(rows)
        .map(|(i, entry)| {
            let marker = if skins.installing.as_deref() == Some(entry.md5.as_str()) {
                Span::styled("\u{2193}", Style::default().fg(sc.led_on))
            } else if entry.is_local && skins.source == Source::Online {
                Span::styled("\u{2713}", Style::default().fg(sc.indicator_on))
            } else {
                Span::raw(" ")
            };
            let swatch = parse_average_color(entry.average_color.as_deref()).map_or_else(
                || Span::raw("  "),
                |(r, g, b)| {
                    Span::styled("\u{2588}\u{2588}", Style::default().fg(Color::Rgb(r, g, b)))
                },
            );
            let name = if entry.display_name.is_empty() {
                &entry.filename
            } else {
                &entry.display_name
            };
            ListItem::new(Line::from(vec![
                Span::styled(
                    format!("{:>4}.", i + 1),
                    Style::default().fg(sc.plist_normal),
                ),
                marker,
                Span::raw(" "),
                swatch,
                Span::raw(" "),
                Span::styled(name.as_str(), Style::default().fg(sc.plist_normal)),
            ]))
        })
        .collect();
    let list = List::new(items)
        .block(Block::default().style(Style::default().bg(sc.plist_normal_bg)))
        .highlight_style(
            Style::default()
                .fg(sc.plist_current)
                .bg(sc.plist_selected_bg)
                .add_modifier(Modifier::BOLD),
        )
        .highlight_symbol("\u{25B6}")
        .highlight_spacing(HighlightSpacing::Always);
    let mut state = ListState::default().with_selected(Some(selected - first));
    frame.render_stateful_widget(list, area, &mut state);
}

/// A playlist-style bar with a centred title.
fn render_bar_title(buf: &mut Buffer, area: Rect, title: &str, sc: &SkinColors) {
    let w = area.width as usize;
    let title = clip(title, w);
    let pad = w.saturating_sub(title.chars().count());
    let line = Line::from(vec![
        Span::styled(
            "\u{2500}".repeat(pad / 2),
            Style::default().fg(sc.plist_normal).bg(sc.plist_normal_bg),
        ),
        Span::styled(
            title,
            Style::default()
                .fg(sc.plist_current)
                .bg(sc.plist_normal_bg)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(
            "\u{2500}".repeat(pad - pad / 2),
            Style::default().fg(sc.plist_normal).bg(sc.plist_normal_bg),
        ),
    ]);
    buf.set_line(area.x, area.y, &line, area.width);
}

// ─── Preview ───────────────────────────────────────────────────────────

fn render_preview(
    buf: &mut Buffer,
    area: Rect,
    skins: &SkinBrowser,
    sc: &SkinColors,
    preview: &mut PreviewUi,
    gfx: &mut GfxPainter,
) {
    let bg = Style::default().bg(sc.plist_normal_bg);
    buf.set_style(area, bg);
    if area.height < 3 {
        return;
    }
    render_bar_title(buf, Rect { height: 1, ..area }, " PREVIEW ", sc);
    let caption_at = area.bottom() - 1;
    // One blank column separates the pane from the list.
    let image = Rect::new(area.x + 1, area.y + 1, area.width - 1, area.height - 2);
    let text = Style::default().fg(sc.plist_normal).bg(sc.plist_normal_bg);
    let hint = Style::default().fg(sc.plist_current).bg(sc.plist_normal_bg);

    let caption: (&str, Style) = match &skins.preview {
        Preview::Ready { skin, .. } => {
            draw_skin(buf, image, skin, preview, gfx);
            match skins.selected() {
                Some(e) if skins.installing.as_deref() == Some(e.md5.as_str()) => {
                    ("Installing\u{2026}", hint)
                }
                Some(e) if e.is_local => ("Enter: apply this skin", hint),
                _ => ("Enter: install and apply", hint),
            }
        }
        Preview::Failed { error, .. } => {
            put_wrapped(buf, image, error, text);
            ("No preview", text)
        }
        Preview::Pending { .. } | Preview::Loading { .. } => ("Loading preview\u{2026}", text),
        Preview::None => ("", text),
    };
    let name = skins.selected().map_or("", |e| e.display_name.as_str());
    let line = Line::from(vec![
        Span::styled(" ", text),
        Span::styled(name, text.add_modifier(Modifier::BOLD)),
        Span::styled("  ", text),
        Span::styled(caption.0, caption.1),
    ]);
    buf.set_line(area.x, caption_at, &line, area.width);
}

/// Draw the skin's main window (and the equalizer when it fits) into `area`.
fn draw_skin(
    buf: &mut Buffer,
    area: Rect,
    skin: &WinampSkin,
    preview: &mut PreviewUi,
    gfx: &mut GfxPainter,
) {
    if !skin.has_bitmaps() {
        put_wrapped(
            buf,
            area,
            "This skin has no main window bitmap.",
            Style::default().fg(skin.colors.plist_normal),
        );
        return;
    }
    if gfx.protocol() != Protocol::HalfBlock {
        if let Some(plan) = skin_layout::plan(area, gfx.cell_size(), 8, true, false) {
            preview.compose(skin, plan.eq.is_some());
            let main = preview.ui.canvas(Window::Main);
            gfx.place(
                buf,
                SLOT_MAIN,
                main.pixels(),
                main.width(),
                main.height(),
                plan.scale,
                plan.main.at,
            );
            if let Some(eq) = plan.eq {
                let canvas = preview.ui.canvas(Window::Equalizer);
                gfx.place(
                    buf,
                    SLOT_EQ,
                    canvas.pixels(),
                    canvas.width(),
                    canvas.height(),
                    plan.scale,
                    eq.at,
                );
            }
            return;
        }
    }
    // Half-blocks, scaled down to the pane (1:1 at most).
    let (w, h) = (u32::from(area.width), u32::from(area.height) * 2);
    let fit = |win_h: u32| -> (u32, u32) {
        // Largest size ≤ 1:1 with the window's aspect ratio inside w × h.
        if w * win_h <= h * WINDOW_W {
            let cols = w.min(WINDOW_W);
            (cols, cols * win_h / WINDOW_W)
        } else {
            let px_h = h.min(win_h);
            (px_h * WINDOW_W / win_h, px_h)
        }
    };
    let both = fit(WINDOW_H * 2);
    let alone = fit(WINDOW_H);
    // Show the equalizer unless it costs the main window more than a third.
    let eq = both.0 * 3 >= alone.0 * 2;
    let (cols, px_h) = if eq { (both.0, both.1 / 2) } else { alone };
    if cols == 0 || px_h < 2 {
        return;
    }
    preview.compose(skin, eq);
    let x = area.x + (area.width - cols as u16) / 2;
    let main = draw_halfblock_scaled(
        buf,
        preview.ui.canvas(Window::Main),
        (x, area.y),
        cols as u16,
        px_h as u16,
    );
    if eq {
        draw_halfblock_scaled(
            buf,
            preview.ui.canvas(Window::Equalizer),
            (x, main.bottom()),
            cols as u16,
            px_h as u16,
        );
    }
}

/// Word-wrapped `text` inside `area`.
fn put_wrapped(buf: &mut Buffer, area: Rect, text: &str, style: Style) {
    let width = area.width.saturating_sub(2) as usize;
    if width == 0 {
        return;
    }
    let mut y = area.y;
    let mut line = String::new();
    for word in text.split_whitespace() {
        if !line.is_empty() && line.chars().count() + 1 + word.chars().count() > width {
            if y >= area.bottom() {
                return;
            }
            buf.set_stringn(area.x + 1, y, &line, width, style);
            y += 1;
            line.clear();
        }
        if !line.is_empty() {
            line.push(' ');
        }
        line.push_str(word);
    }
    if !line.is_empty() && y < area.bottom() {
        buf.set_stringn(area.x + 1, y, &line, width, style);
    }
}

// ─── Footer ────────────────────────────────────────────────────────────

fn render_footer(frame: &mut Frame, area: Rect, skins: &SkinBrowser, sc: &SkinColors) {
    let input_style = Style::default().fg(sc.text_fg).bg(sc.text_bg);
    let typed = skins.input.as_deref();
    let mut spans = vec![
        Span::styled(
            if typed.is_some() { " / " } else { " > " },
            input_style.add_modifier(Modifier::BOLD),
        ),
        Span::styled(typed.unwrap_or(skins.query.as_str()), input_style),
    ];
    if typed.is_some() {
        spans.push(Span::styled("\u{2588}", input_style));
    }
    let buf = frame.buffer_mut();
    buf.set_style(Rect { height: 1, ..area }, input_style);
    buf.set_line(area.x, area.y, &Line::from(spans), area.width);

    if area.height < 2 {
        return;
    }
    let key_style = Style::default().fg(sc.led_on).bg(sc.chrome_dark);
    let desc_style = Style::default().fg(sc.chrome_mid).bg(sc.chrome_dark);
    let mut hints = Vec::new();
    let mut push = |(keys, hint): (&'static str, &'static str)| {
        hints.push(Span::styled(format!(" {keys}"), key_style));
        hints.push(Span::styled(format!(" {hint} "), desc_style));
    };
    if typed.is_some() {
        [Action::Submit, Action::Cancel, Action::ClearLine]
            .into_iter()
            .filter_map(|a| keymap::hint_for(Ctx::PROMPT, a))
            .for_each(&mut push);
    } else {
        keymap::hints(skins.ctx()).for_each(&mut push);
    }
    let row = Rect {
        y: area.y + 1,
        height: 1,
        ..area
    };
    buf.set_style(row, desc_style);
    buf.set_line(row.x, row.y, &Line::from(hints), row.width);
}

// ─── Helpers ───────────────────────────────────────────────────────────

fn parse_average_color(s: Option<&str>) -> Option<(u8, u8, u8)> {
    let s = s?;
    if let Some(inner) = s.strip_prefix("rgb(").and_then(|v| v.strip_suffix(')')) {
        let mut parts = inner.split(',').map(|p| p.trim().parse::<u8>());
        let (Some(Ok(r)), Some(Ok(g)), Some(Ok(b)), None) =
            (parts.next(), parts.next(), parts.next(), parts.next())
        else {
            return None;
        };
        return Some((r, g, b));
    }
    let hex = s.strip_prefix('#').filter(|h| h.len() == 6)?;
    let byte = |i: usize| u8::from_str_radix(hex.get(i..i + 2)?, 16).ok();
    Some((byte(0)?, byte(2)?, byte(4)?))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::skin_browser::SkinEntry;
    use ratatui::layout::Position;

    #[test]
    fn counts_and_colours_format() {
        assert_eq!(thousands(0), "0");
        assert_eq!(thousands(999), "999");
        assert_eq!(thousands(103_909), "103,909");
        assert_eq!(thousands(1_234_567), "1,234,567");
        assert_eq!(parse_average_color(Some("rgb(1, 2,3)")), Some((1, 2, 3)));
        assert_eq!(parse_average_color(Some("#0a0B0c")), Some((10, 11, 12)));
        assert_eq!(parse_average_color(Some("rgb(1,2)")), None);
        assert_eq!(parse_average_color(Some("#ffé0000")), None);
        assert_eq!(clip("abcdef", 4), "abc\u{2026}");
    }

    fn browser_with(n: usize) -> SkinBrowser {
        let mut b = SkinBrowser::default();
        b.open(Source::Online);
        b.apply_page(
            crate::net::SkinPage {
                entries: (0..n)
                    .map(|i| SkinEntry {
                        md5: format!("{i:032x}"),
                        filename: format!("skin{i}.wsz"),
                        display_name: format!("skin{i}"),
                        is_local: i == 0,
                        average_color: None,
                    })
                    .collect(),
                next_offset: n,
                total: Some(103_909),
                has_more: true,
            },
            0,
        );
        b
    }

    fn draw(app: &App, preview: &mut PreviewUi, gfx: &mut GfxPainter, w: u16, h: u16) -> Buffer {
        let mut terminal =
            ratatui::Terminal::new(ratatui::backend::TestBackend::new(w, h)).unwrap();
        terminal
            .draw(|f| {
                let area = f.area();
                render(f, area, app, preview, gfx);
            })
            .unwrap();
        terminal.backend().buffer().clone()
    }

    fn text(buf: &Buffer) -> String {
        let a = buf.area;
        let mut s = String::new();
        for y in a.top()..a.bottom() {
            for x in a.left()..a.right() {
                s.push_str(buf.cell(Position::new(x, y)).map_or(" ", |c| c.symbol()));
            }
            s.push('\n');
        }
        s
    }

    #[test]
    fn shows_the_museum_total_and_a_half_block_preview() {
        let mut app = App::new(crate::config::Config::default());
        app.skins = browser_with(300);
        app.skins.list_state.select(Some(250));
        app.skins.preview = Preview::Ready {
            key: format!("{:032x}", 250),
            skin: Box::new(synthetic_skin()),
        };
        let mut preview = PreviewUi::default();
        let mut gfx = GfxPainter::with_cell_size(Protocol::HalfBlock, None);
        let buf = draw(&app, &mut preview, &mut gfx, 120, 40);
        let screen = text(&buf);
        assert!(screen.contains("103,909 skins"), "{screen}");
        assert!(screen.contains("251."), "the cursor row is drawn");
        assert!(screen.contains("PREVIEW"));
        assert!(screen.contains("Enter: install and apply"));
        assert!(screen.contains("apply") && screen.contains("all/curated"));
        // The main window's solid colour, box-filtered into half-blocks.
        let main = Color::Rgb(40, 40, 60);
        assert!(buf.content().iter().any(|c| c.bg == main || c.fg == main));

        app.skins.preview = Preview::Failed {
            key: String::new(),
            error: "Downloaded file is not a skin archive".into(),
        };
        let screen = text(&draw(&app, &mut preview, &mut gfx, 120, 40));
        assert!(screen.contains("not a skin archive"), "{screen}");
        // Too narrow for a preview pane: the list keeps working.
        let screen = text(&draw(&app, &mut preview, &mut gfx, 50, 20));
        assert!(!screen.contains("PREVIEW"));
    }

    #[test]
    fn pixel_protocols_place_the_preview_images() {
        let mut app = App::new(crate::config::Config::default());
        app.skins = browser_with(3);
        app.skins.preview = Preview::Ready {
            key: format!("{:032x}", 0),
            skin: Box::new(synthetic_skin()),
        };
        let mut preview = PreviewUi::default();
        let mut gfx = GfxPainter::with_cell_size(Protocol::Kitty, Some((10, 20)));
        draw(&app, &mut preview, &mut gfx, 160, 50);
        let mut out = Vec::new();
        gfx.flush(&mut out).unwrap();
        let sent = String::from_utf8_lossy(&out);
        assert!(sent.matches("\x1b_G").count() >= 2, "main + EQ images");

        // Without a preview the images are deleted on the next flush.
        app.skins.preview = Preview::Loading { key: String::new() };
        draw(&app, &mut preview, &mut gfx, 160, 50);
        let mut out = Vec::new();
        gfx.flush(&mut out).unwrap();
        assert!(
            String::from_utf8_lossy(&out).contains("a=d"),
            "kitty delete"
        );
    }

    /// A skin with solid main and equalizer bitmaps.
    fn synthetic_skin() -> WinampSkin {
        use crate::skin::tests::{build_wsz, solid_bmp};
        let wsz = build_wsz(&[
            ("MAIN.BMP", solid_bmp(275, 116, [40, 40, 60])),
            ("EQMAIN.BMP", solid_bmp(275, 315, [20, 60, 20])),
            ("TEXT.BMP", solid_bmp(155, 18, [0, 0, 0])),
        ]);
        WinampSkin::from_wsz_bytes(&wsz, "synthetic").unwrap()
    }
}
