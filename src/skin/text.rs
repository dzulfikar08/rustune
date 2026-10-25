//! Text assets inside a skin: PLEDIT.TXT, VISCOLOR.TXT and the various
//! metadata files (GENEX.INI, SKIN.INI, SKIN.XML, README/INFO).
//! Parsing follows webamp's `parseIni` / `parseViscolors`
//! (https://github.com/captbaritone/webamp/blob/master/packages/webamp/js/utils.ts).

use super::bmp::Rgba;

/// Playlist editor colours from `PLEDIT.TXT` `[Text]`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PleditColors {
    pub normal: Rgba,
    pub current: Rgba,
    pub normal_bg: Rgba,
    pub selected_bg: Rgba,
    /// Font face requested by the skin (informational; terminals pick their own).
    pub font: Option<String>,
}

impl Default for PleditColors {
    /// Winamp 2.91 base skin values.
    fn default() -> Self {
        Self {
            normal: [0x00, 0xFF, 0x00, 255],
            current: [0xFF, 0xFF, 0xFF, 255],
            normal_bg: [0x00, 0x00, 0x00, 255],
            selected_bg: [0x00, 0x00, 0xC6, 255],
            font: None,
        }
    }
}

/// Winamp 2.91 base skin VISCOLOR.TXT.
pub const DEFAULT_VISCOLOR: [Rgba; 24] = [
    [0, 0, 0, 255],
    [24, 33, 41, 255],
    [239, 49, 16, 255],
    [206, 41, 16, 255],
    [214, 90, 0, 255],
    [214, 102, 0, 255],
    [214, 115, 0, 255],
    [198, 123, 8, 255],
    [222, 165, 24, 255],
    [214, 181, 33, 255],
    [189, 222, 41, 255],
    [148, 222, 33, 255],
    [41, 206, 16, 255],
    [50, 190, 16, 255],
    [57, 181, 16, 255],
    [49, 156, 8, 255],
    [41, 148, 0, 255],
    [24, 132, 8, 255],
    [255, 255, 255, 255],
    [214, 214, 222, 255],
    [181, 189, 189, 255],
    [160, 170, 175, 255],
    [148, 156, 165, 255],
    [150, 150, 150, 255],
];

/// Parse `#RRGGBB` / `RRGGBB` (surrounding junk tolerated).
pub fn parse_hex_color(s: &str) -> Option<Rgba> {
    let s = s.trim().trim_matches(|c| c == '"' || c == '\'').trim();
    let s = s.strip_prefix('#').unwrap_or(s);
    let hex = s.get(..6)?;
    if !hex.bytes().all(|b| b.is_ascii_hexdigit()) {
        return None;
    }
    let v = u32::from_str_radix(hex, 16).ok()?;
    Some([(v >> 16) as u8, (v >> 8) as u8, v as u8, 255])
}

/// Parse PLEDIT.TXT. Unknown or malformed keys keep base-skin defaults.
pub fn parse_pledit(bytes: &[u8]) -> PleditColors {
    let text = String::from_utf8_lossy(bytes);
    let mut out = PleditColors::default();
    let mut in_text = false;
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with(';') {
            continue;
        }
        if let Some(section) = line.strip_prefix('[').and_then(|l| l.strip_suffix(']')) {
            in_text = section.trim().eq_ignore_ascii_case("text");
            continue;
        }
        if !in_text {
            continue;
        }
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        // webamp: ignore anything after a second '='.
        let value = value.split('=').next().unwrap_or("").trim();
        let key = key.trim();
        let slot = if key.eq_ignore_ascii_case("normal") {
            &mut out.normal
        } else if key.eq_ignore_ascii_case("current") {
            &mut out.current
        } else if key.eq_ignore_ascii_case("normalbg") {
            &mut out.normal_bg
        } else if key.eq_ignore_ascii_case("selectedbg") {
            &mut out.selected_bg
        } else {
            if key.eq_ignore_ascii_case("font") {
                let font = value.trim_matches(|c| c == '"' || c == '\'').trim();
                if !font.is_empty() {
                    out.font = Some(font.to_string());
                }
            }
            continue;
        };
        if let Some(c) = parse_hex_color(value) {
            *slot = c;
        }
    }
    out
}

/// Parse VISCOLOR.TXT: up to 24 lines of `r,g,b` (commas optional, trailing
/// comments ignored). Lines that don't start with three numbers are skipped,
/// exactly like webamp's regex `^\s*(\d+)\s*,?\s*(\d+)\s*,?\s*(\d+)`.
pub fn parse_viscolor(bytes: &[u8]) -> [Rgba; 24] {
    let text = String::from_utf8_lossy(bytes);
    let mut colors = DEFAULT_VISCOLOR;
    let mut i = 0;
    for line in text.lines() {
        if i >= colors.len() {
            break;
        }
        if let Some(rgb) = leading_triplet(line) {
            colors[i] = [rgb[0], rgb[1], rgb[2], 255];
            i += 1;
        }
    }
    colors
}

fn leading_triplet(line: &str) -> Option<[u8; 3]> {
    let b = line.as_bytes();
    let mut pos = 0;
    let mut out = [0u8; 3];
    for (n, slot) in out.iter_mut().enumerate() {
        while pos < b.len() && b[pos].is_ascii_whitespace() {
            pos += 1;
        }
        if n > 0 && pos < b.len() && b[pos] == b',' {
            pos += 1;
            while pos < b.len() && b[pos].is_ascii_whitespace() {
                pos += 1;
            }
        }
        let start = pos;
        let mut v: u32 = 0;
        while pos < b.len() && b[pos].is_ascii_digit() {
            v = v
                .saturating_mul(10)
                .saturating_add(u32::from(b[pos] - b'0'));
            pos += 1;
        }
        if pos == start {
            return None;
        }
        *slot = v.min(255) as u8;
    }
    Some(out)
}

/// (name, author, description) from the first metadata file present.
pub fn parse_metadata<'a>(
    get: impl Fn(&str) -> Option<&'a [u8]>,
    fallback_name: &str,
) -> (String, Option<String>, Option<String>) {
    for key in METADATA_FILES {
        if let Some(bytes) = get(key) {
            let s = String::from_utf8_lossy(bytes);
            let (n, a, d) = match key {
                "SKIN.XML" => parse_xmlish_metadata(&s),
                _ => parse_iniish_metadata(&s),
            };
            let clean =
                |v: Option<String>| v.map(|v| v.trim().to_string()).filter(|v| !v.is_empty());
            let name = clean(n).unwrap_or_else(|| fallback_name.to_string());
            return (name, clean(a), clean(d));
        }
    }
    (fallback_name.to_string(), None, None)
}

/// Metadata files, in priority order (upper-case basenames).
pub const METADATA_FILES: [&str; 5] = [
    "GENEX.INI",
    "SKIN.INI",
    "SKIN.XML",
    "README.TXT",
    "INFO.TXT",
];

fn parse_iniish_metadata(s: &str) -> (Option<String>, Option<String>, Option<String>) {
    let mut name: Option<String> = None;
    let mut author: Option<String> = None;
    let mut desc: Option<String> = None;

    for raw in s.lines() {
        let line = raw.trim();
        if line.is_empty() || line.starts_with(';') || line.starts_with('#') {
            continue;
        }
        if line.starts_with('[') && line.ends_with(']') {
            continue;
        }
        let Some((k, v)) = line.split_once('=') else {
            continue;
        };
        let key = k.trim().to_lowercase();
        let val = v.trim().trim_matches('"').trim_matches('\'').trim();
        if val.is_empty() {
            continue;
        }
        let slot = match key.as_str() {
            "name" | "skinname" | "title" | "skin_title" => &mut name,
            "author" | "skin_author" => &mut author,
            "comment" | "description" | "skin_description" => &mut desc,
            _ => continue,
        };
        if slot.is_none() {
            *slot = Some(val.to_string());
        }
    }

    // README/INFO files are often prose; take the first line as the name.
    if name.is_none() {
        if let Some(first) = s.lines().map(str::trim).find(|l| !l.is_empty()) {
            let lower = first.to_lowercase();
            if !lower.starts_with("readme") && !lower.starts_with("skin") && first.len() <= 80 {
                name = Some(first.to_string());
            }
        }
    }
    (name, author, desc)
}

fn parse_xmlish_metadata(s: &str) -> (Option<String>, Option<String>, Option<String>) {
    // Case-insensitive tag extraction; byte offsets are shared because ASCII
    // lower-casing preserves lengths.
    let lower = s.to_ascii_lowercase();
    let between = |open: &str, close: &str| -> Option<String> {
        let start = lower.find(open)? + open.len();
        let end = start + lower[start..].find(close)?;
        s.get(start..end).map(|v| v.trim().to_string())
    };
    (
        between("<name>", "</name>"),
        between("<author>", "</author>"),
        between("<description>", "</description>"),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pledit_reads_all_text_keys_case_insensitively() {
        let txt = b"[Text]\r\nNormal=#00FF00\r\ncurrent=FFFFFF\r\nNormalBG=#000000\r\nSELECTEDBG=#0000C6\r\nFont=Arial\r\nmbBG=#000000\r\n";
        let c = parse_pledit(txt);
        assert_eq!(c.normal, [0, 255, 0, 255]);
        assert_eq!(c.current, [255, 255, 255, 255]);
        assert_eq!(c.normal_bg, [0, 0, 0, 255]);
        assert_eq!(c.selected_bg, [0, 0, 0xC6, 255]);
        assert_eq!(c.font.as_deref(), Some("Arial"));
    }

    #[test]
    fn pledit_ignores_other_sections_and_bad_values() {
        let txt = b"[Other]\nNormal=#123456\n[text]\n; comment\nNormal = #ABCDEF ; trailing\nCurrent=zzz\n";
        let c = parse_pledit(txt);
        assert_eq!(c.normal, [0xAB, 0xCD, 0xEF, 255]);
        assert_eq!(c.current, PleditColors::default().current);
        assert_eq!(c.font, None);
    }

    #[test]
    fn viscolor_parses_lines_with_and_without_commas() {
        let txt = b"0,0,0, // background\n 24 , 33 , 41 // dots\n\nnot a colour\n255 255 300\n";
        let c = parse_viscolor(txt);
        assert_eq!(c[0], [0, 0, 0, 255]);
        assert_eq!(c[1], [24, 33, 41, 255]);
        assert_eq!(c[2], [255, 255, 255, 255]); // 300 clamps
        assert_eq!(c[3], DEFAULT_VISCOLOR[3]); // missing → default
    }

    #[test]
    fn viscolor_stops_after_24_entries() {
        let mut txt = String::new();
        for i in 0..30 {
            txt.push_str(&format!("{i},{i},{i}\n"));
        }
        let c = parse_viscolor(txt.as_bytes());
        assert_eq!(c[23], [23, 23, 23, 255]);
    }

    #[test]
    fn hex_colors() {
        assert_eq!(parse_hex_color("#0a0B0c"), Some([10, 11, 12, 255]));
        assert_eq!(parse_hex_color("\"FF0000\""), Some([255, 0, 0, 255]));
        assert_eq!(parse_hex_color("#12"), None);
        assert_eq!(parse_hex_color("#GG0000"), None);
    }

    #[test]
    fn metadata_priority_and_xml() {
        let xml = b"<skin><Name>Cool</Name><AUTHOR>Me</AUTHOR></skin>".as_slice();
        let get = |k: &str| if k == "SKIN.XML" { Some(xml) } else { None };
        let (n, a, d) = parse_metadata(get, "fallback");
        assert_eq!(n, "Cool");
        assert_eq!(a.as_deref(), Some("Me"));
        assert_eq!(d, None);
        let (n, _, _) = parse_metadata(|_| None, "fallback");
        assert_eq!(n, "fallback");
    }
}
