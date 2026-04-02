use ratatui::{
    layout::{Alignment, Constraint, Direction, Layout, Rect},
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::{Block, BorderType, Paragraph},
    Frame,
};

use crate::app::{App, SettingsField};

pub fn render(frame: &mut Frame, area: Rect, app: &App) {
    let theme = &app.theme;

    let outer = Block::bordered()
        .title(" Settings ")
        .title_style(Style::default().fg(ratatui::style::Color::Cyan).add_modifier(Modifier::BOLD))
        .border_type(BorderType::Rounded)
        .border_style(theme.result_border);

    let inner = outer.inner(area);
    frame.render_widget(outer, area);

    let fields = [
        (
            "Music Directory",
            app.config.music_dir.to_string_lossy().to_string(),
            SettingsField::MusicDir,
        ),
        (
            "Extensions",
            app.config.extensions.join(", "),
            SettingsField::Extensions,
        ),
        (
            "Theme",
            app.config.theme.clone(),
            SettingsField::Theme,
        ),
        (
            "Page Size",
            app.config.page_size.to_string(),
            SettingsField::PageSize,
        ),
        (
            "Extractor",
            if app.config.extractor.is_empty() {
                "auto".into()
            } else {
                app.config.extractor.clone()
            },
            SettingsField::Extractor,
        ),
    ];

    let mut lines = vec![Line::from("")];

    for (label, value, field) in &fields {
        let is_selected = app.settings_field == *field;
        let prefix = if is_selected { " >> " } else { "    " };
        let label_style = if is_selected {
            Style::default().add_modifier(Modifier::BOLD)
        } else {
            Style::default()
        };

        lines.push(Line::from(vec![
            Span::styled(prefix, theme.help_key),
            Span::styled(
                format!("{label}: "),
                label_style.fg(theme.help_key.fg.unwrap_or(ratatui::style::Color::Yellow)),
            ),
            Span::styled(value.clone(), theme.result_title),
        ]));
    }

    lines.push(Line::from(""));
    lines.push(Line::from(Span::styled(
        "  j/k: navigate  Enter: change  Esc: back",
        theme.help_desc,
    )));

    // Theme preview hint
    if app.settings_field == SettingsField::Theme {
        if app.theme.name == "Winamp" {
            lines.push(Line::from(Span::styled(
                "  (i: local skins  o: online skins)",
                theme.help_desc,
            )));
        } else {
            lines.push(Line::from(Span::styled(
                "  (Enter cycles: Dark -> Light -> Winamp)",
                theme.help_desc,
            )));
        }
    }

    let content = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Min(3),
            Constraint::Length(1), // hint line at bottom
        ])
        .split(inner);

    frame.render_widget(
        Paragraph::new(lines).alignment(Alignment::Left),
        content[0],
    );
}
