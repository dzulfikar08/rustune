use ratatui::{
    text::{Line, Span},
    widgets::Paragraph,
    Frame,
};

use crate::app::Mode;
use crate::theme::Theme;

pub fn render(frame: &mut Frame, area: ratatui::layout::Rect, mode: &Mode, theme: &Theme) {
    let hints = match mode {
        Mode::Browse => vec![
            Span::styled(" /", theme.help_key),
            Span::styled(" search ", theme.help_desc),
            Span::styled("j/k", theme.help_key),
            Span::styled(" nav ", theme.help_desc),
            Span::styled("Enter", theme.help_key),
            Span::styled(" play ", theme.help_desc),
            Span::styled("Space", theme.help_key),
            Span::styled(" pause ", theme.help_desc),
            Span::styled("Tab", theme.help_key),
            Span::styled(" source ", theme.help_desc),
            Span::styled("S", theme.help_key),
            Span::styled(" settings ", theme.help_desc),
            Span::styled("q", theme.help_key),
            Span::styled(" quit", theme.help_desc),
        ],
        Mode::Input => vec![
            Span::styled(" Enter", theme.help_key),
            Span::styled(" submit ", theme.help_desc),
            Span::styled("Esc", theme.help_key),
            Span::styled(" cancel ", theme.help_desc),
            Span::styled("\u{2191}\u{2193}", theme.help_key),
            Span::styled(" history ", theme.help_desc),
            Span::styled("Ctrl+A/E", theme.help_key),
            Span::styled(" home/end ", theme.help_desc),
            Span::styled("Ctrl+U", theme.help_key),
            Span::styled(" clear", theme.help_desc),
        ],
        Mode::Settings => vec![
            Span::styled(" j/k", theme.help_key),
            Span::styled(" navigate ", theme.help_desc),
            Span::styled("Enter", theme.help_key),
            Span::styled(" change ", theme.help_desc),
            Span::styled("Esc", theme.help_key),
            Span::styled(" back", theme.help_desc),
        ],
        Mode::Onboarding => vec![
            Span::styled(" Enter", theme.help_key),
            Span::styled(" next ", theme.help_desc),
            Span::styled("Esc", theme.help_key),
            Span::styled(" skip ", theme.help_desc),
        ],
        Mode::SkinBrowser => vec![
            Span::styled(" j/k", theme.help_key),
            Span::styled(" nav ", theme.help_desc),
            Span::styled("Enter", theme.help_key),
            Span::styled(" select ", theme.help_desc),
            Span::styled("n", theme.help_key),
            Span::styled(" next ", theme.help_desc),
            Span::styled("Esc", theme.help_key),
            Span::styled(" back", theme.help_desc),
        ],
    };

    let paragraph = Paragraph::new(Line::from(hints));
    frame.render_widget(paragraph, area);
}
