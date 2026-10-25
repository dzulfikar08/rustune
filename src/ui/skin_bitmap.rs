//! Half-block (`▀`) rasterizer: draws an RGBA framebuffer at 1:1 pixel scale
//! into ratatui cells (1 pixel wide, 2 pixels tall per cell). Used when the
//! terminal has no pixel graphics protocol.

use ratatui::buffer::Buffer;
use ratatui::layout::{Position, Rect};
use ratatui::style::Color;

use crate::winamp::canvas::Canvas;

/// Draw `canvas` with its top-left at cell `at`; returns the covered cells
/// (clipped to the buffer).
pub fn draw_halfblock(buf: &mut Buffer, canvas: &Canvas, at: (u16, u16)) -> Rect {
    let cols = canvas.width().min(u32::from(u16::MAX)) as u16;
    let rows = canvas.height().div_ceil(2).min(u32::from(u16::MAX)) as u16;
    let area = Rect::new(at.0, at.1, cols, rows).intersection(buf.area);
    for y in area.top()..area.bottom() {
        let py = u32::from(y - at.1) * 2;
        for x in area.left()..area.right() {
            let px = u32::from(x - at.0);
            let top = canvas.get(px, py);
            let bottom = canvas.get(px, py + 1).or(top);
            let (Some(t), Some(b)) = (top, bottom) else {
                continue;
            };
            let Some(cell) = buf.cell_mut(Position::new(x, y)) else {
                continue;
            };
            let (t, b) = (Color::Rgb(t[0], t[1], t[2]), Color::Rgb(b[0], b[1], b[2]));
            cell.reset();
            if t == b {
                cell.set_symbol(" ").set_bg(t);
            } else {
                cell.set_symbol("▀").set_fg(t).set_bg(b);
            }
        }
    }
    area
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn two_pixels_per_cell() {
        let mut c = Canvas::new();
        c.resize(2, 3);
        c.fill([10, 10, 10, 255]);
        c.set(0, 1, [200, 0, 0, 255]);
        let mut buf = Buffer::empty(Rect::new(0, 0, 5, 5));
        let r = draw_halfblock(&mut buf, &c, (1, 1));
        assert_eq!(r, Rect::new(1, 1, 2, 2));
        let a = buf.cell(Position::new(1, 1)).unwrap();
        assert_eq!(a.symbol(), "▀");
        assert_eq!(a.fg, Color::Rgb(10, 10, 10));
        assert_eq!(a.bg, Color::Rgb(200, 0, 0));
        let b = buf.cell(Position::new(2, 1)).unwrap();
        assert_eq!(b.symbol(), " ");
        assert_eq!(b.bg, Color::Rgb(10, 10, 10));
        // Odd height: last row repeats the top pixel.
        let c2 = buf.cell(Position::new(1, 2)).unwrap();
        assert_eq!(c2.bg, Color::Rgb(10, 10, 10));
    }
}
