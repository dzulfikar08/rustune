mod help;
mod input;
mod onboarding;
mod player;
mod results;
mod settings;
// The three skin modules below are owned by the skin/gfx workstream, which
// carries their lint fixes.
#[allow(dead_code, clippy::all)]
mod skin_bitmap;
mod skin_browser;
#[allow(dead_code, clippy::all)]
pub mod skin_layout;
#[allow(dead_code, unused_assignments, clippy::all)]
mod winamp;

use ratatui::Frame;

use crate::app::{App, Mode};

pub fn draw(frame: &mut Frame, app: &mut App) {
    // Full-screen takeovers
    match app.mode {
        Mode::Onboarding => return onboarding::render(frame, frame.area(), app),
        Mode::Settings => return settings::render(frame, frame.area(), app),
        Mode::SkinBrowser => return skin_browser::render(frame, frame.area(), app),
        Mode::Browse | Mode::Input => {}
    }

    // Winamp gets its own complete layout
    if app.theme.name == "Winamp" {
        winamp::render(frame, app);
        return;
    }

    draw_standard(frame, app);
}

fn draw_standard(frame: &mut Frame, app: &mut App) {
    use ratatui::layout::{Constraint, Direction, Layout, Rect};

    let area = frame.area();

    let vertical = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Min(3),    // results / playlist
            Constraint::Length(2), // player bar (info + gauge)
            Constraint::Length(1), // input / status bar
            Constraint::Length(1), // help bar
        ])
        .split(area);

    let player_chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(1), Constraint::Length(1)])
        .split(vertical[1]);

    let results_area = vertical[0];
    let info = player_chunks[0];

    app.layout_rects = crate::app::LayoutRects {
        results: results_area,
        player_info: info,
        player_bar: player_chunks[1],
        input: vertical[2],
        help: vertical[3],
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

    if app.show_playlist {
        results::render_queue(frame, results_area, app);
    } else {
        results::render(frame, results_area, app);
    }
    player::render(frame, player_chunks[0], player_chunks[1], app);
    input::render(frame, vertical[2], app);
    help::render(frame, vertical[3], app);
}
