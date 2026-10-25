//! ReplayGain tag parsing and gain selection.

use symphonia::core::meta::{StandardTagKey, Tag, Value};

use super::dsp::db_to_gain;
use super::ReplayGainMode;

/// Gains/peaks read from a file's tags.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct ReplayGainTags {
    pub track_db: Option<f32>,
    pub track_peak: Option<f32>,
    pub album_db: Option<f32>,
    pub album_peak: Option<f32>,
}

/// Parse a gain value such as `"-6.54 dB"`, `"+3.2dB"`, `"-6,54 dB"` or `"2"`.
pub fn parse_gain_db(s: &str) -> Option<f32> {
    let s = s.trim();
    let num = s
        .strip_suffix("dB")
        .or_else(|| s.strip_suffix("db"))
        .or_else(|| s.strip_suffix("DB"))
        .unwrap_or(s)
        .trim();
    let num = num.strip_prefix('+').unwrap_or(num);
    let v: f32 = if num.contains(',') && !num.contains('.') {
        num.replace(',', ".").parse().ok()?
    } else {
        num.parse().ok()?
    };
    // Real-world gains are within ±60 dB; anything else is a broken tag.
    (v.is_finite() && v.abs() <= 60.0).then_some(v)
}

/// Parse a linear peak value such as `"0.988547"`.
pub fn parse_peak(s: &str) -> Option<f32> {
    let v: f32 = s.trim().parse().ok()?;
    (v.is_finite() && v > 0.0 && v < 100.0).then_some(v)
}

/// Opus/Vorbis `R128_*_GAIN` is Q7.8 dB relative to -23 LUFS; ReplayGain targets
/// ~-18 LUFS, hence the +5 dB offset.
fn parse_r128(s: &str) -> Option<f32> {
    let q: i32 = s.trim().parse().ok()?;
    Some(q as f32 / 256.0 + 5.0)
}

#[derive(Clone, Copy)]
enum Field {
    TrackGain,
    TrackPeak,
    AlbumGain,
    AlbumPeak,
    R128Track,
    R128Album,
}

fn classify(tag: &Tag) -> Option<Field> {
    match tag.std_key {
        Some(StandardTagKey::ReplayGainTrackGain) => return Some(Field::TrackGain),
        Some(StandardTagKey::ReplayGainTrackPeak) => return Some(Field::TrackPeak),
        Some(StandardTagKey::ReplayGainAlbumGain) => return Some(Field::AlbumGain),
        Some(StandardTagKey::ReplayGainAlbumPeak) => return Some(Field::AlbumPeak),
        _ => {}
    }
    // Keys arrive as e.g. "REPLAYGAIN_TRACK_GAIN", "TXXX:replaygain_track_gain"
    // or "----:com.apple.iTunes:replaygain_track_gain".
    let key = tag.key.as_bytes();
    let ends = |suffix: &str| {
        key.len() >= suffix.len()
            && key[key.len() - suffix.len()..].eq_ignore_ascii_case(suffix.as_bytes())
    };
    if ends("replaygain_track_gain") {
        Some(Field::TrackGain)
    } else if ends("replaygain_track_peak") {
        Some(Field::TrackPeak)
    } else if ends("replaygain_album_gain") {
        Some(Field::AlbumGain)
    } else if ends("replaygain_album_peak") {
        Some(Field::AlbumPeak)
    } else if ends("r128_track_gain") {
        Some(Field::R128Track)
    } else if ends("r128_album_gain") {
        Some(Field::R128Album)
    } else {
        None
    }
}

fn value_text(v: &Value) -> Option<String> {
    match v {
        Value::String(s) => Some(s.clone()),
        Value::Float(f) => Some(f.to_string()),
        Value::SignedInt(i) => Some(i.to_string()),
        Value::UnsignedInt(u) => Some(u.to_string()),
        Value::Binary(b) => std::str::from_utf8(b)
            .ok()
            .map(|s| s.trim_end_matches('\0').to_owned()),
        Value::Boolean(_) | Value::Flag => None,
    }
}

impl ReplayGainTags {
    /// Merge in tags; values already set are kept (first source wins).
    pub fn merge_tags(&mut self, tags: &[Tag]) {
        for tag in tags {
            let Some(field) = classify(tag) else { continue };
            let Some(text) = value_text(&tag.value) else {
                continue;
            };
            match field {
                Field::TrackGain => self.track_db = self.track_db.or(parse_gain_db(&text)),
                Field::TrackPeak => self.track_peak = self.track_peak.or(parse_peak(&text)),
                Field::AlbumGain => self.album_db = self.album_db.or(parse_gain_db(&text)),
                Field::AlbumPeak => self.album_peak = self.album_peak.or(parse_peak(&text)),
                Field::R128Track => self.track_db = self.track_db.or(parse_r128(&text)),
                Field::R128Album => self.album_db = self.album_db.or(parse_r128(&text)),
            }
        }
    }

    /// The gain shown to the user: track gain, else album gain.
    pub fn preferred_db(&self) -> Option<f32> {
        self.track_db.or(self.album_db)
    }

    /// Linear gain for `mode`, falling back to the other gain when one is missing
    /// and limited so the tagged peak does not clip. 1.0 without tags.
    pub fn gain(&self, mode: ReplayGainMode) -> f32 {
        let (db, peak) = match mode {
            ReplayGainMode::Off => return 1.0,
            ReplayGainMode::Track => match self.track_db {
                Some(db) => (db, self.track_peak),
                None => (self.album_db.unwrap_or(0.0), self.album_peak),
            },
            ReplayGainMode::Album => match self.album_db {
                Some(db) => (db, self.album_peak),
                None => (self.track_db.unwrap_or(0.0), self.track_peak),
            },
        };
        let g = db_to_gain(db);
        match peak {
            Some(p) if p * g > 1.0 => 1.0 / p,
            _ => g,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_gain_strings() {
        assert_eq!(parse_gain_db("-6.54 dB"), Some(-6.54));
        assert_eq!(parse_gain_db("+3.20 dB"), Some(3.2));
        assert_eq!(parse_gain_db("  -0.5dB "), Some(-0.5));
        assert_eq!(parse_gain_db("-6,54 dB"), Some(-6.54));
        assert_eq!(parse_gain_db("2"), Some(2.0));
        assert_eq!(parse_gain_db("1.5 db"), Some(1.5));
        assert_eq!(parse_gain_db(""), None);
        assert_eq!(parse_gain_db("loud"), None);
        assert_eq!(parse_gain_db("NaN dB"), None);
        assert_eq!(parse_gain_db("-900 dB"), None);
        assert_eq!(parse_peak("0.988547"), Some(0.988547));
        assert_eq!(parse_peak("-1"), None);
        assert_eq!(parse_r128("-1280"), Some(0.0));
    }

    #[test]
    fn reads_tags_by_std_key_and_by_name() {
        let tags = vec![
            Tag::new(
                Some(StandardTagKey::ReplayGainTrackGain),
                "REPLAYGAIN_TRACK_GAIN",
                Value::from("-6.54 dB"),
            ),
            Tag::new(None, "TXXX:replaygain_track_peak", Value::from("0.95")),
            Tag::new(
                None,
                "----:com.apple.iTunes:replaygain_album_gain",
                Value::Binary(Box::from(&b"-8.00 dB\0"[..])),
            ),
            Tag::new(None, "TITLE", Value::from("-1 dB")),
        ];
        let mut rg = ReplayGainTags::default();
        rg.merge_tags(&tags);
        assert_eq!(
            rg,
            ReplayGainTags {
                track_db: Some(-6.54),
                track_peak: Some(0.95),
                album_db: Some(-8.0),
                album_peak: None,
            }
        );
        assert_eq!(rg.preferred_db(), Some(-6.54));
    }

    #[test]
    fn gain_selection_fallback_and_clip_prevention() {
        let rg = ReplayGainTags {
            track_db: Some(-6.0),
            track_peak: None,
            album_db: None,
            album_peak: None,
        };
        assert_eq!(rg.gain(ReplayGainMode::Off), 1.0);
        assert!((rg.gain(ReplayGainMode::Track) - 0.501_187).abs() < 1e-5);
        // Album mode falls back to the track gain.
        assert_eq!(
            rg.gain(ReplayGainMode::Album),
            rg.gain(ReplayGainMode::Track)
        );

        // +6 dB on a track peaking at 0.8 would clip: limited to 1 / 0.8.
        let loud = ReplayGainTags {
            track_db: Some(6.0),
            track_peak: Some(0.8),
            ..Default::default()
        };
        assert!((loud.gain(ReplayGainMode::Track) - 1.25).abs() < 1e-6);

        assert_eq!(ReplayGainTags::default().gain(ReplayGainMode::Track), 1.0);
    }
}
