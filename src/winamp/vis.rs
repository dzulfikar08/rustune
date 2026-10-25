//! Classic Winamp visualizer: 19-bar spectrum analyzer with falling peaks and
//! the line oscilloscope, drawn with VISCOLOR.TXT colours into the 76x16 area.
//! Behaviour follows webamp's `VisPainter.ts` (bar falloff "moderate",
//! peak falloff "slow", wide bandwidth: 3px bars with a 1px gap).

use crate::skin::Rgba;

use super::canvas::Canvas;

pub const VIS_W: i32 = 76;
pub const VIS_H: i32 = 16;
pub const BARS: usize = 19;

/// Webamp animates at the display refresh rate; its constants are per 60 Hz frame.
const STEP: f32 = 1.0 / 60.0;
/// Bar falloff per step, in pixels ("moderate": 12/16).
const BAR_FALLOFF: f32 = 0.75;
/// Peak fall speed starts at 3/256 px per step and accelerates ×1.1 ("slow").
const PEAK_START_SPEED: f32 = 3.0 / 256.0;
const PEAK_ACCEL: f32 = 1.1;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum VisMode {
    #[default]
    Analyzer,
    Oscilloscope,
    Off,
}

impl VisMode {
    /// Click cycle, as in Winamp.
    pub fn next(self) -> Self {
        match self {
            Self::Analyzer => Self::Oscilloscope,
            Self::Oscilloscope => Self::Off,
            Self::Off => Self::Analyzer,
        }
    }
}

/// Analyzer dynamics; fixed-size, no allocation.
#[derive(Debug, Clone)]
pub struct VisState {
    bars: [f32; BARS],
    peaks: [f32; BARS],
    peak_speed: [f32; BARS],
    accum: f32,
}

impl Default for VisState {
    fn default() -> Self {
        Self {
            bars: [0.0; BARS],
            peaks: [0.0; BARS],
            peak_speed: [0.0; BARS],
            accum: 0.0,
        }
    }
}

/// Average `spectrum` into `BARS` buckets (low → high), scaled to 0..=16 px.
pub fn bucket(spectrum: &[f32], out: &mut [f32; BARS]) {
    let n = spectrum.len();
    if n == 0 {
        *out = [0.0; BARS];
        return;
    }
    for (i, o) in out.iter_mut().enumerate() {
        let start = i * n / BARS;
        let end = ((i + 1) * n / BARS).max(start + 1).min(n);
        let slice = &spectrum[start.min(n - 1)..end.max(start.min(n - 1) + 1)];
        let sum: f32 = slice.iter().map(|v| v.clamp(0.0, 1.0)).sum();
        *o = sum / slice.len() as f32 * VIS_H as f32;
    }
}

impl VisState {
    /// Advance by `dt` seconds towards the target spectrum.
    pub fn update(&mut self, spectrum: &[f32], dt: f32) {
        let mut target = [0.0f32; BARS];
        bucket(spectrum, &mut target);
        self.accum = (self.accum + dt.max(0.0)).min(1.0);
        while self.accum >= STEP {
            self.accum -= STEP;
            self.step(&target);
        }
    }

    fn step(&mut self, target: &[f32; BARS]) {
        for i in 0..BARS {
            self.bars[i] = (self.bars[i] - BAR_FALLOFF).max(target[i]).max(0.0);
            if self.bars[i] >= self.peaks[i] {
                self.peaks[i] = self.bars[i];
                self.peak_speed[i] = PEAK_START_SPEED;
            } else {
                self.peaks[i] = (self.peaks[i] - self.peak_speed[i]).max(0.0);
                self.peak_speed[i] *= PEAK_ACCEL;
            }
        }
    }

    /// Bar height in whole pixels (0..=16).
    pub fn bar(&self, i: usize) -> i32 {
        self.bars[i].round().clamp(0.0, VIS_H as f32) as i32
    }

    pub fn peak(&self, i: usize) -> i32 {
        self.peaks[i].round().clamp(0.0, VIS_H as f32) as i32
    }

    pub fn reset(&mut self) {
        *self = Self::default();
    }
}

/// Background: colour 0 with colour 1 dots on even columns / odd rows.
pub fn draw_background(c: &mut Canvas, x0: i32, y0: i32, colors: &[Rgba; 24]) {
    for y in 0..VIS_H {
        for x in 0..VIS_W {
            let dot = x % 2 == 0 && y % 2 == 1;
            c.set(x0 + x, y0 + y, if dot { colors[1] } else { colors[0] });
        }
    }
}

/// Analyzer bars: row `y` (0 = top) uses colour `2 + y`; peaks use colour 23.
pub fn draw_analyzer(c: &mut Canvas, x0: i32, y0: i32, colors: &[Rgba; 24], state: &VisState) {
    for i in 0..BARS {
        let h = state.bar(i);
        let bx = x0 + i as i32 * 4;
        for y in (VIS_H - h)..VIS_H {
            let col = colors[2 + y as usize];
            for dx in 0..3 {
                c.set(bx + dx, y0 + y, col);
            }
        }
        // The peak floats one row above its level, as in Winamp.
        let p = state.peak(i);
        let py = VIS_H - p - 1;
        if p >= 1 && py >= 0 {
            for dx in 0..3 {
                c.set(bx + dx, y0 + py, colors[23]);
            }
        }
    }
}

/// Oscilloscope row for a sample in -1..=1 (webamp maps bytes 0..255 via
/// `round(b / 8) - 9`, clamped to the 16 rows).
pub fn scope_row(sample: f32) -> i32 {
    let byte = (sample.clamp(-1.0, 1.0) * 127.5 + 127.5).round();
    ((byte / 8.0).round() as i32 - 9).clamp(0, VIS_H - 1)
}

/// webamp `colorIndex`: brighter scope colours near the centre line.
fn scope_color_index(y: i32) -> usize {
    if y >= 14 {
        4
    } else if y >= 12 {
        3
    } else if y >= 10 {
        2
    } else if y >= 8 {
        1
    } else if y >= 6 {
        0
    } else if y >= 4 {
        1
    } else if y >= 2 {
        2
    } else {
        3
    }
}

/// Line oscilloscope across 76 columns.
pub fn draw_scope(c: &mut Canvas, x0: i32, y0: i32, colors: &[Rgba; 24], scope: &[f32]) {
    if scope.is_empty() {
        return;
    }
    let mut last = 0;
    for x in 0..VIS_W {
        let idx = x as usize * scope.len() / VIS_W as usize;
        let y = scope_row(scope[idx.min(scope.len() - 1)]);
        if x == 0 {
            last = y;
        }
        let (mut top, bottom) = if y <= last { (y, last) } else { (last, y) };
        if y > last {
            // Descending segments start one row lower, as in Winamp.
            top += 1;
        }
        let col = colors[18 + scope_color_index(y)];
        for yy in top..=bottom.max(top) {
            c.set(x0 + x, y0 + yy, col);
        }
        last = y;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::skin::text::DEFAULT_VISCOLOR;

    #[test]
    fn bucketing_averages_and_scales() {
        let mut out = [0.0; BARS];
        bucket(&[1.0; 38], &mut out);
        assert!(out.iter().all(|v| (*v - 16.0).abs() < 1e-4));
        bucket(&[], &mut out);
        assert!(out.iter().all(|v| *v == 0.0));
        // Fewer bins than bars: each bar samples a bin, no panic.
        bucket(&[0.5, 1.0], &mut out);
        assert!((out[0] - 8.0).abs() < 1e-4);
        assert!((out[BARS - 1] - 16.0).abs() < 1e-4);
        // Out-of-range input is clamped.
        bucket(&[4.0; 19], &mut out);
        assert!((out[3] - 16.0).abs() < 1e-4);
    }

    #[test]
    fn bars_jump_up_and_fall_at_constant_rate() {
        let mut s = VisState::default();
        s.update(&[1.0; 19], STEP);
        assert_eq!(s.bar(0), 16);
        // 10 frames of silence: 16 - 7.5 = 8.5 → rounds to 9 (half away from zero).
        s.update(&[0.0; 19], STEP * 10.0 + 1e-4);
        assert_eq!(s.bar(0), 9);
        // Peak lags behind the bar.
        assert!(s.peak(0) > s.bar(0));
        s.update(&[0.0; 19], 1.0);
        s.update(&[0.0; 19], 1.0);
        assert_eq!(s.bar(0), 0);
        assert_eq!(s.peak(0), 0);
    }

    #[test]
    fn scope_mapping_matches_webamp() {
        assert_eq!(scope_row(-1.0), 0);
        assert_eq!(scope_row(1.0), 15);
        assert_eq!(scope_row(0.0), 7);
    }

    #[test]
    fn analyzer_paints_bar_colours_and_peak() {
        let mut c = Canvas::new();
        c.resize(76, 16);
        let colors = DEFAULT_VISCOLOR;
        draw_background(&mut c, 0, 0, &colors);
        assert_eq!(c.get(0, 1), Some(colors[1]));
        assert_eq!(c.get(1, 1), Some(colors[0]));
        let mut s = VisState::default();
        let mut spectrum = [0.0; 19];
        spectrum[0] = 0.5; // 8 px
        s.update(&spectrum, STEP);
        draw_analyzer(&mut c, 0, 0, &colors, &s);
        assert_eq!(c.get(0, 15), Some(colors[17]));
        assert_eq!(c.get(0, 8), Some(colors[10])); // top row of an 8px bar
        assert_eq!(c.get(2, 7), Some(colors[23])); // peak floats above the bar
        assert_eq!(c.get(3, 15), Some(colors[0])); // gap column untouched
    }
}
