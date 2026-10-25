use ratatui::{
    layout::{Alignment, Rect},
    style::Modifier,
    text::{Line, Span},
    widgets::{Block, BorderType, HighlightSpacing, List, ListItem, ListState, Padding},
    Frame,
};

use crate::app::{App, Status, Tab};
use crate::media::MediaItem;
use crate::theme::Theme;

/// Keep `selected` visible in a list of `len` rows shown `height` at a time;
/// returns the visible index range and stores the scroll offset in `state`
/// (mouse hit-testing reads it back).
fn visible_window(
    state: &mut ListState,
    selected: usize,
    len: usize,
    height: usize,
) -> (usize, usize) {
    let height = height.max(1);
    let mut offset = state.offset().min(len.saturating_sub(height));
    if selected < offset {
        offset = selected;
    } else if selected >= offset + height {
        offset = selected + 1 - height;
    }
    *state.offset_mut() = offset;
    (offset, (offset + height).min(len))
}

fn row<'a>(theme: &Theme, index: usize, item: &'a MediaItem, marker: &'a str) -> ListItem<'a> {
    let duration = item
        .duration
        .map_or_else(|| "---".to_string(), App::format_duration);
    let mut spans = vec![
        Span::styled(format!("{marker}{:>3}. ", index + 1), theme.result_index),
        Span::styled(item.title.as_str(), theme.result_title),
    ];
    if let Some(sub) = item.subtitle.as_deref() {
        spans.push(Span::styled(" - ", theme.result_title));
        spans.push(Span::styled(sub, theme.result_title));
    }
    spans.push(Span::styled(format!("  {duration}"), theme.result_duration));
    ListItem::new(Line::from(spans))
}

fn render_list(
    frame: &mut Frame,
    area: Rect,
    block: Block,
    items: Vec<ListItem>,
    selected: Option<usize>,
    theme: &Theme,
) {
    let list = List::new(items)
        .block(block)
        .highlight_style(theme.result_selected)
        .highlight_symbol(">>")
        .highlight_spacing(HighlightSpacing::Always);
    let mut state = ListState::default().with_selected(selected);
    frame.render_stateful_widget(list, area, &mut state);
}

pub fn render(frame: &mut Frame, area: Rect, app: &mut App) {
    let theme = &app.theme;

    let source_label = match app.tab {
        Tab::Local => "Local Music",
        Tab::Online => "Online (yt-dlp)",
        Tab::Provider(kind) => kind.display_name(),
    };
    let title = if app.page > 0 {
        format!(" {source_label} (page {}) ", app.page + 1)
    } else {
        format!(" {source_label} ")
    };

    if app.results.is_empty() {
        let (msg, style) = match &app.status {
            Status::Searching(text) => (text.as_str(), theme.searching_text),
            Status::Scanning(text) => (text.as_str(), theme.scanning_text),
            Status::Downloading(text) => (text.as_str(), theme.loading_text),
            Status::Error(text) => (text.as_str(), theme.error_text),
            _ => {
                let msg = match (app.tab, app.input_history.is_empty()) {
                    (Tab::Local, true) => {
                        "No music files found. Press Tab for online search, radio and podcasts."
                    }
                    (_, true) => "Press / to search",
                    (_, false) => "No results found.",
                };
                (msg, theme.empty_text)
            }
        };
        let block = Block::bordered()
            .title(title)
            .title_style(theme.page_nav_active)
            .border_style(theme.result_border)
            .padding(Padding::horizontal(1));
        frame.render_widget(
            ratatui::widgets::Paragraph::new(msg)
                .style(style)
                .block(block),
            area,
        );
        return;
    }

    let full_page = app.results.len() == app.config.page_size;
    let mut block = Block::bordered()
        .title(title)
        .title_style(theme.page_nav_active)
        .border_style(theme.result_border)
        .border_type(BorderType::Rounded);
    if full_page || app.page > 0 {
        let style = |active: bool| {
            if active {
                theme.page_nav_active
            } else {
                theme.page_nav_inactive
            }
        };
        block = block.title(
            Line::from(vec![
                Span::raw(" "),
                Span::styled("\u{25C0}", style(app.page > 0)),
                Span::raw(" "),
                Span::styled("\u{25B6}", style(full_page)),
                Span::raw(" "),
            ])
            .alignment(Alignment::Right),
        );
    }

    let selected = app.selected_index().unwrap_or(0);
    let height = usize::from(area.height.saturating_sub(2));
    let (start, end) = visible_window(&mut app.list_state, selected, app.results.len(), height);
    let items: Vec<ListItem> = app.results[start..end]
        .iter()
        .enumerate()
        .map(|(i, item)| row(theme, start + i, item, ""))
        .collect();
    render_list(frame, area, block, items, Some(selected - start), theme);
}

/// The play queue (Winamp playlist window).
pub fn render_queue(frame: &mut Frame, area: Rect, app: &mut App) {
    let theme = &app.theme;
    let queue = &app.queue;
    let flags = format!(
        " Playlist ({}){}{} ",
        queue.len(),
        if queue.shuffle() { " · shuffle" } else { "" },
        match queue.repeat() {
            crate::view::RepeatMode::Off => "",
            crate::view::RepeatMode::All => " · repeat all",
            crate::view::RepeatMode::One => " · repeat one",
        }
    );
    let block = Block::bordered()
        .title(flags)
        .title_style(theme.page_nav_active)
        .border_style(theme.result_border)
        .border_type(BorderType::Rounded);

    if queue.is_empty() {
        frame.render_widget(
            ratatui::widgets::Paragraph::new(
                "Queue is empty — Enter plays the result list, a appends, A plays next, P goes back.",
            )
            .style(theme.empty_text)
            .block(block.padding(Padding::horizontal(1))),
            area,
        );
        return;
    }

    let cursor = queue.cursor();
    let current = queue.current_index();
    let height = usize::from(area.height.saturating_sub(2));
    let (start, end) = visible_window(&mut app.queue_list_state, cursor, queue.len(), height);
    let playing = theme.result_title.add_modifier(Modifier::BOLD);
    let items: Vec<ListItem> = queue.items()[start..end]
        .iter()
        .enumerate()
        .map(|(i, item)| {
            let index = start + i;
            if Some(index) == current {
                row(theme, index, item, "\u{25B6}").style(playing)
            } else {
                row(theme, index, item, " ")
            }
        })
        .collect();
    render_list(frame, area, block, items, Some(cursor - start), theme);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn window_follows_selection() {
        let mut state = ListState::default();
        assert_eq!(visible_window(&mut state, 0, 100, 10), (0, 10));
        assert_eq!(visible_window(&mut state, 15, 100, 10), (6, 16));
        assert_eq!(state.offset(), 6);
        assert_eq!(
            visible_window(&mut state, 8, 100, 10),
            (6, 16),
            "no scroll inside"
        );
        assert_eq!(visible_window(&mut state, 2, 100, 10), (2, 12));
        assert_eq!(visible_window(&mut state, 99, 100, 10), (90, 100));
        assert_eq!(
            visible_window(&mut state, 3, 5, 10),
            (0, 5),
            "short lists start at 0"
        );
        assert_eq!(visible_window(&mut state, 0, 1, 0), (0, 1));
    }
}
