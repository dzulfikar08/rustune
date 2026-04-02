use ratatui::{
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::Paragraph,
    Frame,
};

use crate::app::{App, Mode};

pub fn render(frame: &mut Frame, area: ratatui::layout::Rect, app: &App) {
    let prompt = match app.mode {
        Mode::Browse => " > ",
        Mode::Input => " / ",
    };

    let mut spans = vec![
        Span::styled(
            prompt,
            Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD),
        ),
        Span::styled(&app.input_text, Style::default().fg(Color::White)),
    ];

    if app.mode == Mode::Input {
        spans.push(Span::styled(
            "\u{2588}", // block cursor
            Style::default().fg(Color::White),
        ));
    }

    let line = Line::from(spans);
    let paragraph = Paragraph::new(line);
    frame.render_widget(paragraph, area);
}
