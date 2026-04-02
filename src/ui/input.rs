use ratatui::{
    text::{Line, Span},
    widgets::Paragraph,
    Frame,
};

use crate::app::{App, Mode};

pub fn render(frame: &mut Frame, area: ratatui::layout::Rect, app: &App) {
    let theme = &app.theme;

    let prompt = match app.mode {
        Mode::Browse => " > ",
        Mode::Input => " / ",
        Mode::Settings => " S ",
        Mode::Onboarding => " ? ",
        Mode::SkinBrowser => " W ",
    };

    let mut spans = vec![
        Span::styled(prompt, theme.input_prompt),
        Span::styled(&app.input_text, theme.input_text),
    ];

    if app.mode == Mode::Input {
        spans.push(Span::styled(
            theme.input_cursor.clone(),
            theme.input_text,
        ));
    }

    let line = Line::from(spans);
    let paragraph = Paragraph::new(line);
    frame.render_widget(paragraph, area);
}
