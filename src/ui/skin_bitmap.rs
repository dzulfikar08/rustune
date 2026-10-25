//! Half-block (`▀`) rasterizer: draws an RGBA framebuffer into ratatui cells
//! (1 pixel wide, 2 pixels tall per cell), at 1:1 or box-filtered down to fit.
//! Used when the terminal has no pixel graphics protocol.

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

/// Draw `canvas` box-filtered down to `cols` × `px_h` pixels (`px_h / 2`
/// rows, rounded up) with its top-left at cell `at`; returns the covered
/// cells (clipped to the buffer). Never scales up.
pub fn draw_halfblock_scaled(
    buf: &mut Buffer,
    canvas: &Canvas,
    at: (u16, u16),
    cols: u16,
    px_h: u16,
) -> Rect {
    let (w, h) = (canvas.width(), canvas.height());
    let cols = u32::from(cols).min(w);
    let px_h = u32::from(px_h).min(h);
    if cols == 0 || px_h == 0 {
        return Rect::default();
    }
    let rows = px_h.div_ceil(2);
    let area = Rect::new(at.0, at.1, cols as u16, rows as u16).intersection(buf.area);
    // Source span of output pixel `o` out of `n` over `len` source pixels.
    let span = |o: u32, n: u32, len: u32| (o * len / n, ((o + 1) * len / n).max(o * len / n + 1));
    for y in area.top()..area.bottom() {
        let oy = u32::from(y - at.1) * 2;
        let ys_top = span(oy, px_h, h);
        let ys_bottom = (oy + 1 < px_h).then(|| span(oy + 1, px_h, h));
        for x in area.left()..area.right() {
            let xs = span(u32::from(x - at.0), cols, w);
            let Some(t) = box_average(canvas, xs, ys_top) else {
                continue;
            };
            let b = ys_bottom
                .and_then(|ys| box_average(canvas, xs, ys))
                .unwrap_or(t);
            let Some(cell) = buf.cell_mut(Position::new(x, y)) else {
                continue;
            };
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

/// Mean colour of the source pixels `xs.0..xs.1` × `ys.0..ys.1`.
fn box_average(canvas: &Canvas, xs: (u32, u32), ys: (u32, u32)) -> Option<Color> {
    let mut sum = [0u32; 3];
    let mut n = 0u32;
    for py in ys.0..ys.1 {
        for px in xs.0..xs.1 {
            if let Some(p) = canvas.get(px, py) {
                sum[0] += u32::from(p[0]);
                sum[1] += u32::from(p[1]);
                sum[2] += u32::from(p[2]);
                n += 1;
            }
        }
    }
    (n > 0).then(|| Color::Rgb((sum[0] / n) as u8, (sum[1] / n) as u8, (sum[2] / n) as u8))
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

    #[test]
    fn scaled_down_averages_pixel_boxes() {
        let mut c = Canvas::new();
        c.resize(4, 4);
        c.fill([0, 0, 0, 255]);
        // Top-left 2×2 block white; two red pixels in the bottom-right block.
        for (x, y) in [(0, 0), (1, 0), (0, 1), (1, 1)] {
            c.set(x, y, [200, 200, 200, 255]);
        }
        c.set(2, 2, [100, 0, 0, 255]);
        c.set(3, 3, [100, 0, 0, 255]);
        let mut buf = Buffer::empty(Rect::new(0, 0, 4, 4));
        let r = draw_halfblock_scaled(&mut buf, &c, (0, 0), 2, 2);
        assert_eq!(r, Rect::new(0, 0, 2, 1));
        let left = buf.cell(Position::new(0, 0)).unwrap();
        assert_eq!(left.symbol(), "▀");
        assert_eq!(
            (left.fg, left.bg),
            (Color::Rgb(200, 200, 200), Color::Rgb(0, 0, 0))
        );
        let right = buf.cell(Position::new(1, 0)).unwrap();
        assert_eq!(
            (right.fg, right.bg),
            (Color::Rgb(0, 0, 0), Color::Rgb(50, 0, 0))
        );
        // Never scales up; an odd height repeats the top pixel.
        let mut buf = Buffer::empty(Rect::new(0, 0, 8, 8));
        assert_eq!(
            draw_halfblock_scaled(&mut buf, &c, (1, 1), 9, 3),
            Rect::new(1, 1, 4, 2)
        );
        assert_eq!(buf.cell(Position::new(1, 2)).unwrap().symbol(), " ");
    }
}
