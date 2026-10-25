use ratatui::{
    layout::Rect,
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::{Block, BorderType, HighlightSpacing, List, ListItem, ListState, Padding, Paragraph},
    Frame,
};

use crate::browse::{Browser, Entry, LevelState};
use crate::theme::Theme;

fn row<'a>(theme: &Theme, entry: &'a Entry, subscribed: bool) -> ListItem<'a> {
    let mut spans = Vec::with_capacity(4);
    if subscribed {
        spans.push(Span::styled("★ ", theme.page_nav_active));
    }
    spans.push(Span::styled(entry.label(), theme.result_title));
    if !matches!(entry, Entry::Track { .. }) {
        spans.push(Span::styled(" ›", theme.result_duration));
    }
    if let Some(detail) = entry.detail() {
        spans.push(Span::styled(
            format!("  {detail}"),
            theme.result_duration.add_modifier(Modifier::DIM),
        ));
    }
    ListItem::new(Line::from(spans))
}

fn notice(frame: &mut Frame, area: Rect, block: Block, text: &str, style: Style) {
    frame.render_widget(
        Paragraph::new(text)
            .style(style)
            .block(block.padding(Padding::horizontal(1))),
        area,
    );
}

/// The provider browser: breadcrumb title, the current level's rows.
pub fn render(frame: &mut Frame, area: Rect, browser: &mut Browser, theme: &Theme) {
    let block = Block::bordered()
        .border_type(BorderType::Rounded)
        .title(format!(" {} ", browser.title()))
        .title_style(theme.page_nav_active)
        .border_style(theme.result_border);

    match browser.state() {
        LevelState::Loading => {
            notice(frame, area, block, "Loading…", theme.loading_text);
            return;
        }
        LevelState::Error(msg) => {
            notice(frame, area, block, msg, theme.error_text);
            return;
        }
        LevelState::Ready if browser.is_empty() => {
            notice(frame, area, block, "Nothing here", theme.empty_text);
            return;
        }
        LevelState::Ready => {}
    }

    let height = usize::from(area.height.saturating_sub(2));
    let (start, end) = browser.window(height);
    let selected = browser.selected();
    let items: Vec<ListItem> = browser.entries()[start..end]
        .iter()
        .map(|entry| row(theme, entry, browser.subscribed(entry)))
        .collect();
    let list = List::new(items)
        .block(block)
        .highlight_style(theme.result_selected)
        .highlight_symbol(">>")
        .highlight_spacing(HighlightSpacing::Always);
    let mut state = ListState::default().with_selected(Some(selected - start));
    frame.render_stateful_widget(list, area, &mut state);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::browse::BrowseOutcome;
    use crate::providers::{ProviderError, ProviderKind};
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    use ratatui::{backend::TestBackend, Terminal};

    fn draw(browser: &mut Browser, w: u16, h: u16) -> Terminal<TestBackend> {
        let theme = Theme::dark();
        let mut terminal = Terminal::new(TestBackend::new(w, h)).unwrap();
        terminal
            .draw(|f| render(f, f.area(), browser, &theme))
            .unwrap();
        terminal
    }

    fn row_text(terminal: &Terminal<TestBackend>, y: u16) -> String {
        let buf = terminal.backend().buffer();
        (0..buf.area.width).map(|x| buf[(x, y)].symbol()).collect()
    }

    fn open_first(browser: &mut Browser) -> u64 {
        match browser.handle_key(&KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE), &[]) {
            BrowseOutcome::Fetch { token, .. } => token,
            other => panic!("expected Fetch, got {other:?}"),
        }
    }

    #[test]
    fn root_menu_with_breadcrumb_and_selection() {
        let mut browser = Browser::new(ProviderKind::Radio);
        let terminal = draw(&mut browser, 40, 10);
        assert!(row_text(&terminal, 0).contains("Radio"));
        assert!(row_text(&terminal, 1).contains(">>Top voted ›"));
        assert!(row_text(&terminal, 6).contains("Search…"));
    }

    #[test]
    fn states_show_messages() {
        let mut browser = Browser::new(ProviderKind::Radio);
        let token = open_first(&mut browser);
        let terminal = draw(&mut browser, 40, 5);
        assert!(row_text(&terminal, 0).contains("Radio › Top voted"));
        assert!(row_text(&terminal, 1).contains("Loading…"));

        browser.loaded(token, Ok(Vec::new()));
        assert!(row_text(&draw(&mut browser, 40, 5), 1).contains("Nothing here"));

        browser.handle_key(&KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE), &[]);
        let token = open_first(&mut browser);
        browser.loaded(token, Err(ProviderError::Status(503)));
        assert!(row_text(&draw(&mut browser, 40, 5), 1).contains("HTTP 503"));
    }

    #[test]
    fn selection_stays_visible() {
        let mut browser = Browser::new(ProviderKind::Radio);
        for _ in 0..5 {
            browser.scroll(true);
        }
        // 3 visible rows: the selected 6th entry is the last of them.
        let terminal = draw(&mut browser, 40, 5);
        assert_eq!(browser.offset(), 3);
        assert!(row_text(&terminal, 3).contains(">>Search…"));
        assert!(row_text(&terminal, 1).contains("By country"));
    }
}
