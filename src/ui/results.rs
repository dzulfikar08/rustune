use ratatui::{
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{HighlightSpacing, List, ListItem},
    Frame,
};

use crate::app::{App, Status};

pub fn render(frame: &mut Frame, area: ratatui::layout::Rect, app: &mut App) {
    if app.results.is_empty() {
        let msg = match &app.status {
            Status::Searching(text) => text.clone(),
            Status::Error(text) => text.clone(),
            _ => match app.input_history.is_empty() {
                true => "Press / to search YouTube".to_string(),
                false => "No results found.".to_string(),
            },
        };
        let style = match &app.status {
            Status::Searching(_) => Style::default().fg(Color::Yellow),
            Status::Error(_) => Style::default().fg(Color::Red),
            _ => Style::default().fg(Color::DarkGray),
        };
        let paragraph = ratatui::widgets::Paragraph::new(msg).style(style);
        frame.render_widget(paragraph, area);
        return;
    }

    let items: Vec<ListItem> = app
        .results
        .iter()
        .enumerate()
        .map(|(i, result)| {
            let duration = result
                .duration
                .map(|d| App::format_duration(d))
                .unwrap_or_else(|| "LIVE".to_string());

            let channel = result
                .channel
                .as_deref()
                .map(|c| format!(" - {c}"))
                .unwrap_or_default();

            let title = Line::from(vec![
                Span::styled(
                    format!(" {:>2}. ", i + 1),
                    Style::default().fg(Color::DarkGray),
                ),
                Span::styled(
                    format!("{}{channel}", result.title),
                    Style::default().fg(Color::White),
                ),
                Span::styled(
                    format!("  {}", duration),
                    Style::default().fg(Color::DarkGray),
                ),
            ]);
            ListItem::new(title)
        })
        .collect();

    let list = List::new(items)
        .highlight_style(
            Style::default()
                .fg(Color::Yellow)
                .bg(Color::DarkGray)
                .add_modifier(Modifier::BOLD),
        )
        .highlight_symbol(">>")
        .highlight_spacing(HighlightSpacing::Always);

    frame.render_stateful_widget(list, area, &mut app.list_state);
}
