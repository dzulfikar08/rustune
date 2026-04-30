mod help;
mod input;
mod onboarding;
mod player;
mod results;
mod settings;
mod skin_browser;
mod skin_bitmap;
pub mod skin_layout;
mod winamp;

use ratatui::Frame;

use crate::app::{App, Mode};

pub fn draw(frame: &mut Frame, app: &mut App) {
    // Full-screen takeover for onboarding
    if app.mode == Mode::Onboarding {
        onboarding::render(frame, frame.area(), app);
        return;
    }

    // Full-screen takeover for settings
    if app.mode == Mode::Settings {
        settings::render(frame, frame.area(), app);
        return;
    }

    // Full-screen takeover for skin browser
    if app.mode == Mode::SkinBrowser {
        skin_browser::render(frame, frame.area(), app);
        return;
    }

    // Winamp gets its own complete layout
    if app.theme.name == "Winamp" {
        winamp::render(frame, app);
        return;
    }

    // Default Dark/Light layout
    draw_standard(frame, app);
}

fn draw_standard(frame: &mut Frame, app: &mut App) {
    use ratatui::layout::{Constraint, Direction, Layout, Rect};

    let area = frame.area();

    let vertical = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Min(3),    // results area
            Constraint::Length(2), // player bar (info + gauge)
            Constraint::Length(1), // input bar
            Constraint::Length(1), // help bar
        ])
        .split(area);

    let player_chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1), // info line
            Constraint::Length(1), // gauge line
        ])
        .split(vertical[1]);

    let results_area = vertical[0];
    let info = player_chunks[0];

    let pause_button = Rect::new(info.x, info.y, 4, 1);

    let prev_page = Rect::new(
        results_area.x + results_area.width.saturating_sub(6),
        results_area.y,
        3,
        1,
    );
    let next_page = Rect::new(
        results_area.x + results_area.width.saturating_sub(3),
        results_area.y,
        3,
        1,
    );

    app.layout_rects = crate::app::LayoutRects {
        results: results_area,
        player_info: info,
        player_bar: player_chunks[1],
        input: vertical[2],
        help: vertical[3],
        pause_button,
        prev_page,
        next_page,
    };

    results::render(frame, vertical[0], app);
    player::render(frame, player_chunks[0], player_chunks[1], app);
    input::render(frame, vertical[2], app);
    help::render(frame, vertical[3], &app.mode, &app.active_source, &app.theme);
}
