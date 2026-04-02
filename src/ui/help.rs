use ratatui::{
    style::{Color, Style},
    text::{Line, Span},
    widgets::Paragraph,
    Frame,
};

use crate::app::Mode;

pub fn render(frame: &mut Frame, area: ratatui::layout::Rect, mode: &Mode) {
    let hints = match mode {
        Mode::Browse => vec![
            Span::styled(" /", Style::default().fg(Color::Yellow)),
            Span::styled(" search ", Style::default().fg(Color::DarkGray)),
            Span::styled("j/k", Style::default().fg(Color::Yellow)),
            Span::styled(" nav ", Style::default().fg(Color::DarkGray)),
            Span::styled("Enter", Style::default().fg(Color::Yellow)),
            Span::styled(" play ", Style::default().fg(Color::DarkGray)),
            Span::styled("Space", Style::default().fg(Color::Yellow)),
            Span::styled(" pause ", Style::default().fg(Color::DarkGray)),
            Span::styled("n/p", Style::default().fg(Color::Yellow)),
            Span::styled(" page ", Style::default().fg(Color::DarkGray)),
            Span::styled("g/G", Style::default().fg(Color::Yellow)),
            Span::styled(" top/bot ", Style::default().fg(Color::DarkGray)),
            Span::styled("q", Style::default().fg(Color::Yellow)),
            Span::styled(" quit", Style::default().fg(Color::DarkGray)),
        ],
        Mode::Input => vec![
            Span::styled(" Enter", Style::default().fg(Color::Yellow)),
            Span::styled(" submit ", Style::default().fg(Color::DarkGray)),
            Span::styled("Esc", Style::default().fg(Color::Yellow)),
            Span::styled(" cancel ", Style::default().fg(Color::DarkGray)),
            Span::styled("↑↓", Style::default().fg(Color::Yellow)),
            Span::styled(" history ", Style::default().fg(Color::DarkGray)),
            Span::styled("Ctrl+A/E", Style::default().fg(Color::Yellow)),
            Span::styled(" home/end ", Style::default().fg(Color::DarkGray)),
            Span::styled("Ctrl+U", Style::default().fg(Color::Yellow)),
            Span::styled(" clear", Style::default().fg(Color::DarkGray)),
        ],
    };

    let paragraph = Paragraph::new(Line::from(hints));
    frame.render_widget(paragraph, area);
}
