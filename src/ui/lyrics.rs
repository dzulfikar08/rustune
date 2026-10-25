//! Lyrics panel: synced lyrics follow playback with the current line
//! centred; plain lyrics wrap from the top.

use std::time::Duration;

use ratatui::{
    layout::{Alignment, Rect},
    style::{Modifier, Style},
    text::Line,
    widgets::{Block, BorderType, Paragraph, Wrap},
    Frame,
};

use crate::providers::lyrics::Lyrics;
use crate::theme::Theme;

#[derive(Debug, Clone, Copy)]
pub enum LyricsState<'a> {
    Disabled,
    /// Nothing playing.
    Idle,
    Loading,
    NotFound,
    Error(&'a str),
    Ready(&'a Lyrics),
}

pub fn render(
    frame: &mut Frame,
    area: Rect,
    state: LyricsState<'_>,
    position: Duration,
    theme: &Theme,
) {
    let synced = matches!(state, LyricsState::Ready(l) if l.is_synced() && !l.instrumental);
    let block = Block::bordered()
        .border_type(BorderType::Rounded)
        .border_style(theme.result_border)
        .title(if synced {
            " Lyrics · synced "
        } else {
            " Lyrics "
        })
        .title_style(theme.page_nav_active);
    let inner = block.inner(area);
    frame.render_widget(block, area);
    if inner.width == 0 || inner.height == 0 {
        return;
    }

    let (message, style) = match state {
        LyricsState::Disabled => ("Lyrics are disabled", theme.empty_text),
        LyricsState::Idle => ("Nothing playing", theme.empty_text),
        LyricsState::Loading => ("Loading lyrics…", theme.loading_text),
        LyricsState::NotFound => ("No lyrics found", theme.empty_text),
        LyricsState::Error(err) => (err, theme.error_text),
        LyricsState::Ready(lyrics) if lyrics.instrumental => ("♪ Instrumental", theme.player_title),
        LyricsState::Ready(lyrics) if lyrics.is_synced() => {
            render_synced(frame, inner, lyrics, position, theme);
            return;
        }
        LyricsState::Ready(Lyrics {
            plain: Some(text), ..
        }) => {
            let paragraph = Paragraph::new(text.as_str())
                .style(theme.result_title)
                .wrap(Wrap { trim: false });
            frame.render_widget(paragraph, inner);
            return;
        }
        LyricsState::Ready(_) => ("No lyrics found", theme.empty_text),
    };
    let paragraph = Paragraph::new(message)
        .style(style)
        .alignment(Alignment::Center)
        .wrap(Wrap { trim: true });
    frame.render_widget(paragraph, inner);
}

/// One row per line, the current line on the middle row. Before the first
/// line nothing is highlighted and the first line sits in the middle.
fn render_synced(
    frame: &mut Frame,
    inner: Rect,
    lyrics: &Lyrics,
    position: Duration,
    theme: &Theme,
) {
    let current = lyrics.line_index_at(position);
    let anchor = current.unwrap_or(0);
    let middle = usize::from(inner.height / 2);
    let current_style = theme.player_title.add_modifier(Modifier::BOLD);
    let other_style: Style = theme.result_title.add_modifier(Modifier::DIM);

    for row in 0..inner.height {
        // Line shown on this row: anchor + (row - middle), skipping rows
        // above the first or below the last line.
        let Some(index) = (anchor + usize::from(row)).checked_sub(middle) else {
            continue;
        };
        let Some((_, text)) = lyrics.synced.get(index) else {
            break;
        };
        let style = if current == Some(index) {
            current_style
        } else {
            other_style
        };
        let text = if text.is_empty() {
            "♪"
        } else {
            text.as_str()
        };
        let rect = Rect::new(inner.x, inner.y + row, inner.width, 1);
        frame.render_widget(Line::styled(text, style).alignment(Alignment::Center), rect);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::{backend::TestBackend, Terminal};

    fn draw(state: LyricsState<'_>, position: Duration, w: u16, h: u16) -> Terminal<TestBackend> {
        let theme = Theme::dark();
        let mut terminal = Terminal::new(TestBackend::new(w, h)).unwrap();
        terminal
            .draw(|f| render(f, f.area(), state, position, &theme))
            .unwrap();
        terminal
    }

    fn row_text(terminal: &Terminal<TestBackend>, y: u16) -> String {
        let buf = terminal.backend().buffer();
        (0..buf.area.width).map(|x| buf[(x, y)].symbol()).collect()
    }

    /// Screen column where `needle` starts in `row`.
    fn col(row: &str, needle: &str) -> u16 {
        row[..row.find(needle).unwrap()].chars().count() as u16
    }

    fn synced() -> Lyrics {
        Lyrics::from_text(
            "[00:01.00]line zero\n[00:02.00]line one\n[00:03.00]line two\n\
             [00:04.00]line three\n[00:05.00]line four\n[00:06.00]line five\n\
             [00:07.00]line six",
        )
    }

    #[test]
    fn synced_highlights_current_line_centred() {
        let lyrics = synced();
        // Inner height 7 → middle row 3 → screen row 4.
        let terminal = draw(
            LyricsState::Ready(&lyrics),
            Duration::from_millis(4_500),
            30,
            9,
        );
        assert!(row_text(&terminal, 0).contains("Lyrics · synced"));
        let middle = row_text(&terminal, 4);
        assert!(middle.contains("line three"), "{middle}");
        assert!(row_text(&terminal, 1).contains("line zero"));
        assert!(row_text(&terminal, 7).contains("line six"));

        let theme = Theme::dark();
        let buf = terminal.backend().buffer();
        let x = col(&middle, "line three");
        assert_eq!(buf[(x, 4)].fg, theme.player_title.fg.unwrap());
        assert!(buf[(x, 4)].modifier.contains(Modifier::BOLD));
        let x = col(&row_text(&terminal, 1), "line zero");
        assert!(buf[(x, 1)].modifier.contains(Modifier::DIM));
        assert!(!buf[(x, 1)].modifier.contains(Modifier::BOLD));
    }

    #[test]
    fn synced_scrolls_with_position() {
        let lyrics = synced();
        let terminal = draw(LyricsState::Ready(&lyrics), Duration::from_secs(7), 30, 5);
        // Inner height 3 → middle row 1 → screen row 2.
        assert!(row_text(&terminal, 2).contains("line six"));
        assert!(row_text(&terminal, 1).contains("line five"));
        assert!(!row_text(&terminal, 3).contains("line"));

        // Before the first line: first line centred, not highlighted.
        let terminal = draw(LyricsState::Ready(&lyrics), Duration::ZERO, 30, 5);
        let row = row_text(&terminal, 2);
        assert!(row.contains("line zero"), "{row}");
        let x = col(&row, "line zero");
        let buf = terminal.backend().buffer();
        assert!(!buf[(x, 2)].modifier.contains(Modifier::BOLD));
    }

    #[test]
    fn plain_instrumental_and_messages() {
        let plain = Lyrics::from_text("first verse\nsecond verse");
        let terminal = draw(LyricsState::Ready(&plain), Duration::ZERO, 30, 6);
        assert!(row_text(&terminal, 0).contains(" Lyrics "));
        assert!(!row_text(&terminal, 0).contains("synced"));
        assert!(row_text(&terminal, 1).contains("first verse"));
        assert!(row_text(&terminal, 2).contains("second verse"));

        let instrumental = Lyrics {
            instrumental: true,
            ..Lyrics::default()
        };
        let terminal = draw(LyricsState::Ready(&instrumental), Duration::ZERO, 30, 5);
        assert!(row_text(&terminal, 1).contains("♪ Instrumental"));

        let terminal = draw(LyricsState::Error("boom"), Duration::ZERO, 30, 5);
        assert!(row_text(&terminal, 1).contains("boom"));
        let terminal = draw(LyricsState::Loading, Duration::ZERO, 30, 5);
        assert!(row_text(&terminal, 1).contains("Loading lyrics"));
    }

    #[test]
    fn tiny_areas_do_not_panic() {
        let lyrics = synced();
        let plain = Lyrics::from_text("a long line of plain lyrics");
        let theme = Theme::dark();
        let mut terminal = Terminal::new(TestBackend::new(10, 10)).unwrap();
        for w in 0..=3 {
            for h in 0..=3 {
                let area = Rect::new(0, 0, w, h);
                for state in [
                    LyricsState::Ready(&lyrics),
                    LyricsState::Ready(&plain),
                    LyricsState::NotFound,
                    LyricsState::Idle,
                    LyricsState::Disabled,
                ] {
                    terminal
                        .draw(|f| render(f, area, state, Duration::from_secs(3), &theme))
                        .unwrap();
                }
            }
        }
    }
}
