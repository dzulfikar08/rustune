use ratatui::{
    layout::Alignment,
    text::{Line, Span},
    widgets::{Block, BorderType, HighlightSpacing, List, ListItem, Padding},
    Frame,
};

use crate::app::{App, Status};
use crate::media::SourceKind;

pub fn render(frame: &mut Frame, area: ratatui::layout::Rect, app: &mut App) {
    let theme = &app.theme;

    if app.results.is_empty() {
        let msg = match &app.status {
            Status::Searching(text) => text.clone(),
            Status::Scanning(text) => text.clone(),
            Status::Error(text) => text.clone(),
            _ => match (app.active_source.clone(), app.input_history.is_empty()) {
                (SourceKind::Local, true) => "No music files found. Press Tab to switch to online search.".to_string(),
                (SourceKind::Local, false) => "No results found.".to_string(),
                (SourceKind::Extractor(_), true) => "Press / to search".to_string(),
                (SourceKind::Extractor(_), false) => "No results found.".to_string(),
            },
        };
        let style = match &app.status {
            Status::Searching(_) => theme.searching_text,
            Status::Scanning(_) => theme.scanning_text,
            Status::Error(_) => theme.error_text,
            _ => theme.empty_text,
        };

        let source_label = match app.active_source {
            SourceKind::Local => "Local Music",
            SourceKind::Extractor(ref name) => name.as_str(),
        };

        let title = if app.page > 0 {
            format!(" {source_label} (page {}) ", app.page + 1)
        } else {
            format!(" {source_label} ")
        };

        let block = Block::bordered()
            .title(title)
            .title_style(theme.page_nav_active)
            .border_style(theme.result_border)
            .padding(Padding::horizontal(1));

        let paragraph = ratatui::widgets::Paragraph::new(msg).style(style).block(block);
        frame.render_widget(paragraph, area);
        return;
    }

    let page_nav = if app.results.len() == app.config.page_size || app.page > 0 {
        let prev_style = if app.page > 0 {
            theme.page_nav_active
        } else {
            theme.page_nav_inactive
        };
        let next_style = if app.results.len() == app.config.page_size {
            theme.page_nav_active
        } else {
            theme.page_nav_inactive
        };

        Some(
            Line::from(vec![
                Span::styled(" ", ratatui::style::Style::default()),
                Span::styled("\u{25C0}", prev_style),
                Span::styled(" ", ratatui::style::Style::default()),
                Span::styled("\u{25B6}", next_style),
                Span::styled(" ", ratatui::style::Style::default()),
            ])
            .alignment(Alignment::Right),
        )
    } else {
        None
    };

    let items: Vec<ListItem> = app
        .results
        .iter()
        .enumerate()
        .map(|(i, result)| {
            let duration = result
                .duration
                .map(|d| App::format_duration(d))
                .unwrap_or_else(|| "---".to_string());

            let subtitle = result
                .subtitle
                .as_deref()
                .map(|c| format!(" - {c}"))
                .unwrap_or_default();

            let title = Line::from(vec![
                Span::styled(format!(" {:>2}. ", i + 1), theme.result_index),
                Span::styled(format!("{}{subtitle}", result.title), theme.result_title),
                Span::styled(format!("  {}", duration), theme.result_duration),
            ]);
            ListItem::new(title)
        })
        .collect();

    let source_label = match app.active_source {
        SourceKind::Local => "Local Music",
        SourceKind::Extractor(ref name) => name.as_str(),
    };

    let title = if app.page > 0 {
        format!(" {source_label} (page {}) ", app.page + 1)
    } else {
        format!(" {source_label} ")
    };

    let mut block = Block::bordered()
        .title(title)
        .title_style(theme.page_nav_active)
        .border_style(theme.result_border)
        .border_type(BorderType::Rounded);

    if let Some(nav) = page_nav {
        block = block.title(nav);
    }

    let list = List::new(items)
        .block(block)
        .highlight_style(theme.result_selected)
        .highlight_symbol(">>")
        .highlight_spacing(HighlightSpacing::Always);

    frame.render_stateful_widget(list, area, &mut app.list_state);
}
