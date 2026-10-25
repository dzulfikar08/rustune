//! Audio visualization: FFT analyzer over the sample [`Tap`](crate::tap::Tap),
//! frame pacing, and ratatui widgets.
//!
//! Typical wiring: keep one [`analyzer::Analyzer`] and one
//! [`pacing::FramePacer`] in the app. On each visualizer tick call
//! `analyzer.update(&tap, dt, playing)`, then draw
//! [`widget::Visualizer::new`]`(&analyzer, mode, &palette)`. Use
//! `pacer.timeout(..)` as the event-poll timeout so nothing redraws while the
//! display is idle. The Winamp compositor reads `analyzer.spectrum()` /
//! `analyzer.scope()` (configure with [`analyzer::AnalyzerConfig::winamp`]).

pub mod analyzer;
pub mod fft;
pub mod pacing;
pub mod palette;
pub mod widget;

use std::fmt;

use serde::{Deserialize, Serialize};

/// Visualizer style. Names (`name`/`from_name`, serde) are stable for config
/// and IPC.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum VisMode {
    /// 1/8-block vertical bars.
    #[default]
    Bars,
    /// Braille bars, two per cell, quarter-cell height steps.
    Braille,
    /// Segmented Winamp-style LED bars with falling peaks.
    Led,
    /// Centre-out bars mirrored horizontally and vertically.
    Mirror,
    /// Braille oscilloscope line.
    Scope,
    /// Scrolling waterfall of past spectra.
    Spectrogram,
    /// Flickering flames following the spectrum.
    Fire,
    /// Visualizer off.
    None,
}

const ALL: [VisMode; 8] = [
    VisMode::Bars,
    VisMode::Braille,
    VisMode::Led,
    VisMode::Mirror,
    VisMode::Scope,
    VisMode::Spectrogram,
    VisMode::Fire,
    VisMode::None,
];

/// Upper bound for [`VisMode::bands_for_width`].
pub const MAX_BANDS: usize = 256;

impl VisMode {
    #[cfg(test)]
    /// All modes in cycling order.
    pub fn list() -> &'static [VisMode] {
        &ALL
    }

    pub fn name(self) -> &'static str {
        match self {
            Self::Bars => "bars",
            Self::Braille => "braille",
            Self::Led => "led",
            Self::Mirror => "mirror",
            Self::Scope => "scope",
            Self::Spectrogram => "spectrogram",
            Self::Fire => "fire",
            Self::None => "none",
        }
    }

    #[cfg(test)]
    /// Case-insensitive inverse of [`name`](Self::name).
    pub fn from_name(name: &str) -> Option<Self> {
        let name = name.trim();
        ALL.iter()
            .copied()
            .find(|m| m.name().eq_ignore_ascii_case(name))
    }

    /// The following mode, wrapping around (includes `None`).
    pub fn next(self) -> Self {
        let i = ALL.iter().position(|&m| m == self).unwrap_or(0);
        ALL[(i + 1) % ALL.len()]
    }

    pub fn is_none(self) -> bool {
        self == Self::None
    }

    /// Analyzer band count that fills `width` cells at this mode's native
    /// resolution. Scope/None return 0 (keep the current band count).
    pub fn bands_for_width(self, width: u16) -> usize {
        let w = width as usize;
        let n = match self {
            // Two-cell bars with a one-cell gap.
            Self::Bars | Self::Led => w / 3,
            Self::Braille => w * 2,
            // Each half of the mirror shows the full range.
            Self::Mirror => w - w / 2,
            Self::Spectrogram | Self::Fire => w,
            Self::Scope | Self::None => return 0,
        };
        n.clamp(1, MAX_BANDS)
    }
}

impl fmt::Display for VisMode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.name())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_round_trip_and_match_serde() {
        for &m in VisMode::list() {
            assert_eq!(VisMode::from_name(m.name()), Some(m));
            assert_eq!(VisMode::from_name(&m.name().to_uppercase()), Some(m));
            let json = serde_json::to_string(&m).unwrap_or_default();
            assert_eq!(json, format!("\"{}\"", m.name()));
            assert_eq!(m.to_string(), m.name());
        }
        assert_eq!(VisMode::from_name(" Fire "), Some(VisMode::Fire));
        assert_eq!(VisMode::from_name("nope"), None);
    }

    #[test]
    fn next_cycles_through_every_mode() {
        let mut m = VisMode::default();
        let mut seen = Vec::new();
        for _ in 0..VisMode::list().len() {
            seen.push(m);
            m = m.next();
        }
        assert_eq!(m, VisMode::default());
        assert_eq!(seen, VisMode::list());
    }

    #[test]
    fn bands_for_width_is_bounded() {
        assert_eq!(VisMode::Bars.bands_for_width(60), 20);
        assert_eq!(VisMode::Braille.bands_for_width(40), 80);
        assert_eq!(VisMode::Mirror.bands_for_width(41), 21);
        assert_eq!(VisMode::Fire.bands_for_width(0), 1);
        assert_eq!(VisMode::Braille.bands_for_width(1000), MAX_BANDS);
        assert_eq!(VisMode::Scope.bands_for_width(80), 0);
    }
}
