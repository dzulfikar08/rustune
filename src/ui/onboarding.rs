use ratatui::{
    layout::{Alignment, Constraint, Direction, Layout, Rect},
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::{Block, BorderType, Paragraph},
    Frame,
};

use crate::app::{App, OnboardingStep};
use crate::theme::Theme;

pub fn render(frame: &mut Frame, area: Rect, app: &App) {
    let theme = &app.theme;

    let outer = Block::bordered()
        .title(" rustune setup ")
        .title_style(Style::default().fg(ratatui::style::Color::Cyan).add_modifier(Modifier::BOLD))
        .border_type(BorderType::Rounded)
        .border_style(theme.result_border);

    let inner = outer.inner(area);
    frame.render_widget(outer, area);

    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(2), // title + spacer
            Constraint::Min(5),    // content
            Constraint::Length(2), // step indicator + spacer
        ])
        .split(inner);

    match app.onboarding_step {
        OnboardingStep::Welcome => render_welcome(frame, chunks[1], theme),
        OnboardingStep::Dependencies => render_dependencies(frame, chunks[1], theme),
        OnboardingStep::MusicDir => render_music_dir(frame, chunks[1], app, theme),
        OnboardingStep::Theme => render_theme_selection(frame, chunks[1], app, theme),
    }

    // Step indicator at bottom
    let step_num = match app.onboarding_step {
        OnboardingStep::Welcome => 1,
        OnboardingStep::Dependencies => 2,
        OnboardingStep::MusicDir => 3,
        OnboardingStep::Theme => 4,
    };

    let steps = format!("[ {step_num} / 4 ]");
    let step_line = Line::from(Span::styled(
        steps,
        theme.page_nav_active,
    ));
    frame.render_widget(
        Paragraph::new(step_line).alignment(Alignment::Center),
        chunks[2],
    );
}

fn render_welcome(frame: &mut Frame, area: Rect, theme: &Theme) {
    let lines = vec![
        Line::from(""),
        Line::from(Span::styled(
            "  rustune",
            Style::default().add_modifier(Modifier::BOLD).fg(ratatui::style::Color::Cyan),
        )),
        Line::from(Span::styled(
            "  A lightweight terminal music player",
            theme.empty_text,
        )),
        Line::from(""),
        Line::from(Span::styled(
            "  Press Enter to get started...",
            theme.help_key,
        )),
    ];

    frame.render_widget(Paragraph::new(lines), area);
}

fn render_dependencies(frame: &mut Frame, area: Rect, theme: &Theme) {
    // Check mpv status
    let mpv_status = match std::process::Command::new("mpv").arg("--version").output() {
        Ok(o) if o.status.success() => {
            let v = String::from_utf8_lossy(&o.stdout);
            let version = v.lines().next().unwrap_or("mpv");
            (true, version.to_string())
        }
        _ => (false, "Not found".into()),
    };

    // Check yt-dlp status
    let ytdlp_status = match std::process::Command::new("yt-dlp").arg("--version").output() {
        Ok(o) if o.status.success() => {
            let v = String::from_utf8_lossy(&o.stdout).trim().to_string();
            (true, v)
        }
        _ => (false, "Not found".into()),
    };

    let check = |found: bool, name: &str, version: &str| -> Line<'static> {
        let (mark, style) = if found {
            ("\u{2713}", Style::default().fg(ratatui::style::Color::Green))
        } else {
            ("\u{2717}", theme.error_text)
        };
        Line::from(vec![
            Span::styled(format!("  {mark} "), style),
            Span::styled(format!("{name}: "), theme.result_title),
            Span::styled(version.to_string(), theme.result_duration),
        ])
    };

    let lines = vec![
        Line::from(""),
        Line::from(Span::styled(
            "  Dependency Check",
            Style::default().add_modifier(Modifier::BOLD).fg(ratatui::style::Color::Cyan),
        )),
        Line::from(""),
        check(mpv_status.0, "mpv (required)", &mpv_status.1),
        check(ytdlp_status.0, "yt-dlp (optional, for online search)", &ytdlp_status.1),
        Line::from(""),
        Line::from(Span::styled(
            "  Press Enter to continue...",
            theme.help_key,
        )),
    ];

    frame.render_widget(Paragraph::new(lines), area);
}

fn render_music_dir(frame: &mut Frame, area: Rect, app: &App, theme: &Theme) {
    let default_dir = app.config.music_dir.to_string_lossy().to_string();

    let lines = vec![
        Line::from(""),
        Line::from(Span::styled(
            "  Music Directory",
            Style::default().add_modifier(Modifier::BOLD).fg(ratatui::style::Color::Cyan),
        )),
        Line::from(""),
        Line::from(Span::styled(
            format!("  Default: {default_dir}"),
            theme.empty_text,
        )),
        Line::from(""),
        Line::from(vec![
            Span::styled("  Path: ", theme.help_key),
            Span::styled(&app.input_text, theme.input_text),
            Span::styled(theme.input_cursor.clone(), theme.input_text),
        ]),
        Line::from(""),
        Line::from(Span::styled(
            "  Enter to confirm, Esc to use default",
            theme.help_desc,
        )),
    ];

    frame.render_widget(Paragraph::new(lines), area);
}

fn render_theme_selection(frame: &mut Frame, area: Rect, _app: &App, theme: &Theme) {
    let builtins = Theme::builtins();

    let mut lines = vec![
        Line::from(""),
        Line::from(Span::styled(
            "  Select Theme (j/k to preview, Enter to confirm)",
            Style::default().add_modifier(Modifier::BOLD).fg(ratatui::style::Color::Cyan),
        )),
        Line::from(""),
    ];

    for t in &builtins {
        let is_selected = t.name == theme.name;
        let prefix = if is_selected { " >> " } else { "    " };
        let style = if is_selected {
            Style::default().add_modifier(Modifier::BOLD)
        } else {
            Style::default()
        };
        lines.push(Line::from(Span::styled(
            format!("{prefix}{}", t.name),
            style.fg(theme.page_nav_active.fg.unwrap_or(ratatui::style::Color::White)),
        )));
    }

    lines.push(Line::from(""));
    lines.push(Line::from(Span::styled(
        "  Press Enter to confirm",
        theme.help_key,
    )));

    frame.render_widget(Paragraph::new(lines), area);
}
