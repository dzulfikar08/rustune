use ratatui::{
    text::{Line, Span},
    widgets::Paragraph,
    Frame,
};

use crate::app::{App, InputPurpose, Mode, Tab};

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
        Mode::Browse if app.show_eq => vec![
            ("←/→", "band"),
            ("↑/↓", "±1 dB"),
            ("0", "reset"),
            ("o", "on/off"),
            ("[ ]", "preset"),
            ("e", "close"),
            ("q", "quit"),
        ],
        Mode::Browse => {
            let mut v = vec![("/", "search"), ("Enter", "play"), ("a/A", "queue/next")];
            match app.tab {
                Tab::Online => v.extend([("n/p", "page"), ("d", "download")]),
                Tab::Provider(_) => v.extend([("⌫", "back"), ("r", "reload")]),
                Tab::Local => {}
            }
            if app.tab == Tab::Provider(crate::providers::ProviderKind::Podcast) {
                v.push(("f", "subscribe"));
            }
            v.extend([
                ("z x c v b", "transport"),
                ("←/→", "seek"),
                ("+/-", "vol"),
                ("P", "playlist"),
                ("e", "EQ"),
                ("y", "lyrics"),
                ("V", "vis"),
                ("Tab", "source"),
                ("s", "settings"),
                ("q", "quit"),
            ]);
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
            InputPurpose::ProviderSearch => vec![("Enter", "search"), ("Esc", "cancel")],
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
