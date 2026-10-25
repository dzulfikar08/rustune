use ratatui::{
    text::{Line, Span},
    widgets::Paragraph,
    Frame,
};

use crate::app::{App, InputPurpose, Mode};
use crate::media::SourceKind;

/// Key hints for the current mode, as (key, description) pairs.
fn hints(app: &App) -> Vec<(&'static str, &'static str)> {
    match app.mode {
        Mode::Browse if app.show_playlist => vec![
            ("j/k", "move"),
            ("Enter", "play"),
            ("J/K", "reorder"),
            ("D", "remove"),
            ("z x c v b", "transport"),
            ("w", "save"),
            ("P", "library"),
            ("q", "quit"),
        ],
        Mode::Browse => {
            let mut v = vec![
                ("/", "search"),
                ("Enter", "play"),
                ("a/A", "queue/next"),
                ("z x c v b", "prev play pause stop next"),
                ("←/→", "seek"),
                ("+/-", "vol"),
                ("S/R", "shuffle/repeat"),
                ("P", "playlist"),
            ];
            if matches!(app.active_source, SourceKind::Extractor(_)) {
                v.push(("n/p", "page"));
                v.push(("d", "download"));
            }
            v.extend([("Tab", "source"), ("s", "settings"), ("q", "quit")]);
            v
        }
        Mode::Input => match app.input_purpose {
            InputPurpose::Search => vec![
                ("Enter", "submit"),
                ("Esc", "cancel"),
                ("↑↓", "history"),
                (":", "command (:q :clear :eq :vol :seek …)"),
            ],
            InputPurpose::SavePlaylist => vec![("Enter", "save playlist"), ("Esc", "cancel")],
            InputPurpose::Open => vec![("Enter", "play file/dir/URL/playlist"), ("Esc", "cancel")],
        },
        Mode::Settings => vec![("j/k", "navigate"), ("Enter", "change"), ("Esc", "back")],
        Mode::Onboarding => vec![("Enter", "next"), ("Esc", "skip")],
        Mode::SkinBrowser => vec![
            ("j/k", "nav"),
            ("Enter", "select"),
            ("n", "more"),
            ("/", "search"),
            ("Esc", "back"),
        ],
    }
}

pub fn render(frame: &mut Frame, area: ratatui::layout::Rect, app: &App) {
    let theme = &app.theme;
    let mut spans = Vec::new();
    for (key, desc) in hints(app) {
        spans.push(Span::styled(format!(" {key}"), theme.help_key));
        spans.push(Span::styled(format!(" {desc}"), theme.help_desc));
    }
    frame.render_widget(Paragraph::new(Line::from(spans)), area);
}
