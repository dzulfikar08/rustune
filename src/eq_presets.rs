//! Winamp's built-in equalizer presets (bands in dB, 60 Hz .. 16 kHz),
//! clamped to the player's ±12 dB range when applied.

use crate::view::{EqSettings, EQ_BANDS, EQ_MAX_DB};

pub const PRESETS: &[(&str, [f32; EQ_BANDS])] = &[
    ("Flat", [0.0; EQ_BANDS]),
    (
        "Classical",
        [0.0, 0.0, 0.0, 0.0, 0.0, 0.0, -7.2, -7.2, -7.2, -9.6],
    ),
    ("Club", [0.0, 0.0, 8.0, 5.6, 5.6, 5.6, 3.2, 0.0, 0.0, 0.0]),
    (
        "Dance",
        [9.6, 7.2, 2.4, 0.0, 0.0, -5.6, -7.2, -7.2, 0.0, 0.0],
    ),
    (
        "Full Bass",
        [-8.0, 9.6, 9.6, 5.6, 1.6, -4.0, -8.0, -10.4, -11.2, -11.2],
    ),
    (
        "Full Bass & Treble",
        [7.2, 5.6, 0.0, -7.2, -4.8, 1.6, 8.0, 11.2, 12.0, 12.0],
    ),
    (
        "Full Treble",
        [-9.6, -9.6, -9.6, -4.0, 2.4, 11.2, 12.0, 12.0, 12.0, 12.0],
    ),
    (
        "Laptop Speakers/Headphones",
        [4.8, 11.2, 5.6, -3.2, -2.4, 1.6, 4.8, 9.6, 12.0, 12.0],
    ),
    (
        "Large Hall",
        [10.4, 10.4, 5.6, 5.6, 0.0, -4.8, -4.8, -4.8, 0.0, 0.0],
    ),
    ("Live", [-4.8, 0.0, 4.0, 5.6, 5.6, 5.6, 4.0, 2.4, 2.4, 2.4]),
    ("Party", [7.2, 7.2, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 7.2, 7.2]),
    (
        "Pop",
        [-1.6, 4.8, 7.2, 8.0, 5.6, 0.0, -2.4, -2.4, -1.6, -1.6],
    ),
    (
        "Reggae",
        [0.0, 0.0, 0.0, -5.6, 0.0, 6.4, 6.4, 0.0, 0.0, 0.0],
    ),
    (
        "Rock",
        [8.0, 4.8, -5.6, -8.0, -3.2, 4.0, 8.8, 11.2, 11.2, 11.2],
    ),
    (
        "Ska",
        [-2.4, -4.8, -4.0, 0.0, 4.0, 5.6, 8.8, 9.6, 11.2, 9.6],
    ),
    (
        "Soft",
        [4.8, 1.6, 0.0, -2.4, 0.0, 4.0, 8.0, 9.6, 11.2, 12.0],
    ),
    (
        "Soft Rock",
        [4.0, 4.0, 2.4, 0.0, -4.0, -5.6, -3.2, 0.0, 2.4, 8.8],
    ),
    (
        "Techno",
        [8.0, 5.6, 0.0, -5.6, -4.8, 0.0, 8.0, 9.6, 9.6, 8.8],
    ),
];

/// Apply the named preset (case-insensitive) to the band gains, leaving the
/// preamp and on/off state alone. Returns `false` for an unknown name.
pub fn apply(eq: &mut EqSettings, name: &str) -> bool {
    let Some((_, bands)) = PRESETS
        .iter()
        .find(|(preset, _)| preset.eq_ignore_ascii_case(name.trim()))
    else {
        return false;
    };
    for (dst, src) in eq.bands_db.iter_mut().zip(bands) {
        *dst = src.clamp(-EQ_MAX_DB, EQ_MAX_DB);
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn presets_are_within_range_and_unique() {
        for (i, (name, bands)) in PRESETS.iter().enumerate() {
            assert!(bands.iter().all(|b| b.abs() <= EQ_MAX_DB), "{name}");
            assert!(
                PRESETS[i + 1..]
                    .iter()
                    .all(|(other, _)| !other.eq_ignore_ascii_case(name)),
                "{name}"
            );
        }
    }

    #[test]
    fn apply_by_name() {
        let mut eq = EqSettings {
            preamp_db: 3.0,
            ..EqSettings::default()
        };
        assert!(apply(&mut eq, " rock "));
        assert_eq!(eq.bands_db[0], 8.0);
        assert_eq!(eq.preamp_db, 3.0);
        assert!(apply(&mut eq, "FLAT"));
        assert!(eq.bands_db.iter().all(|b| *b == 0.0));
        assert!(!apply(&mut eq, "nope"));
    }
}
