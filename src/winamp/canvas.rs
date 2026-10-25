//! Reusable RGBA framebuffer with clipped sprite blitting.

use crate::skin::{Bitmap, Rgba};

use super::sprites::Px;

/// Top-down RGBA framebuffer. Resizing reuses the allocation.
#[derive(Debug, Default, Clone)]
pub struct Canvas {
    width: u32,
    height: u32,
    pixels: Vec<Rgba>,
}

impl Canvas {
    pub fn new() -> Self {
        Self::default()
    }

    /// Resize to `w`×`h`; contents are unspecified afterwards (callers redraw
    /// everything every frame).
    pub fn resize(&mut self, w: u32, h: u32) {
        self.width = w;
        self.height = h;
        self.pixels.resize(w as usize * h as usize, [0, 0, 0, 255]);
    }

    pub fn width(&self) -> u32 {
        self.width
    }

    pub fn height(&self) -> u32 {
        self.height
    }

    pub fn pixels(&self) -> &[Rgba] {
        &self.pixels
    }

    pub fn get(&self, x: u32, y: u32) -> Option<Rgba> {
        if x >= self.width || y >= self.height {
            return None;
        }
        Some(self.pixels[y as usize * self.width as usize + x as usize])
    }

    pub fn fill(&mut self, c: Rgba) {
        self.pixels.fill(c);
    }

    /// Fill `r` (clipped to the canvas).
    pub fn fill_rect(&mut self, r: Px, c: Rgba) {
        let Some((x0, y0, x1, y1)) = self.clip(r, None) else {
            return;
        };
        let w = self.width as usize;
        for y in y0..y1 {
            self.pixels[y * w + x0..y * w + x1].fill(c);
        }
    }

    pub fn set(&mut self, x: i32, y: i32, c: Rgba) {
        if x >= 0 && y >= 0 && (x as u32) < self.width && (y as u32) < self.height {
            let w = self.width as usize;
            self.pixels[y as usize * w + x as usize] = c;
        }
    }

    /// Intersection of `r` with the canvas (and optional clip), as
    /// half-open usize bounds.
    fn clip(&self, r: Px, clip: Option<Px>) -> Option<(usize, usize, usize, usize)> {
        let mut x0 = r.x.max(0);
        let mut y0 = r.y.max(0);
        let mut x1 = (r.x + r.w as i32).min(self.width as i32);
        let mut y1 = (r.y + r.h as i32).min(self.height as i32);
        if let Some(c) = clip {
            x0 = x0.max(c.x);
            y0 = y0.max(c.y);
            x1 = x1.min(c.x + c.w as i32);
            y1 = y1.min(c.y + c.h as i32);
        }
        (x0 < x1 && y0 < y1).then_some((x0 as usize, y0 as usize, x1 as usize, y1 as usize))
    }

    /// Copy sprite `src_rect` of `src` to (dx, dy). Parts of the sprite that
    /// fall outside the sheet (short/odd bitmaps) are not drawn; a missing
    /// sheet draws nothing.
    pub fn blit(&mut self, src: Option<&Bitmap>, src_rect: Px, dx: i32, dy: i32) {
        self.blit_clipped(src, src_rect, dx, dy, None);
    }

    pub fn blit_clipped(
        &mut self,
        src: Option<&Bitmap>,
        src_rect: Px,
        dx: i32,
        dy: i32,
        clip: Option<Px>,
    ) {
        let Some(src) = src else { return };
        // Clip the source rect to the sheet first.
        let sx0 = src_rect.x.max(0);
        let sy0 = src_rect.y.max(0);
        let sx1 = (src_rect.x + src_rect.w as i32).min(src.width as i32);
        let sy1 = (src_rect.y + src_rect.h as i32).min(src.height as i32);
        if sx0 >= sx1 || sy0 >= sy1 {
            return;
        }
        let dest = Px {
            x: dx + (sx0 - src_rect.x),
            y: dy + (sy0 - src_rect.y),
            w: (sx1 - sx0) as u32,
            h: (sy1 - sy0) as u32,
        };
        let Some((x0, y0, x1, y1)) = self.clip(dest, clip) else {
            return;
        };
        let w = self.width as usize;
        let sw = src.width as usize;
        // Source coordinate of destination (x0, y0).
        let sx = (sx0 + (x0 as i32 - dest.x)) as usize;
        let sy = (sy0 + (y0 as i32 - dest.y)) as usize;
        let n = x1 - x0;
        for row in 0..(y1 - y0) {
            let s = (sy + row) * sw + sx;
            let d = (y0 + row) * w + x0;
            self.pixels[d..d + n].copy_from_slice(&src.pixels[s..s + n]);
        }
    }

    /// Tile sprite horizontally across `[x0, x1)` at row `dy`.
    pub fn tile_h(&mut self, src: Option<&Bitmap>, sprite: Px, x0: i32, x1: i32, dy: i32) {
        let clip = Px {
            x: x0,
            y: dy,
            w: (x1 - x0).max(0) as u32,
            h: sprite.h,
        };
        let mut x = x0;
        while x < x1 && sprite.w > 0 {
            self.blit_clipped(src, sprite, x, dy, Some(clip));
            x += sprite.w as i32;
        }
    }

    /// Tile sprite vertically across `[y0, y1)` at column `dx`.
    pub fn tile_v(&mut self, src: Option<&Bitmap>, sprite: Px, dx: i32, y0: i32, y1: i32) {
        let clip = Px {
            x: dx,
            y: y0,
            w: sprite.w,
            h: (y1 - y0).max(0) as u32,
        };
        let mut y = y0;
        while y < y1 && sprite.h > 0 {
            self.blit_clipped(src, sprite, dx, y, Some(clip));
            y += sprite.h as i32;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::winamp::sprites::px;

    fn gradient(w: u32, h: u32) -> Bitmap {
        let mut b = Bitmap::new(w, h, [0; 4]);
        for y in 0..h {
            for x in 0..w {
                b.pixels[(y * w + x) as usize] = [x as u8, y as u8, 0, 255];
            }
        }
        b
    }

    #[test]
    fn blit_copies_sprite_and_clips_to_canvas() {
        let src = gradient(10, 10);
        let mut c = Canvas::new();
        c.resize(4, 4);
        c.fill([9, 9, 9, 255]);
        c.blit(Some(&src), px(5, 6, 3, 3), 2, 2);
        assert_eq!(c.get(2, 2), Some([5, 6, 0, 255]));
        assert_eq!(c.get(3, 3), Some([6, 7, 0, 255]));
        assert_eq!(c.get(1, 1), Some([9, 9, 9, 255]));
        // Negative destination clips the top-left of the sprite.
        c.blit(Some(&src), px(0, 0, 3, 3), -2, -2);
        assert_eq!(c.get(0, 0), Some([2, 2, 0, 255]));
    }

    #[test]
    fn blit_outside_sheet_or_missing_sheet_draws_nothing() {
        let src = gradient(4, 4);
        let mut c = Canvas::new();
        c.resize(4, 4);
        c.fill([1, 1, 1, 255]);
        c.blit(Some(&src), px(10, 10, 3, 3), 0, 0);
        c.blit(None, px(0, 0, 3, 3), 0, 0);
        assert!(c.pixels().iter().all(|p| *p == [1, 1, 1, 255]));
        // Partially outside the sheet: only the overlapping part is drawn.
        c.blit(Some(&src), px(3, 3, 5, 5), 0, 0);
        assert_eq!(c.get(0, 0), Some([3, 3, 0, 255]));
        assert_eq!(c.get(1, 1), Some([1, 1, 1, 255]));
    }

    #[test]
    fn tiling_respects_bounds() {
        let src = gradient(3, 2);
        let mut c = Canvas::new();
        c.resize(8, 6);
        c.fill([0, 0, 0, 255]);
        c.tile_h(Some(&src), px(0, 0, 3, 2), 1, 7, 0);
        assert_eq!(c.get(1, 0), Some([0, 0, 0, 255]));
        assert_eq!(c.get(4, 1), Some([0, 1, 0, 255]));
        assert_eq!(c.get(6, 0), Some([2, 0, 0, 255]));
        assert_eq!(c.get(7, 0), Some([0, 0, 0, 255]));
        c.tile_v(Some(&src), px(0, 0, 3, 2), 0, 2, 5);
        assert_eq!(c.get(1, 4), Some([1, 0, 0, 255]));
        assert_eq!(c.get(1, 5), Some([0, 0, 0, 255]));
    }
}
