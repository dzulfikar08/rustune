use ratatui::{
    layout::Rect,
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::{Block, BorderType, List, ListItem, Paragraph},
    Frame,
};

use crate::app::{App, SkinBrowserSource};

pub fn render(frame: &mut Frame, area: Rect, app: &App) {
    let theme = &app.theme;

    let title = match app.skin_browser_source {
        SkinBrowserSource::Local => " Local Skins ",
        SkinBrowserSource::Online => " Online Skins ",
    };

    let outer = Block::bordered()
        .title(title)
        .title_style(Style::default()
            .fg(ratatui::style::Color::Cyan)
            .add_modifier(Modifier::BOLD))
        .border_type(BorderType::Rounded)
        .border_style(theme.result_border);

    let inner = outer.inner(area);
    frame.render_widget(outer, area);

    // Header
    let header_text = if app.skin_browser_loading {
        "Loading skins...".to_string()
    } else if let Some(ref err) = app.skin_browser_error {
        format!("Error: {err}")
    } else if app.skin_entries.is_empty() {
        "No skins found".to_string()
    } else {
        format!(
            "{} skins loaded ({} total available) | offset: {}",
            app.skin_entries.len(),
            app.skin_total_count,
            app.skin_browser_offset,
        )
    };

    let header = Paragraph::new(Line::from(vec![
        Span::styled(" ", theme.help_key),
        Span::styled(&header_text, theme.result_title),
    ]));
    frame.render_widget(header, Rect {
        x: inner.x,
        y: inner.y,
        width: inner.width,
        height: 1,
    });

    let list_area = Rect {
        x: inner.x,
        y: inner.y + 1,
        width: inner.width,
        height: inner.height.saturating_sub(2),
    };

    let items: Vec<ListItem> = app
        .skin_entries
        .iter()
        .map(|entry| {
            let local_badge = if entry.is_local {
                Span::styled(
                    " [local] ",
                    Style::default()
                        .fg(ratatui::style::Color::Green)
                        .add_modifier(Modifier::BOLD),
                )
            } else {
                Span::raw("")
            };

            let downloading = app
                .skin_downloading_md5
                .as_ref()
                .map_or(false, |md5| md5 == &entry.md5);

            let dl_indicator = if downloading {
                Span::styled(
                    " downloading...",
                    Style::default().fg(ratatui::style::Color::Yellow),
                )
            } else {
                Span::raw("")
            };

            let name = if entry.display_name.is_empty() {
                entry.filename.clone()
            } else {
                entry.display_name.clone()
            };

            ListItem::new(Line::from(vec![
                local_badge,
                Span::styled(name, theme.result_title),
                dl_indicator,
            ]))
        })
        .collect();

    let list = List::new(items)
        .highlight_style(
            Style::default()
                .bg(theme.result_selected.bg.unwrap_or(ratatui::style::Color::DarkGray))
                .add_modifier(Modifier::BOLD),
        )
        .highlight_symbol(">> ");

    frame.render_stateful_widget(list, list_area, &mut app.skin_list_state.clone());

    // Footer
    let mut footer_spans = vec![
        Span::styled(" j/k", theme.help_key),
        Span::styled(" nav ", theme.help_desc),
        Span::styled("Enter", theme.help_key),
    ];
    match app.skin_browser_source {
        SkinBrowserSource::Local => {
            footer_spans.push(Span::styled(" apply ", theme.help_desc));
        }
        SkinBrowserSource::Online => {
            footer_spans.push(Span::styled(" download ", theme.help_desc));
            footer_spans.push(Span::styled("n", theme.help_key));
            footer_spans.push(Span::styled(" next page ", theme.help_desc));
        }
    }
    footer_spans.push(Span::styled("Esc", theme.help_key));
    footer_spans.push(Span::styled(" back", theme.help_desc));

    let footer = Paragraph::new(Line::from(footer_spans));

    let footer_area = Rect {
        x: inner.x,
        y: inner.y + inner.height.saturating_sub(1),
        width: inner.width,
        height: 1,
    };
    frame.render_widget(footer, footer_area);
}
