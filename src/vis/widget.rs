//! ratatui rendering of analyzer output. Every mode writes each cell of its
//! area exactly once (O(cells)) and never allocates.

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::Color;
use ratatui::widgets::Widget;

use super::analyzer::{Analyzer, HistoryView};
use super::palette::VisPalette;
use super::VisMode;

/// Bottom-aligned eighth blocks, index = filled eighths.
const EIGHTHS: [char; 9] = [' ', '▁', '▂', '▃', '▄', '▅', '▆', '▇', '█'];
/// One lit LED segment (the top gap separates rows visually).
const LED: char = '▆';
/// Braille dot bits per column, top → bottom.
const LEFT_DOTS: [u8; 4] = [0x01, 0x02, 0x04, 0x40];
const RIGHT_DOTS: [u8; 4] = [0x08, 0x10, 0x20, 0x80];
/// Braille bits for a column filled `k` dots from the bottom.
const LEFT_FILL: [u8; 5] = [0, 0x40, 0x44, 0x46, 0x47];
const RIGHT_FILL: [u8; 5] = [0, 0x80, 0xA0, 0xB0, 0xB8];
/// Spectrogram cells below this level stay blank.
const WATERFALL_MIN: f32 = 1.0 / 32.0;

/// Borrowed data a frame is drawn from: an [`Analyzer`] or any external
/// source (e.g. `PlayerView.spectrum` / `scope`).
#[derive(Debug, Clone, Copy, Default)]
pub struct VisFrame<'a> {
    /// 0.0..=1.0 per band, low → high frequency.
    pub spectrum: &'a [f32],
    /// Peak markers parallel to `spectrum` (may be empty).
    pub peaks: &'a [f32],
    /// -1.0..=1.0, oldest first.
    pub scope: &'a [f32],
    pub history: HistoryView<'a>,
    /// Animation counter for stateless effects (fire flicker).
    pub frame: u64,
}

impl<'a> VisFrame<'a> {
    pub fn from_analyzer(a: &'a Analyzer) -> Self {
        Self {
            spectrum: a.spectrum(),
            peaks: a.peaks(),
            scope: a.scope(),
            history: a.history(),
            frame: a.frame(),
        }
    }
}

pub struct Visualizer<'a> {
    data: VisFrame<'a>,
    mode: VisMode,
    palette: &'a VisPalette,
}

impl<'a> Visualizer<'a> {
    pub fn new(analyzer: &'a Analyzer, mode: VisMode, palette: &'a VisPalette) -> Self {
        Self::from_frame(VisFrame::from_analyzer(analyzer), mode, palette)
    }

    pub fn from_frame(data: VisFrame<'a>, mode: VisMode, palette: &'a VisPalette) -> Self {
        Self {
            data,
            mode,
            palette,
        }
    }
}

impl Widget for Visualizer<'_> {
    fn render(self, area: Rect, buf: &mut Buffer) {
        let area = area.intersection(buf.area);
        if area.is_empty() {
            return;
        }
        let mut p = Painter {
            buf,
            area,
            bg: self.palette.background,
        };
        let (w, h) = (area.width as usize, area.height as usize);
        match self.mode {
            VisMode::Bars => self.bars(&mut p, w, h),
            VisMode::Braille => self.braille(&mut p, w, h),
            VisMode::Led => self.led(&mut p, w, h),
            VisMode::Mirror => self.mirror(&mut p, w, h),
            VisMode::Scope => self.scope(&mut p, w, h),
            VisMode::Spectrogram => self.spectrogram(&mut p, w, h),
            VisMode::Fire => self.fire(&mut p, w, h),
            VisMode::None => {
                for y in 0..h {
                    for x in 0..w {
                        p.blank(x, y);
                    }
                }
            }
        }
    }
}

impl Visualizer<'_> {
    fn bars(&self, p: &mut Painter, w: usize, h: usize) {
        let cols = Columns::new(w, self.data.spectrum.len());
        for x in 0..w {
            let filled = cols
                .sample(self.data.spectrum, x)
                .map_or(0, |v| units(v, h * 8));
            for r in 0..h {
                let fill = filled.saturating_sub(r * 8).min(8);
                let y = h - 1 - r;
                if fill == 0 {
                    p.blank(x, y);
                } else {
                    p.put(x, y, EIGHTHS[fill], self.palette.row(r, h));
                }
            }
        }
    }

    fn braille(&self, p: &mut Painter, w: usize, h: usize) {
        let spec = self.data.spectrum;
        for x in 0..w {
            let left = units(resample(spec, 2 * x, 2 * w), h * 4);
            let right = units(resample(spec, 2 * x + 1, 2 * w), h * 4);
            for r in 0..h {
                let bits = LEFT_FILL[left.saturating_sub(r * 4).min(4)]
                    | RIGHT_FILL[right.saturating_sub(r * 4).min(4)];
                let y = h - 1 - r;
                if bits == 0 {
                    p.blank(x, y);
                } else {
                    p.put(x, y, braille(bits), self.palette.row(r, h));
                }
            }
        }
    }

    fn led(&self, p: &mut Painter, w: usize, h: usize) {
        let cols = Columns::new(w, self.data.spectrum.len());
        for x in 0..w {
            let Some(level) = cols.sample(self.data.spectrum, x) else {
                for y in 0..h {
                    p.blank(x, y);
                }
                continue;
            };
            let lit = units(level, h);
            let peak = cols.sample(self.data.peaks, x).map_or(0, |v| units(v, h));
            for r in 0..h {
                let y = h - 1 - r;
                if peak > 0 && r == peak - 1 {
                    p.put(x, y, LED, self.palette.peak);
                } else if r < lit {
                    p.put(x, y, LED, self.palette.row(r, h));
                } else if let Some(grid) = self.palette.grid {
                    p.put(x, y, LED, grid);
                } else {
                    p.blank(x, y);
                }
            }
        }
    }

    /// Lows in the centre, highs toward both edges; bars grow up and down
    /// from the middle line at half-cell resolution.
    fn mirror(&self, p: &mut Painter, w: usize, h: usize) {
        let centre = w / 2;
        let side = w - centre;
        for x in 0..w {
            let k = if x >= centre {
                x - centre
            } else {
                centre - 1 - x
            };
            let a = units(resample(self.data.spectrum, k, side), h);
            // Lit half-cells, counted from the top over 2*h half-rows.
            let (lo, hi) = (h - a, h + a);
            for y in 0..h {
                let upper = (lo..hi).contains(&(2 * y));
                let lower = (lo..hi).contains(&(2 * y + 1));
                let ch = match (upper, lower) {
                    (true, true) => '█',
                    (true, false) => '▀',
                    (false, true) => '▄',
                    (false, false) => {
                        p.blank(x, y);
                        continue;
                    }
                };
                let dist = (2 * y + 1).abs_diff(h) as f32 / h as f32;
                p.put(x, y, ch, self.palette.level(dist));
            }
        }
    }

    /// Braille oscilloscope; consecutive points are joined vertically so the
    /// trace stays continuous.
    fn scope(&self, p: &mut Painter, w: usize, h: usize) {
        let s = self.data.scope;
        let dots_w = 2 * w;
        let rows = 4 * h;
        let ypos = |dx: usize| -> usize {
            let v = if s.is_empty() {
                0.0
            } else {
                s[dx * s.len() / dots_w].clamp(-1.0, 1.0)
            };
            (((1.0 - v) * 0.5 * (rows - 1) as f32).round() as usize).min(rows - 1)
        };
        let mut prev = ypos(0);
        for x in 0..w {
            let y0 = ypos(2 * x);
            let left = (prev.min(y0), prev.max(y0));
            let y1 = ypos(2 * x + 1);
            let right = (y0.min(y1), y0.max(y1));
            prev = y1;
            for cy in 0..h {
                let bits =
                    span_bits(left, cy * 4, &LEFT_DOTS) | span_bits(right, cy * 4, &RIGHT_DOTS);
                if bits == 0 {
                    p.blank(x, cy);
                } else {
                    p.put(x, cy, braille(bits), self.palette.scope);
                }
            }
        }
    }

    /// Waterfall: newest row at the top, two history rows per cell.
    fn spectrogram(&self, p: &mut Painter, w: usize, h: usize) {
        let hist = self.data.history;
        let empty = self.palette.background.unwrap_or(Color::Reset);
        for y in 0..h {
            let top = hist.row(2 * y);
            let bottom = hist.row(2 * y + 1);
            for x in 0..w {
                let t = top.map_or(0.0, |r| resample(r, x, w));
                let b = bottom.map_or(0.0, |r| resample(r, x, w));
                match (t >= WATERFALL_MIN, b >= WATERFALL_MIN) {
                    (false, false) => p.blank(x, y),
                    (true, true) => {
                        p.put_bg(x, y, '▀', self.palette.level(t), self.palette.level(b))
                    }
                    (true, false) => p.put_bg(x, y, '▀', self.palette.level(t), empty),
                    (false, true) => p.put_bg(x, y, '▄', self.palette.level(b), empty),
                }
            }
        }
    }

    /// Flames whose height follows the spectrum, hottest colour at the base,
    /// with deterministic per-column flicker.
    fn fire(&self, p: &mut Painter, w: usize, h: usize) {
        let seed = (self.data.frame / 2) as u32;
        for x in 0..w {
            let v = resample(self.data.spectrum, x, w).clamp(0.0, 1.0);
            let flame = (v * (0.85 + 0.3 * hash01(x as u32, seed)) * h as f32).min(h as f32);
            let full = flame as usize;
            let frac = flame - full as f32;
            for r in 0..h {
                let y = h - 1 - r;
                let ch = if r < full {
                    '█'
                } else if r == full && frac > 0.05 {
                    match frac {
                        f if f > 0.66 => '▓',
                        f if f > 0.33 => '▒',
                        _ => '░',
                    }
                } else {
                    p.blank(x, y);
                    continue;
                };
                let heat = 1.0 - r as f32 / flame;
                p.put(x, y, ch, self.palette.level(heat));
            }
        }
    }
}

struct Painter<'b> {
    buf: &'b mut Buffer,
    area: Rect,
    bg: Option<Color>,
}

impl Painter<'_> {
    #[inline]
    fn cell(&mut self, x: usize, y: usize) -> &mut ratatui::buffer::Cell {
        &mut self.buf[(self.area.x + x as u16, self.area.y + y as u16)]
    }

    #[inline]
    fn put(&mut self, x: usize, y: usize, ch: char, fg: Color) {
        let bg = self.bg;
        let cell = self.cell(x, y);
        cell.set_char(ch).set_fg(fg);
        if let Some(bg) = bg {
            cell.set_bg(bg);
        }
    }

    #[inline]
    fn put_bg(&mut self, x: usize, y: usize, ch: char, fg: Color, bg: Color) {
        self.cell(x, y).set_char(ch).set_fg(fg).set_bg(bg);
    }

    #[inline]
    fn blank(&mut self, x: usize, y: usize) {
        let bg = self.bg;
        let cell = self.cell(x, y);
        cell.set_char(' ');
        if let Some(bg) = bg {
            cell.set_bg(bg);
        }
    }
}

/// Column → band mapping: one bar per band with a one-cell gap when the
/// area is at least twice as wide as the band count (bars centred),
/// otherwise every column resamples the bands.
struct Columns {
    w: usize,
    n: usize,
    group: usize,
    offset: usize,
}

impl Columns {
    fn new(w: usize, n: usize) -> Self {
        let group = if n > 0 && w >= 2 * n { w / n } else { 0 };
        Self {
            w,
            n,
            group,
            offset: (w - group * n) / 2,
        }
    }

    /// Value of `data` (parallel to the spectrum) for column `x`, or `None`
    /// for gap/padding columns.
    fn sample(&self, data: &[f32], x: usize) -> Option<f32> {
        if self.n == 0 {
            return None;
        }
        if self.group == 0 {
            return Some(resample(data, x, self.w));
        }
        let rel = x.checked_sub(self.offset)?;
        let band = rel / self.group;
        if band >= self.n || rel % self.group == self.group - 1 {
            return None;
        }
        Some(data.get(band).copied().unwrap_or(0.0))
    }
}

/// Maximum of the slice of `data` that output column `i` of `n` covers.
fn resample(data: &[f32], i: usize, n: usize) -> f32 {
    if data.is_empty() || n == 0 {
        return 0.0;
    }
    let len = data.len();
    let lo = (i * len / n).min(len - 1);
    let hi = ((i + 1) * len / n).clamp(lo + 1, len);
    data[lo..hi].iter().fold(0.0f32, |acc, &v| acc.max(v))
}

/// Level 0.0..=1.0 → whole units out of `max`.
#[inline]
fn units(v: f32, max: usize) -> usize {
    ((v.clamp(0.0, 1.0) * max as f32).round() as usize).min(max)
}

#[inline]
fn braille(bits: u8) -> char {
    char::from_u32(0x2800 + bits as u32).unwrap_or(' ')
}

/// Dots of one braille column (rows `base..base+4`) inside `lo..=hi`.
#[inline]
fn span_bits((lo, hi): (usize, usize), base: usize, dots: &[u8; 4]) -> u8 {
    let mut bits = 0;
    for (i, &d) in dots.iter().enumerate() {
        if (lo..=hi).contains(&(base + i)) {
            bits |= d;
        }
    }
    bits
}

/// Cheap deterministic hash → 0.0..1.0.
#[inline]
fn hash01(x: u32, seed: u32) -> f32 {
    let mut h = x.wrapping_mul(0x9E37_79B9) ^ seed.wrapping_mul(0x85EB_CA6B);
    h ^= h >> 15;
    h = h.wrapping_mul(0x2C1B_3C6D);
    h ^= h >> 12;
    (h >> 8) as f32 / (1u32 << 24) as f32
}

#[cfg(test)]
mod tests {
    use super::*;

    fn render(mode: VisMode, data: VisFrame, w: u16, h: u16) -> Buffer {
        let area = Rect::new(0, 0, w, h);
        let mut buf = Buffer::empty(area);
        Visualizer::from_frame(data, mode, &VisPalette::default()).render(area, &mut buf);
        buf
    }

    fn spectrum(s: &[f32]) -> VisFrame<'_> {
        VisFrame {
            spectrum: s,
            ..VisFrame::default()
        }
    }

    fn sym(buf: &Buffer, x: u16, y: u16) -> &str {
        buf[(x, y)].symbol()
    }

    #[test]
    fn bars_use_eighth_blocks_and_row_colours() {
        let pal = VisPalette::default();
        let buf = render(VisMode::Bars, spectrum(&[1.0, 0.5, 0.0, 0.3]), 4, 2);
        assert_eq!(sym(&buf, 0, 0), "█");
        assert_eq!(sym(&buf, 0, 1), "█");
        assert_eq!(sym(&buf, 1, 0), " ");
        assert_eq!(sym(&buf, 1, 1), "█");
        assert_eq!(sym(&buf, 2, 1), " ");
        // 0.3 * 16 eighths = 4.8 → 5: bottom row ▅.
        assert_eq!(sym(&buf, 3, 1), "▅");
        assert_eq!(buf[(0, 1)].fg, pal.row(0, 2));
        assert_eq!(buf[(0, 0)].fg, pal.row(1, 2));
        assert_ne!(pal.row(0, 2), pal.row(1, 2));
    }

    #[test]
    fn bars_get_gaps_when_wide() {
        // 2 bands in 6 columns → groups of 3: bar, bar, gap.
        let buf = render(VisMode::Bars, spectrum(&[1.0, 1.0]), 6, 1);
        let row: String = (0..6).map(|x| sym(&buf, x, 0)).collect();
        assert_eq!(row, "██ ██ ");
    }

    #[test]
    fn braille_bars_fill_from_the_bottom() {
        let buf = render(VisMode::Braille, spectrum(&[1.0, 0.25]), 1, 1);
        // Left column full (0x47) + right column bottom dot (0x80).
        assert_eq!(sym(&buf, 0, 0), "\u{28C7}");
        let buf = render(VisMode::Braille, spectrum(&[0.5, 0.0]), 1, 2);
        assert_eq!(sym(&buf, 0, 0), " ");
        assert_eq!(sym(&buf, 0, 1), "\u{2847}");
    }

    #[test]
    fn led_draws_segments_and_peak() {
        let pal = VisPalette::from_viscolor(&super::super::palette::WINAMP_VISCOLOR);
        let data = VisFrame {
            spectrum: &[0.5],
            peaks: &[1.0],
            ..VisFrame::default()
        };
        let area = Rect::new(0, 0, 1, 4);
        let mut buf = Buffer::empty(area);
        Visualizer::from_frame(data, VisMode::Led, &pal).render(area, &mut buf);
        assert_eq!(buf[(0, 0)].fg, pal.peak);
        assert_eq!(buf[(0, 1)].fg, pal.grid.unwrap_or_default());
        assert_eq!(buf[(0, 2)].fg, pal.row(1, 4));
        assert_eq!(buf[(0, 3)].fg, pal.row(0, 4));
        for y in 0..4 {
            assert_eq!(sym(&buf, 0, y), "▆");
            assert_eq!(Some(buf[(0, y)].bg), pal.background);
        }
        // Without a grid colour unlit segments are blank.
        let buf = render(VisMode::Led, data, 1, 4);
        assert_eq!(sym(&buf, 0, 1), " ");
    }

    #[test]
    fn mirror_grows_from_the_middle_in_half_cells() {
        let col = |v: f32| -> String {
            let s = [v];
            let buf = render(VisMode::Mirror, spectrum(&s), 1, 4);
            (0..4).map(|y| sym(&buf, 0, y).to_string()).collect()
        };
        assert_eq!(col(1.0), "████");
        assert_eq!(col(0.5), " ██ ");
        assert_eq!(col(0.25), " ▄▀ ");
        assert_eq!(col(0.0), "    ");
        // Lows sit in the centre, highs at both edges.
        let buf = render(VisMode::Mirror, spectrum(&[1.0, 0.0]), 4, 2);
        let row: String = (0..4).map(|x| sym(&buf, x, 0)).collect();
        assert_eq!(row, " ██ ");
    }

    #[test]
    fn scope_draws_a_continuous_braille_line() {
        let flat = VisFrame {
            scope: &[0.0; 8],
            ..VisFrame::default()
        };
        let buf = render(VisMode::Scope, flat, 2, 1);
        assert_eq!(sym(&buf, 0, 0), "\u{2824}");
        assert_eq!(sym(&buf, 1, 0), "\u{2824}");

        // Full-scale step: top row, then bottom row, joined by a vertical run.
        let step = VisFrame {
            scope: &[1.0, -1.0],
            ..VisFrame::default()
        };
        let buf = render(VisMode::Scope, step, 1, 2);
        // Left dot column at the very top; right column spans all 8 rows.
        assert_eq!(
            sym(&buf, 0, 0),
            braille(0x01 | 0x08 | 0x10 | 0x20 | 0x80).to_string()
        );
        assert_eq!(
            sym(&buf, 0, 1),
            braille(0x08 | 0x10 | 0x20 | 0x80).to_string()
        );
    }

    #[test]
    fn spectrogram_packs_two_rows_per_cell() {
        let pal = VisPalette::default();
        // Newest row first: [1.0], then [0.0], then [0.5].
        let rows = [1.0, 0.0, 0.5];
        let data = VisFrame {
            history: HistoryView::from_rows(&rows, 1),
            ..VisFrame::default()
        };
        let buf = render(VisMode::Spectrogram, data, 1, 3);
        assert_eq!(sym(&buf, 0, 0), "▀");
        assert_eq!(buf[(0, 0)].fg, pal.level(1.0));
        assert_eq!(buf[(0, 0)].bg, Color::Reset);
        assert_eq!(sym(&buf, 0, 1), "▀");
        assert_eq!(buf[(0, 1)].fg, pal.level(0.5));
        assert_eq!(sym(&buf, 0, 2), " ");
    }

    #[test]
    fn fire_burns_hot_at_the_base() {
        let pal = VisPalette::default();
        let data = VisFrame {
            spectrum: &[0.5],
            frame: 7,
            ..VisFrame::default()
        };
        let buf = render(VisMode::Fire, data, 1, 10);
        assert_eq!(sym(&buf, 0, 9), "█");
        assert_eq!(buf[(0, 9)].fg, pal.level(1.0));
        assert_eq!(sym(&buf, 0, 0), " ");
        let silent = render(VisMode::Fire, spectrum(&[0.0]), 1, 10);
        assert!((0..10).all(|y| sym(&silent, 0, y) == " "));
    }

    #[test]
    fn every_mode_tolerates_empty_data_and_offset_areas() {
        let mut buf = Buffer::empty(Rect::new(0, 0, 20, 6));
        buf[(0, 0)].set_char('x');
        let pal = VisPalette::default();
        for &mode in VisMode::list() {
            let area = Rect::new(3, 2, 30, 10); // partly outside the buffer
            Visualizer::from_frame(VisFrame::default(), mode, &pal).render(area, &mut buf);
            Visualizer::from_frame(VisFrame::default(), mode, &pal)
                .render(Rect::default(), &mut buf);
        }
        // Cells outside the area are untouched.
        assert_eq!(sym(&buf, 0, 0), "x");
    }
}
