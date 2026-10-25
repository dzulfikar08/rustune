mod browse;
mod eq;
mod help;
mod input;
mod lyrics;
mod onboarding;
mod player;
mod results;
mod settings;
mod skin_bitmap;
mod skin_browser;
pub mod skin_layout;
mod winamp;

pub use eq::handle_key as eq_key;

use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;
use ratatui::Frame;

use crate::app::{App, LayoutRects, LyricsStatus, Mode, Tab};
use crate::gfx::GfxPainter;
use crate::vis::widget::Visualizer;
use crate::winamp::WinampUiState;

/// Height of the visualizer strip in the standard layout (0 = hidden).
fn vis_height(app: &App, total: u16) -> u16 {
    if app.vis_mode.is_none() || total < 20 {
        0
    } else {
        (total / 5).clamp(4, 10)
    }
}

/// Draw one frame; returns whether the Winamp skin view was drawn (its
/// images then need `gfx.flush`). Winamp state lives outside `App` because
/// the skin view borrows `App` through `player_view()` while the painter
/// is mutated.
pub fn draw(
    frame: &mut Frame,
    app: &mut App,
    winamp_ui: &mut WinampUiState,
    gfx: &mut GfxPainter,
) -> bool {
    // Full-screen takeovers
    let area = frame.area();
    match app.mode {
        Mode::Onboarding => onboarding::render(frame, area, app),
        Mode::Settings => settings::render(frame, area, app),
        Mode::SkinBrowser => skin_browser::render(frame, area, app),
        Mode::Browse | Mode::Input if app.is_winamp() => {
            let view = app.player_view();
            winamp::draw(frame, app, app.winamp_skin.as_ref(), &view, winamp_ui, gfx);
            return true;
        }
        Mode::Browse | Mode::Input => draw_standard(frame, app),
    }
    false
}

/// Whether the visualizer is on screen for the current theme and size.
pub fn vis_visible(app: &App, winamp_ui: &WinampUiState, screen_height: u16) -> bool {
    match app.mode {
        Mode::Browse | Mode::Input if app.is_winamp() => {
            winamp_ui.vis_mode != crate::winamp::VisMode::Off
        }
        Mode::Browse | Mode::Input => vis_height(app, screen_height) > 0,
        _ => false,
    }
}

fn draw_tabs(frame: &mut Frame, area: Rect, app: &App) {
    let theme = &app.theme;
    let mut spans = Vec::with_capacity(app.tabs.len() * 2 + 1);
    for &tab in &app.tabs {
        let style = if tab == app.tab {
            theme.result_selected
        } else {
            theme.help_desc
        };
        spans.push(Span::styled(format!(" {} ", tab.label()), style));
        spans.push(Span::raw(" "));
    }
    spans.push(Span::styled("Tab ⇥", theme.help_key));
    frame.render_widget(Paragraph::new(Line::from(spans)), area);
}

fn draw_standard(frame: &mut Frame, app: &mut App) {
    let area = frame.area();
    let vis_h = vis_height(app, area.height);

    let vertical = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1),     // source tabs
            Constraint::Min(3),        // results / browser / playlist (+ side panels)
            Constraint::Length(vis_h), // visualizer
            Constraint::Length(2),     // player bar (info + gauge)
            Constraint::Length(1),     // input / status bar
            Constraint::Length(1),     // help bar
        ])
        .split(area);

    let player_chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(1), Constraint::Length(1)])
        .split(vertical[3]);

    let main = vertical[1];
    let side_w = if (app.show_eq || app.show_lyrics) && main.width >= 60 {
        (main.width * 2 / 5).min(56)
    } else {
        0
    };
    let columns = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Min(20), Constraint::Length(side_w)])
        .split(main);
    let results_area = columns[0];
    let side = columns[1];
    let info = player_chunks[0];

    app.layout_rects = LayoutRects {
        results: results_area,
        player_bar: player_chunks[1],
        pause_button: Rect::new(info.x, info.y, 4, 1),
        prev_page: Rect::new(
            results_area.x + results_area.width.saturating_sub(6),
            results_area.y,
            3,
            1,
        ),
        next_page: Rect::new(
            results_area.x + results_area.width.saturating_sub(3),
            results_area.y,
            3,
            1,
        ),
    };

    draw_tabs(frame, vertical[0], app);
    if app.show_playlist {
        results::render_queue(frame, results_area, app);
    } else if let Tab::Provider(kind) = app.tab {
        let theme = &app.theme;
        if let Some(browser) = app.browsers.iter_mut().find(|b| b.kind == kind) {
            browse::render(frame, results_area, browser, theme);
        }
    } else {
        results::render(frame, results_area, app);
    }

    if side.width > 0 {
        let (eq_area, lyrics_area) = match (app.show_eq, app.show_lyrics) {
            (true, true) => {
                let parts = Layout::default()
                    .direction(Direction::Vertical)
                    .constraints([Constraint::Length(14), Constraint::Min(3)])
                    .split(side);
                (parts[0], parts[1])
            }
            (true, false) => (side, Rect::default()),
            _ => (Rect::default(), side),
        };
        if eq_area.area() > 0 {
            eq::render(frame, eq_area, &app.eq, app.eq_focus, &app.theme);
        }
        if lyrics_area.area() > 0 {
            let state = if !app.config.providers.lyrics.enabled {
                lyrics::LyricsState::Disabled
            } else {
                match &app.lyrics {
                    LyricsStatus::Idle => lyrics::LyricsState::Idle,
                    LyricsStatus::Loading => lyrics::LyricsState::Loading,
                    LyricsStatus::NotFound => lyrics::LyricsState::NotFound,
                    LyricsStatus::Error(e) => lyrics::LyricsState::Error(e),
                    LyricsStatus::Ready(l) => lyrics::LyricsState::Ready(l),
                }
            };
            let pos = app
                .playback
                .as_ref()
                .map_or_else(Default::default, |p| p.elapsed);
            lyrics::render(frame, lyrics_area, state, pos, &app.theme);
        }
    }

    if vis_h > 0 {
        let vis_area = vertical[2];
        app.fit_vis(vis_area.width);
        frame.render_widget(
            Visualizer::new(&app.analyzer, app.vis_mode, &app.vis_palette),
            vis_area,
        );
    }

    player::render(frame, player_chunks[0], player_chunks[1], app);
    input::render(frame, vertical[4], app);
    help::render(frame, vertical[5], app);
}
