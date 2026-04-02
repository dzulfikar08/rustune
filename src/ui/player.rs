use ratatui::{
    style::{Color, Style},
    text::{Line, Span},
    widgets::Paragraph,
    Frame,
};

use crate::app::{App, PlaybackState, Status};

pub fn render(frame: &mut Frame, area: ratatui::layout::Rect, app: &App) {
    let content = match &app.playback {
        Some(PlaybackState {
            title,
            duration_secs,
            elapsed_secs,
            paused,
        }) => {
            let elapsed_str = App::format_duration(*elapsed_secs);
            let duration_str = App::format_duration(*duration_secs);
            let state_icon = if *paused { "||" } else { ">>" };

            // Progress bar
            let pct = if *duration_secs > 0 {
                *elapsed_secs as f64 / *duration_secs as f64
            } else {
                0.0
            };
            let bar_width = 20;
            let filled = (pct * bar_width as f64).round() as usize;
            let bar: String = "=".repeat(filled) + "-".repeat(bar_width - filled).as_str();

            vec![Line::from(vec![
                Span::styled(format!(" {state_icon} "), Style::default().fg(Color::Cyan)),
                Span::styled(
                    truncate(title, 40),
                    Style::default().fg(Color::Cyan),
                ),
                Span::styled(
                    format!(" [{bar}] {elapsed_str}/{duration_str}"),
                    Style::default().fg(Color::Green),
                ),
            ])]
        }
        None => match &app.status {
            Status::Loading(text) => {
                vec![Line::from(Span::styled(
                    format!(" {text}"),
                    Style::default().fg(Color::Yellow),
                ))]
            }
            _ => vec![Line::from("")],
        },
    };

    let paragraph = Paragraph::new(content);
    frame.render_widget(paragraph, area);
}

fn truncate(s: &str, max_len: usize) -> String {
    if s.len() <= max_len {
        s.to_string()
    } else {
        let mut truncated: String = s.chars().take(max_len - 1).collect();
        truncated.push('…');
        truncated
    }
}
