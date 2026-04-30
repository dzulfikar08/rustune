use ratatui::{
    text::{Line, Span},
    widgets::{LineGauge, Paragraph},
    Frame,
};

use crate::app::{App, PlaybackState, Status};

pub fn render(
    frame: &mut Frame,
    info_area: ratatui::layout::Rect,
    gauge_area: ratatui::layout::Rect,
    app: &App,
) {
    let theme = &app.theme;

    match &app.playback {
        Some(PlaybackState {
            title,
            duration_secs,
            elapsed_secs,
            paused,
        }) => {
            let elapsed_str = App::format_duration(*elapsed_secs);
            let duration_str = App::format_duration(*duration_secs);

            let (btn_text, btn_style) = if *paused {
                (" \u{25B6} ", theme.player_button_play)
            } else {
                (" \u{23F8} ", theme.player_button_pause)
            };

            let info = Line::from(vec![
                Span::styled(btn_text.to_string(), btn_style),
                Span::styled(" ", ratatui::style::Style::default()),
                Span::styled(truncate(title, 50), theme.player_title),
                Span::styled(
                    format!("  {elapsed_str}/{duration_str}"),
                    theme.player_time,
                ),
            ]);
            frame.render_widget(Paragraph::new(info), info_area);

            let ratio = if *duration_secs > 0 {
                *elapsed_secs as f64 / *duration_secs as f64
            } else {
                0.0
            };

            let pct_str = format!("{:.0}%", ratio * 100.0);
            let gauge = LineGauge::default()
                .ratio(ratio)
                .label(Span::styled(pct_str, theme.gauge_label))
                .filled_style(theme.gauge_filled)
                .unfilled_style(theme.gauge_unfilled)
                .line_set(ratatui::symbols::line::THICK);

            frame.render_widget(gauge, gauge_area);
        }
        None => match &app.status {
            Status::Loading(text) => {
                let info = Line::from(Span::styled(format!(" {text}"), theme.loading_text));
                frame.render_widget(Paragraph::new(info), info_area);
            }
            Status::Downloading(text) => {
                let info = Line::from(vec![
                    Span::styled(" \u{2B07} ", theme.loading_text),
                    Span::styled(truncate(text, 60), theme.loading_text),
                ]);
                frame.render_widget(Paragraph::new(info), info_area);

                let gauge = LineGauge::default()
                    .ratio(0.0)
                    .label(Span::styled("downloading...", theme.gauge_label))
                    .filled_style(theme.gauge_filled)
                    .unfilled_style(theme.gauge_unfilled)
                    .line_set(ratatui::symbols::line::THICK);
                frame.render_widget(gauge, gauge_area);
            }
            _ => {
                frame.render_widget(Paragraph::new(Line::from("")), info_area);
            }
        },
    }
}

fn truncate(s: &str, max_len: usize) -> String {
    if s.len() <= max_len {
        s.to_string()
    } else {
        let mut truncated: String = s.chars().take(max_len - 1).collect();
        truncated.push('\u{2026}');
        truncated
    }
}
