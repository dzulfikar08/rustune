use ratatui::{
    layout::Rect,
    style::{Color, Style},
    Frame,
};

use crate::skin::BmpImage;

pub fn render_scaled_bitmap(frame: &mut Frame, area: Rect, bmp: &BmpImage) {
    if area.width == 0 || area.height == 0 {
        return;
    }

    // We render using upper-half block '▀' so each terminal cell represents 2 vertical pixels.
    let virtual_h = (area.height as u32).saturating_mul(2).max(1);
    let virtual_w = area.width as u32;

    let buf = frame.buffer_mut();

    for dy in 0..area.height {
        for dx in 0..area.width {
            let vx = (dx as u32)
                .saturating_mul(bmp.width)
                .checked_div(virtual_w)
                .unwrap_or(0);

            let vy_top = (dy as u32)
                .saturating_mul(2)
                .saturating_mul(bmp.height)
                .checked_div(virtual_h)
                .unwrap_or(0);

            let vy_bot = (dy as u32)
                .saturating_mul(2)
                .saturating_add(1)
                .saturating_mul(bmp.height)
                .checked_div(virtual_h)
                .unwrap_or(0);

            let top = bmp.color_at(vx, vy_top);
            let bot = bmp.color_at(vx, vy_bot);

            // If both colors are identical, a space with bg is cheaper visually than '▀'.
            if top == bot {
                buf[(area.x + dx, area.y + dy)]
                    .set_symbol(" ")
                    .set_style(Style::default().bg(top));
            } else {
                buf[(area.x + dx, area.y + dy)]
                    .set_symbol("▀")
                    .set_style(Style::default().fg(top).bg(bot));
            }
        }
    }
}

/// Render a sub-rectangle of a BMP into the given terminal area.
/// Crops from (src_x, src_y) with size (src_w, src_h), then scales
/// the cropped image to fill `area`.
pub fn render_bitmap_region(
    frame: &mut Frame,
    area: Rect,
    bmp: &BmpImage,
    src_x: u32,
    src_y: u32,
    src_w: u32,
    src_h: u32,
) {
    if area.width == 0 || area.height == 0 || src_w == 0 || src_h == 0 {
        return;
    }

    // Clamp source rect to BMP bounds
    let src_x = src_x.min(bmp.width.saturating_sub(1));
    let src_y = src_y.min(bmp.height.saturating_sub(1));
    let src_w = src_w.min(bmp.width.saturating_sub(src_x));
    let src_h = src_h.min(bmp.height.saturating_sub(src_y));

    if src_w == 0 || src_h == 0 {
        return;
    }

    // Build a cropped BmpImage
    let mut pixels = vec![0u8; (src_w as usize) * (src_h as usize)];
    for dy in 0..src_h as usize {
        let src_row_start = ((src_y as usize) + dy) * (bmp.width as usize) + (src_x as usize);
        let dst_row_start = dy * (src_w as usize);
        let src_row = &bmp.pixels[src_row_start..src_row_start + (src_w as usize)];
        pixels[dst_row_start..dst_row_start + (src_w as usize)].copy_from_slice(src_row);
    }

    let cropped = BmpImage {
        width: src_w,
        height: src_h,
        palette: bmp.palette.clone(),
        pixels,
        raw_rgb: None, // crop not supported for raw_rgb mode yet
    };

    render_scaled_bitmap(frame, area, &cropped);
}

#[allow(dead_code)]
pub fn solid_fill(frame: &mut Frame, area: Rect, color: Color) {
    let buf = frame.buffer_mut();
    for y in area.top()..area.bottom() {
        for x in area.left()..area.right() {
            buf[(x, y)].set_style(Style::default().bg(color));
        }
    }
}

