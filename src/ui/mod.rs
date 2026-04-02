mod results;
mod player;
mod input;
mod help;

use ratatui::layout::{Constraint, Direction, Layout};
use ratatui::Frame;

use crate::app::App;

pub fn draw(frame: &mut Frame, app: &mut App) {
    let area = frame.area();

    let vertical = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Min(3),    // results area
            Constraint::Length(1), // player bar
            Constraint::Length(1), // input bar
            Constraint::Length(1), // help bar
        ])
        .split(area);

    results::render(frame, vertical[0], app);
    player::render(frame, vertical[1], app);
    input::render(frame, vertical[2], app);
    help::render(frame, vertical[3], &app.mode);
}
