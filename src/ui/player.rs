use ratatui::{
    layout::Rect,
    text::{Line, Span},
    widgets::{LineGauge, Paragraph},
    Frame,
};

use crate::app::{App, Status};
use crate::view::{PlayStatus, RepeatMode};

/// Two-line transport: status + title + time + flags, then the seek gauge.
pub fn render(frame: &mut Frame, info_area: Rect, gauge_area: Rect, app: &App) {
    let theme = &app.theme;
    let view = app.player_view();

    if view.status == PlayStatus::Stopped {
        // Stopped: show progress of background work / messages instead.
        let line = match &app.status {
            Status::Loading(text) | Status::Downloading(text) | Status::Info(text) => {
                Line::from(Span::styled(format!(" {text}"), theme.loading_text))
            }
            Status::Error(text) => Line::from(Span::styled(format!(" {text}"), theme.error_text)),
            _ if !view.title.is_empty() => Line::from(vec![
                Span::styled(" \u{25A0} ", theme.player_button_play),
                Span::styled(truncate(view.title, 50), theme.player_title),
            ]),
            _ => Line::default(),
        };
        frame.render_widget(Paragraph::new(line), info_area);
        if let Status::Downloading(_) = app.status {
            let gauge = LineGauge::default()
                .ratio(0.0)
                .label(Span::styled("downloading...", theme.gauge_label))
                .filled_style(theme.gauge_filled)
                .unfilled_style(theme.gauge_unfilled)
                .line_set(ratatui::symbols::line::THICK);
            frame.render_widget(gauge, gauge_area);
        }
        return;
    }

    let (btn_text, btn_style) = if view.status == PlayStatus::Paused {
        (" \u{25B6} ", theme.player_button_play)
    } else {
        (" \u{23F8} ", theme.player_button_pause)
    };
    let elapsed = view.elapsed.as_secs();
    let duration = view.duration.map_or(0, |d| d.as_secs());

    let mut flags = format!("  vol {:.0}%", view.volume * 100.0);
    if view.shuffle {
        flags.push_str(" · shuffle");
    }
    match view.repeat {
        RepeatMode::Off => {}
        RepeatMode::All => flags.push_str(" · repeat"),
        RepeatMode::One => flags.push_str(" · repeat 1"),
    }
    if view.eq.enabled {
        flags.push_str(" · EQ");
    }
    if let Some(next) = app.queue.peek_next() {
        flags.push_str(" · next: ");
        flags.push_str(&truncate(&next.title, 30));
    }

    let mut spans = vec![
        Span::styled(btn_text, btn_style),
        Span::raw(" "),
        Span::styled(truncate(view.title, 50), theme.player_title),
        Span::styled(
            format!(
                "  {}/{}",
                App::format_duration(elapsed),
                App::format_duration(duration)
            ),
            theme.player_time,
        ),
        Span::styled(flags, theme.player_time),
    ];
    match &app.status {
        Status::Error(text) => spans.push(Span::styled(format!("  {text}"), theme.error_text)),
        Status::Info(text) | Status::Loading(text) => {
            spans.push(Span::styled(format!("  {text}"), theme.loading_text))
        }
        _ => {}
    }
    frame.render_widget(Paragraph::new(Line::from(spans)), info_area);

    let ratio = if duration > 0 {
        (elapsed as f64 / duration as f64).clamp(0.0, 1.0)
    } else {
        0.0
    };
    let gauge = LineGauge::default()
        .ratio(ratio)
        .label(Span::styled(
            format!("{:.0}%", ratio * 100.0),
            theme.gauge_label,
        ))
        .filled_style(theme.gauge_filled)
        .unfilled_style(theme.gauge_unfilled)
        .line_set(ratatui::symbols::line::THICK);
    frame.render_widget(gauge, gauge_area);
}

fn truncate(s: &str, max_chars: usize) -> String {
    if s.chars().count() <= max_chars {
        s.to_string()
    } else {
        let mut truncated: String = s.chars().take(max_chars.saturating_sub(1)).collect();
        truncated.push('\u{2026}');
        truncated
    }
}

#[cfg(test)]
mod tests {
    use super::truncate;

    #[test]
    fn truncate_counts_chars_not_bytes() {
        assert_eq!(truncate("héllo", 5), "héllo");
        assert_eq!(truncate("héllo wörld", 6), "héllo…");
        assert_eq!(truncate("", 3), "");
    }
}
