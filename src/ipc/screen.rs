//! `rustune screen`: the last drawn TUI frame as plain text and as ANSI.
//! Built from ratatui's buffer right after a draw, only when a client asked.

use std::fmt::Write as _;

use ratatui::buffer::Buffer;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::Span;

use super::protocol::{Cursor, Screen};

/// Where the prompt's caret glyph is drawn: its symbol and style.
pub struct Caret<'a> {
    pub symbol: &'a str,
    pub style: Style,
}

/// Render `buffer` as text (rows right-trimmed) and as ANSI-coloured text.
/// `caret` locates the text cursor of a focused prompt (the TUI draws its
/// own caret glyph; the terminal cursor stays hidden).
pub fn capture(buffer: &Buffer, caret: Option<Caret<'_>>) -> Screen {
    let area = buffer.area;
    let mut text = String::with_capacity(usize::from(area.width + 1) * usize::from(area.height));
    let mut ansi = String::with_capacity(text.capacity() * 2);
    let mut row = String::new();
    for y in area.top()..area.bottom() {
        row.clear();
        let mut style: Option<(Color, Color, Modifier)> = None;
        let mut x = area.left();
        while x < area.right() {
            let cell = &buffer[(x, y)];
            let symbol = cell.symbol();
            let width = Span::raw(symbol).width().max(1) as u16;
            let cell_style = (cell.fg, cell.bg, cell.modifier);
            if style != Some(cell_style) {
                write_sgr(&mut ansi, cell.fg, cell.bg, cell.modifier);
                style = Some(cell_style);
            }
            ansi.push_str(symbol);
            row.push_str(symbol);
            // A wide character covers the cells after it.
            x = x.saturating_add(width);
        }
        ansi.push_str("\x1b[0m\n");
        text.push_str(row.trim_end());
        text.push('\n');
    }
    let cursor = caret.and_then(|caret| find_caret(buffer, &caret));
    Screen {
        width: area.width,
        height: area.height,
        text,
        ansi,
        cursor,
    }
}

/// The bottom-most cell drawing the caret glyph in the prompt's colour.
fn find_caret(buffer: &Buffer, caret: &Caret<'_>) -> Option<Cursor> {
    let area = buffer.area;
    (area.top()..area.bottom()).rev().find_map(|y| {
        (area.left()..area.right()).find_map(|x| {
            let cell = &buffer[(x, y)];
            let same_fg = caret.style.fg.is_none_or(|fg| fg == cell.fg);
            (cell.symbol() == caret.symbol && same_fg).then_some(Cursor {
                x: x - area.left(),
                y: y - area.top(),
            })
        })
    })
}

fn color_code(out: &mut String, color: Color, base: u8) {
    // `base` is 30 (foreground) or 40 (background).
    let _ = match color {
        Color::Reset => write!(out, ";{}", base + 9),
        Color::Black => write!(out, ";{base}"),
        Color::Red => write!(out, ";{}", base + 1),
        Color::Green => write!(out, ";{}", base + 2),
        Color::Yellow => write!(out, ";{}", base + 3),
        Color::Blue => write!(out, ";{}", base + 4),
        Color::Magenta => write!(out, ";{}", base + 5),
        Color::Cyan => write!(out, ";{}", base + 6),
        Color::Gray => write!(out, ";{}", base + 7),
        Color::DarkGray => write!(out, ";{}", base + 60),
        Color::LightRed => write!(out, ";{}", base + 61),
        Color::LightGreen => write!(out, ";{}", base + 62),
        Color::LightYellow => write!(out, ";{}", base + 63),
        Color::LightBlue => write!(out, ";{}", base + 64),
        Color::LightMagenta => write!(out, ";{}", base + 65),
        Color::LightCyan => write!(out, ";{}", base + 66),
        Color::White => write!(out, ";{}", base + 67),
        Color::Indexed(i) => write!(out, ";{};5;{i}", base + 8),
        Color::Rgb(r, g, b) => write!(out, ";{};2;{r};{g};{b}", base + 8),
    };
}

/// Reset, then set the colours and attributes of the next cells.
fn write_sgr(out: &mut String, fg: Color, bg: Color, modifier: Modifier) {
    out.push_str("\x1b[0");
    for (flag, code) in [
        (Modifier::BOLD, ";1"),
        (Modifier::DIM, ";2"),
        (Modifier::ITALIC, ";3"),
        (Modifier::UNDERLINED, ";4"),
        (Modifier::SLOW_BLINK, ";5"),
        (Modifier::RAPID_BLINK, ";6"),
        (Modifier::REVERSED, ";7"),
        (Modifier::HIDDEN, ";8"),
        (Modifier::CROSSED_OUT, ";9"),
    ] {
        if modifier.contains(flag) {
            out.push_str(code);
        }
    }
    color_code(out, fg, 30);
    color_code(out, bg, 40);
    out.push('m');
}

#[cfg(test)]
mod tests {
    use ratatui::backend::TestBackend;
    use ratatui::layout::Rect;
    use ratatui::style::Stylize;
    use ratatui::Terminal;

    use super::*;
    use crate::app::App;
    use crate::config::Config;
    use crate::gfx::{GfxPainter, Protocol};
    use crate::media::{MediaItem, SourceKind};
    use crate::ui;
    use crate::winamp::WinampUiState;

    fn draw_app(app: &mut App, width: u16, height: u16) -> Terminal<TestBackend> {
        let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
        let mut winamp_ui = WinampUiState::new();
        let mut preview = ui::PreviewUi::default();
        let mut gfx = GfxPainter::with_cell_size(Protocol::HalfBlock, None);
        terminal
            .draw(|frame| {
                ui::draw(frame, app, &mut winamp_ui, &mut preview, &mut gfx);
            })
            .unwrap();
        terminal
    }

    fn test_app() -> App {
        let mut app = App::new(Config {
            onboarding_done: true,
            theme: "Dark".into(),
            ..Config::default()
        });
        app.results = vec![MediaItem {
            id: "/m/tone.wav".into(),
            title: "Sine Tone".into(),
            artist: None,
            album: None,
            duration: Some(20),
            subtitle: None,
            source: SourceKind::Local,
        }];
        app
    }

    #[test]
    fn captures_the_drawn_ui_as_text() {
        let mut app = test_app();
        let terminal = draw_app(&mut app, 100, 30);
        let screen = capture(terminal.backend().buffer(), None);
        assert_eq!((screen.width, screen.height), (100, 30));
        assert_eq!(screen.text.lines().count(), 30);
        for needle in ["Local", "Online", "Sine Tone"] {
            assert!(screen.text.contains(needle), "{needle}:\n{}", screen.text);
        }
        assert!(screen.text.lines().all(|l| l == l.trim_end()));
        assert!(screen.ansi.contains("\x1b[0"));
        assert!(screen.ansi.contains("Sine Tone"));
        assert_eq!(screen.cursor, None);
    }

    #[test]
    fn the_palette_shows_up_after_its_key() {
        let mut app = test_app();
        let before = capture(draw_app(&mut app, 100, 30).backend().buffer(), None);
        let key = crate::ipc::keys::parse_key("?").unwrap();
        let _ = app.handle_browse_key(key);
        assert!(app.palette.is_some());
        let after = capture(draw_app(&mut app, 100, 30).backend().buffer(), None);
        assert_ne!(before.text, after.text);
        assert!(after.text.contains("Commands"), "{}", after.text);
        assert!(!before.text.contains("Commands"));
    }

    #[test]
    fn ansi_colours_wide_chars_and_caret() {
        let mut buffer = Buffer::empty(Rect::new(0, 0, 6, 2));
        buffer.set_string(0, 0, "a", Style::new().red().bold());
        buffer.set_string(1, 0, "日b", Style::new());
        buffer.set_string(2, 1, "\u{2588}", Style::new().fg(Color::Rgb(1, 2, 3)));
        let caret = Caret {
            symbol: "\u{2588}",
            style: Style::new().fg(Color::Rgb(1, 2, 3)),
        };
        let screen = capture(&buffer, Some(caret));
        assert_eq!(screen.text, "a日b\n  \u{2588}\n");
        assert!(screen.ansi.starts_with("\x1b[0;1;31;49ma\x1b[0;39;49m日b"));
        assert!(screen.ansi.contains("\x1b[0;38;2;1;2;3;49m\u{2588}"));
        assert_eq!(screen.cursor, Some(Cursor { x: 2, y: 1 }));
    }
}
