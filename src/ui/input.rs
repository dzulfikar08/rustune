use ratatui::{
    text::{Line, Span},
    widgets::Paragraph,
    Frame,
};

use crate::app::{App, InputPurpose, Mode};

pub fn render(frame: &mut Frame, area: ratatui::layout::Rect, app: &App) {
    let theme = &app.theme;

    let prompt = match (&app.mode, app.input_purpose) {
        (Mode::Input, InputPurpose::Search) => " / ",
        (Mode::Input, InputPurpose::SavePlaylist) => " save as: ",
        (Mode::Input, InputPurpose::Open) => " open: ",
        (Mode::Browse, _) => " > ",
        (Mode::Settings, _) => " S ",
        (Mode::Onboarding, _) => " ? ",
        (Mode::SkinBrowser, _) => " W ",
    };

    let mut spans = vec![Span::styled(prompt, theme.input_prompt)];
    if app.mode == Mode::Input {
        spans.push(Span::styled(app.input_text.as_str(), theme.input_text));
        spans.push(Span::styled(theme.input_cursor.as_str(), theme.input_text));
    } else {
        spans.push(Span::styled(app.input_text.as_str(), theme.input_text));
    }

    frame.render_widget(Paragraph::new(Line::from(spans)), area);
}
