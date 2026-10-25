//! Colours for the visualizer widgets, built from a theme or a Winamp
//! `VISCOLOR.TXT` table.

use ratatui::style::Color;

/// Number of intensity steps in [`VisPalette::gradient`] (Winamp uses 16).
pub const GRADIENT_STEPS: usize = 16;

/// The stock Winamp 2.x base-skin `VISCOLOR.TXT`.
pub const WINAMP_VISCOLOR: [[u8; 3]; 24] = [
    [0, 0, 0],
    [24, 33, 41],
    [239, 49, 16],
    [206, 41, 16],
    [214, 90, 0],
    [214, 102, 0],
    [214, 115, 0],
    [198, 123, 8],
    [222, 165, 24],
    [214, 181, 33],
    [189, 222, 41],
    [148, 222, 33],
    [41, 206, 16],
    [50, 190, 16],
    [57, 181, 16],
    [49, 156, 8],
    [41, 148, 0],
    [24, 132, 8],
    [255, 255, 255],
    [214, 214, 222],
    [181, 189, 189],
    [160, 170, 175],
    [148, 156, 165],
    [150, 150, 150],
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VisPalette {
    /// Intensity ramp: index 0 = quietest/bottom, last = loudest/top.
    pub gradient: [Color; GRADIENT_STEPS],
    /// Peak-hold markers.
    pub peak: Color,
    /// Oscilloscope trace.
    pub scope: Color,
    /// Unlit LED segments; `None` leaves them blank.
    pub grid: Option<Color>,
    /// Cell background; `None` keeps the surrounding theme's background.
    pub background: Option<Color>,
}

impl Default for VisPalette {
    /// Winamp colours over the terminal's own background.
    fn default() -> Self {
        Self {
            grid: None,
            background: None,
            ..Self::from_viscolor(&WINAMP_VISCOLOR)
        }
    }
}

impl VisPalette {
    /// Map a Winamp `VISCOLOR.TXT` table: 0 background, 1 grid dots,
    /// 2..=17 analyzer (top → bottom), 18..=22 oscilloscope, 23 peaks.
    pub fn from_viscolor(c: &[[u8; 3]; 24]) -> Self {
        let rgb = |[r, g, b]: [u8; 3]| Color::Rgb(r, g, b);
        let mut gradient = [Color::Reset; GRADIENT_STEPS];
        for (i, g) in gradient.iter_mut().enumerate() {
            *g = rgb(c[17 - i]);
        }
        Self {
            gradient,
            peak: rgb(c[23]),
            scope: rgb(c[18]),
            grid: Some(rgb(c[1])),
            background: Some(rgb(c[0])),
        }
    }

    #[cfg(test)]
    /// Ramp from `low` through `mid` to `high`. RGB stops are interpolated;
    /// indexed/named colours switch at the thirds.
    pub fn from_stops(low: Color, mid: Color, high: Color) -> Self {
        let mut gradient = [Color::Reset; GRADIENT_STEPS];
        let last = (GRADIENT_STEPS - 1) as f32;
        for (i, g) in gradient.iter_mut().enumerate() {
            let t = i as f32 / last;
            *g = if t <= 0.5 {
                lerp(low, mid, t * 2.0)
            } else {
                lerp(mid, high, t * 2.0 - 1.0)
            };
        }
        Self {
            gradient,
            peak: high,
            scope: mid,
            grid: None,
            background: None,
        }
    }

    /// Colour for a 0.0..=1.0 level.
    pub fn level(&self, v: f32) -> Color {
        let i = (v.clamp(0.0, 1.0) * GRADIENT_STEPS as f32) as usize;
        self.gradient[i.min(GRADIENT_STEPS - 1)]
    }

    /// Colour for cell row `row` (0 = bottom) of a `rows`-tall column, so
    /// bars keep a fixed colour per height like Winamp's analyzer.
    pub fn row(&self, row: usize, rows: usize) -> Color {
        let rows = rows.max(1);
        let i = (2 * row + 1) * GRADIENT_STEPS / (2 * rows);
        self.gradient[i.min(GRADIENT_STEPS - 1)]
    }
}

#[cfg(test)]
fn lerp(a: Color, b: Color, t: f32) -> Color {
    match (a, b) {
        (Color::Rgb(ar, ag, ab), Color::Rgb(br, bg, bb)) => {
            let mix = |x: u8, y: u8| (x as f32 + (y as f32 - x as f32) * t).round() as u8;
            Color::Rgb(mix(ar, br), mix(ag, bg), mix(ab, bb))
        }
        _ if t < 0.5 => a,
        _ => b,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn viscolor_maps_bottom_to_gradient_start() {
        let p = VisPalette::from_viscolor(&WINAMP_VISCOLOR);
        assert_eq!(p.gradient[0], Color::Rgb(24, 132, 8)); // colour 17, bottom
        assert_eq!(p.gradient[15], Color::Rgb(239, 49, 16)); // colour 2, top
        assert_eq!(p.peak, Color::Rgb(150, 150, 150));
        assert_eq!(p.background, Some(Color::Rgb(0, 0, 0)));
        assert_eq!(VisPalette::default().background, None);
    }

    #[test]
    fn stops_interpolate_rgb_and_hit_endpoints() {
        let p = VisPalette::from_stops(
            Color::Rgb(0, 0, 0),
            Color::Rgb(100, 100, 100),
            Color::Rgb(200, 0, 0),
        );
        assert_eq!(p.gradient[0], Color::Rgb(0, 0, 0));
        assert_eq!(p.gradient[15], Color::Rgb(200, 0, 0));
        let named = VisPalette::from_stops(Color::Green, Color::Yellow, Color::Red);
        assert_eq!(named.gradient[0], Color::Green);
        assert_eq!(named.gradient[15], Color::Red);
    }

    #[test]
    fn level_and_row_are_clamped() {
        let p = VisPalette::default();
        assert_eq!(p.level(-1.0), p.gradient[0]);
        assert_eq!(p.level(1.0), p.gradient[15]);
        assert_eq!(p.row(0, 1), p.gradient[8]);
        assert_eq!(p.row(0, 16), p.gradient[0]);
        assert_eq!(p.row(15, 16), p.gradient[15]);
        assert_eq!(p.row(99, 4), p.gradient[15]);
    }
}
