//! Classic stacked window layout (main, equalizer, playlist) fitted to a
//! terminal area, plus the per-skin render state the legacy
//! `winamp::render(frame, app)` entry keeps in `App::skin_layout`.

use ratatui::layout::Rect;

use crate::gfx::{GfxPainter, Protocol};
use crate::skin::WinampSkin;
use crate::winamp::compose::playlist_height;
use crate::winamp::sprites::{Px, PL_BOTTOM_H, PL_ROW_STEP, PL_TOP_H, WINDOW_H, WINDOW_W};
use crate::winamp::{WinampUiState, Window};

/// Render state for the legacy entry point. The legacy main loop never
/// flushes images, so its painter is pinned to half-block output.
pub struct SkinLayout {
    pub ui: WinampUiState,
    pub painter: GfxPainter,
}

impl SkinLayout {
    /// `None` when the skin has no MAIN.BMP (text layout only).
    pub fn from_skin(skin: &WinampSkin) -> Option<Self> {
        skin.has_bitmaps().then(|| Self {
            ui: WinampUiState::new(),
            painter: GfxPainter::with_cell_size(Protocol::HalfBlock, None),
        })
    }
}

/// One window placed on screen.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WindowPlan {
    pub window: Window,
    /// Top-left cell.
    pub at: (u16, u16),
    /// Covered cells.
    pub cells: Rect,
    pub height_px: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Plan {
    /// Integer pixel scale (1 in half-block mode).
    pub scale: u32,
    /// Cell size in pixels ((1, 2) in half-block mode).
    pub cell: (u16, u16),
    pub main: WindowPlan,
    pub eq: Option<WindowPlan>,
    pub playlist: Option<WindowPlan>,
}

impl Plan {
    /// Cells covered by window pixel rect `r` of `w`.
    pub fn cells_of(&self, w: &WindowPlan, r: Px) -> Rect {
        let (cw, ch) = (i64::from(self.cell.0), i64::from(self.cell.1));
        let s = i64::from(self.scale);
        let x0 = i64::from(w.at.0) + i64::from(r.x) * s / cw;
        let y0 = i64::from(w.at.1) + i64::from(r.y) * s / ch;
        let x1 = i64::from(w.at.0) + ((i64::from(r.x) + i64::from(r.w)) * s + cw - 1) / cw;
        let y1 = i64::from(w.at.1) + ((i64::from(r.y) + i64::from(r.h)) * s + ch - 1) / ch;
        let clamp = |v: i64| v.clamp(0, i64::from(u16::MAX)) as u16;
        Rect::new(clamp(x0), clamp(y0), clamp(x1 - x0), clamp(y1 - y0)).intersection(w.cells)
    }
}

const MAX_SCALE: u32 = 8;

fn rows_for(px: u32, scale: u32, ch: u16) -> u16 {
    (px * scale)
        .div_ceil(u32::from(ch))
        .min(u32::from(u16::MAX)) as u16
}

/// Fit the windows into `area` (cells) for cell size `cell` (px). Picks the
/// largest integer scale at which main + EQ + a minimal playlist fit; if
/// nothing fits at 1x it drops the EQ, then the playlist. `None` = even the
/// main window doesn't fit (use the text layout).
pub fn plan(
    area: Rect,
    cell: (u16, u16),
    max_scale: u32,
    show_eq: bool,
    show_pl: bool,
) -> Option<Plan> {
    let (cw, ch) = (cell.0.max(1), cell.1.max(1));
    let avail_w = u32::from(area.width) * u32::from(cw);
    let max_scale = max_scale.clamp(1, MAX_SCALE).min(avail_w / WINDOW_W);
    let wants = [(show_eq, show_pl), (false, show_pl), (false, false)];
    for (eq, pl) in wants {
        for scale in (1..=max_scale).rev() {
            let main_rows = rows_for(WINDOW_H, scale, ch);
            let eq_rows = if eq { rows_for(WINDOW_H, scale, ch) } else { 0 };
            let pl_min = if pl {
                rows_for(playlist_height(1), scale, ch)
            } else {
                0
            };
            let need = u32::from(main_rows) + u32::from(eq_rows) + u32::from(pl_min);
            if need > u32::from(area.height) {
                continue;
            }
            let cols = (WINDOW_W * scale).div_ceil(u32::from(cw)) as u16;
            let x = area.x + (area.width - cols) / 2;
            let place = |window, y: u16, rows: u16, height_px| WindowPlan {
                window,
                at: (x, y),
                cells: Rect::new(x, y, cols, rows),
                height_px,
            };
            let main = place(Window::Main, area.y, main_rows, WINDOW_H);
            let eq_plan =
                eq.then(|| place(Window::Equalizer, area.y + main_rows, eq_rows, WINDOW_H));
            let playlist = pl.then(|| {
                let y = area.y + main_rows + eq_rows;
                let rows_left = area.height - main_rows - eq_rows;
                let px_avail = u32::from(rows_left) * u32::from(ch) / scale;
                let segments = px_avail.saturating_sub(PL_TOP_H + PL_BOTTOM_H) / PL_ROW_STEP;
                let h = playlist_height(segments);
                place(Window::Playlist, y, rows_for(h, scale, ch), h)
            });
            return Some(Plan {
                scale,
                cell: (cw, ch),
                main,
                eq: eq_plan,
                playlist,
            });
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn halfblock_needs_full_width_and_drops_windows_by_height() {
        // 275 cols × 150 rows at 1×2 px: main 58 + eq 58 + playlist ≥ 44 rows.
        let p = plan(Rect::new(0, 0, 300, 160), (1, 2), 1, true, true).unwrap();
        assert_eq!(p.scale, 1);
        assert_eq!(p.main.cells, Rect::new(12, 0, 275, 58));
        assert_eq!(p.eq.unwrap().cells.y, 58);
        let pl = p.playlist.unwrap();
        assert_eq!(pl.cells.y, 116);
        // 44 rows = 88 px → 1 segment (87 px).
        assert_eq!(pl.height_px, 87);
        // Too short for EQ + playlist: EQ goes first.
        let p = plan(Rect::new(0, 0, 275, 110), (1, 2), 1, true, true).unwrap();
        assert!(p.eq.is_none());
        assert!(p.playlist.is_some());
        // Too narrow: text layout.
        assert!(plan(Rect::new(0, 0, 200, 200), (1, 2), 1, true, true).is_none());
    }

    #[test]
    fn graphics_picks_largest_integer_scale() {
        // 200×60 cells of 10×20 px = 2000×1200 px.
        let p = plan(Rect::new(0, 0, 200, 60), (10, 20), 8, true, true).unwrap();
        // 3x: main 348px = 18 rows, eq 18, playlist min 261px = 14 rows → 50 ≤ 60.
        assert_eq!(p.scale, 3);
        assert_eq!(p.main.cells.width, 83);
        let pl = p.playlist.unwrap();
        assert_eq!(pl.cells.y, 36);
        // 24 rows × 20 px / 3 = 160 px → 3 segments → 145 px.
        assert_eq!(pl.height_px, 145);
        assert_eq!(pl.cells.height, 22);
    }

    #[test]
    fn cells_of_maps_window_pixels() {
        let p = plan(Rect::new(0, 0, 200, 60), (10, 20), 3, false, false).unwrap();
        assert_eq!(p.scale, 3);
        // Play button (39,88,23,18) at scale 3 → x 117..186 → cols 11..19.
        let r = p.cells_of(
            &p.main,
            Px {
                x: 39,
                y: 88,
                w: 23,
                h: 18,
            },
        );
        assert_eq!(r.x, p.main.at.0 + 11);
        assert_eq!(r.width, 8);
    }
}
