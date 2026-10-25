//! Winamp 2.x skin (`.wsz`) loader.
//!
//! Every sprite sheet is decoded to RGBA exactly once at load time and owned
//! by [`WinampSkin`]; renderers borrow them. Missing or undecodable sheets are
//! simply `None` and the compositor skips the elements they would draw.
//!
//! Sheets (sprite coordinates follow webamp's `skinSprites.ts`):
//!   MAIN, TITLEBAR, CBUTTONS, SHUFREP, TEXT, NUMBERS, NUMS_EX, VOLUME,
//!   BALANCE (falls back to VOLUME), POSBAR, PLAYPAUS, MONOSTER, EQMAIN,
//!   EQ_EX, PLEDIT.
//! Text files: PLEDIT.TXT, VISCOLOR.TXT and metadata (GENEX.INI, SKIN.INI,
//! SKIN.XML, README.TXT, INFO.TXT).

pub mod bmp;
pub mod text;

use std::fmt;
use std::io::{Read, Seek};
use std::path::Path;

use ratatui::style::Color;

pub use bmp::{Bitmap, Rgba};
pub use text::PleditColors;

/// Sprite sheets a classic skin may contain.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Sheet {
    Main,
    Titlebar,
    Cbuttons,
    Shufrep,
    Text,
    Numbers,
    NumsEx,
    Volume,
    Balance,
    Posbar,
    Playpaus,
    Monoster,
    Eqmain,
    EqEx,
    Pledit,
}

impl Sheet {
    pub const COUNT: usize = 15;
    pub const ALL: [Sheet; Self::COUNT] = [
        Sheet::Main,
        Sheet::Titlebar,
        Sheet::Cbuttons,
        Sheet::Shufrep,
        Sheet::Text,
        Sheet::Numbers,
        Sheet::NumsEx,
        Sheet::Volume,
        Sheet::Balance,
        Sheet::Posbar,
        Sheet::Playpaus,
        Sheet::Monoster,
        Sheet::Eqmain,
        Sheet::EqEx,
        Sheet::Pledit,
    ];

    /// Upper-case file name inside the archive.
    pub fn file_name(self) -> &'static str {
        match self {
            Sheet::Main => "MAIN.BMP",
            Sheet::Titlebar => "TITLEBAR.BMP",
            Sheet::Cbuttons => "CBUTTONS.BMP",
            Sheet::Shufrep => "SHUFREP.BMP",
            Sheet::Text => "TEXT.BMP",
            Sheet::Numbers => "NUMBERS.BMP",
            Sheet::NumsEx => "NUMS_EX.BMP",
            Sheet::Volume => "VOLUME.BMP",
            Sheet::Balance => "BALANCE.BMP",
            Sheet::Posbar => "POSBAR.BMP",
            Sheet::Playpaus => "PLAYPAUS.BMP",
            Sheet::Monoster => "MONOSTER.BMP",
            Sheet::Eqmain => "EQMAIN.BMP",
            Sheet::EqEx => "EQ_EX.BMP",
            Sheet::Pledit => "PLEDIT.BMP",
        }
    }

    fn from_file_name(upper: &str) -> Option<Sheet> {
        Self::ALL.into_iter().find(|s| s.file_name() == upper)
    }

    fn index(self) -> usize {
        self as usize
    }
}

/// Terminal colours derived from the skin, for the text-mode layouts
/// (Winamp text fallback and the skin browser).
#[derive(Debug, Clone, PartialEq)]
pub struct SkinColors {
    pub chrome_dark: Color,
    pub chrome_mid: Color,
    pub chrome_light: Color,
    pub body_bg: Color,
    pub titlebar_bg: Color,
    pub led_on: Color,
    pub led_off: Color,
    pub led_bg: Color,
    pub text_fg: Color,
    pub text_bg: Color,
    pub plist_normal: Color,
    pub plist_current: Color,
    pub plist_normal_bg: Color,
    pub plist_selected_bg: Color,
    /// VISCOLOR.TXT: 0 bg, 1 dots, 2..=17 analyzer (top→bottom), 18..=22 scope, 23 peaks.
    pub vis_colors: [Color; 24],
    pub btn_normal: Color,
    pub btn_pressed: Color,
    pub btn_text: Color,
    pub seek_track: Color,
    pub seek_thumb: Color,
    pub seek_filled: Color,
    pub play_indicator: Color,
    pub pause_indicator: Color,
    pub indicator_on: Color,
    pub indicator_off: Color,
}

pub const fn rgba_color(c: Rgba) -> Color {
    Color::Rgb(c[0], c[1], c[2])
}

const fn vis_default_colors() -> [Color; 24] {
    let mut out = [Color::Black; 24];
    let mut i = 0;
    while i < 24 {
        out[i] = rgba_color(text::DEFAULT_VISCOLOR[i]);
        i += 1;
    }
    out
}

impl SkinColors {
    /// Classic Winamp 2.x base colours.
    pub const DEFAULT: SkinColors = SkinColors {
        chrome_dark: Color::Rgb(8, 8, 16),
        chrome_mid: Color::Rgb(123, 140, 156),
        chrome_light: Color::Rgb(189, 206, 214),
        body_bg: Color::Rgb(57, 57, 90),
        titlebar_bg: Color::Rgb(0, 198, 255),
        led_on: Color::Rgb(0, 248, 0),
        led_off: Color::Rgb(24, 33, 41),
        led_bg: Color::Rgb(0, 20, 0),
        text_fg: Color::Rgb(0, 226, 0),
        text_bg: Color::Rgb(0, 0, 165),
        plist_normal: Color::Rgb(0, 255, 0),
        plist_current: Color::Rgb(255, 255, 255),
        plist_normal_bg: Color::Rgb(0, 0, 0),
        plist_selected_bg: Color::Rgb(0, 0, 198),
        vis_colors: vis_default_colors(),
        btn_normal: Color::Rgb(34, 33, 51),
        btn_pressed: Color::Rgb(48, 47, 76),
        btn_text: Color::Rgb(255, 255, 255),
        seek_track: Color::Rgb(16, 15, 24),
        seek_thumb: Color::Rgb(20, 19, 31),
        seek_filled: Color::Rgb(22, 21, 33),
        play_indicator: Color::Rgb(0, 232, 0),
        pause_indicator: Color::Rgb(255, 40, 51),
        indicator_on: Color::Rgb(0, 255, 0),
        indicator_off: Color::Rgb(52, 52, 82),
    };
}

impl Default for SkinColors {
    fn default() -> Self {
        Self::DEFAULT
    }
}

/// A loaded skin. Not `Clone`: sheets are decoded once and borrowed.
pub struct WinampSkin {
    pub name: String,
    pub author: Option<String>,
    pub description: Option<String>,
    sheets: [Option<Bitmap>; Sheet::COUNT],
    pub pledit: PleditColors,
    pub viscolor: [Rgba; 24],
    pub colors: SkinColors,
}

impl fmt::Debug for WinampSkin {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let loaded: Vec<&str> = Sheet::ALL
            .iter()
            .filter(|s| self.sheet(**s).is_some())
            .map(|s| s.file_name())
            .collect();
        f.debug_struct("WinampSkin")
            .field("name", &self.name)
            .field("author", &self.author)
            .field("description", &self.description)
            .field("sheets", &loaded)
            .finish_non_exhaustive()
    }
}

/// Largest archive member we are willing to inflate (zip-bomb guard).
const MAX_MEMBER_BYTES: u64 = 8 << 20;

/// Upper-case basename of an archive path, ignoring macOS resource forks.
fn member_basename(path: &str) -> Option<String> {
    if path.ends_with('/') || path.starts_with("__MACOSX") || path.contains("/__MACOSX") {
        return None;
    }
    let base = path.rsplit(['/', '\\']).next().unwrap_or(path);
    if base.is_empty() || base.starts_with("._") {
        return None;
    }
    Some(base.to_ascii_uppercase())
}

fn read_member<R: Read>(f: R, size: u64) -> Option<Vec<u8>> {
    if size > MAX_MEMBER_BYTES {
        return None;
    }
    let mut buf = Vec::with_capacity(size as usize);
    f.take(MAX_MEMBER_BYTES).read_to_end(&mut buf).ok()?;
    Some(buf)
}

fn is_text_asset(name: &str) -> bool {
    name == "PLEDIT.TXT" || name == "VISCOLOR.TXT" || text::METADATA_FILES.contains(&name)
}

impl WinampSkin {
    /// Load a skin from a `.wsz` file.
    pub fn from_wsz(path: &Path) -> anyhow::Result<Self> {
        // Skins are small; one read beats many seeks through a BufReader.
        let bytes = std::fs::read(path)?;
        Self::from_wsz_bytes(&bytes, &fallback_name(path))
    }

    /// Load a skin from in-memory `.wsz` bytes.
    pub fn from_wsz_bytes(bytes: &[u8], fallback_name: &str) -> anyhow::Result<Self> {
        Self::from_reader(std::io::Cursor::new(bytes), fallback_name)
    }

    fn from_reader<R: Read + Seek>(reader: R, fallback: &str) -> anyhow::Result<Self> {
        let mut archive = zip::ZipArchive::new(reader)?;
        let mut sheets: [Option<Bitmap>; Sheet::COUNT] = Default::default();
        let mut palettes: [Vec<Rgba>; Sheet::COUNT] = Default::default();
        let mut texts: Vec<(String, Vec<u8>)> = Vec::new();

        for i in 0..archive.len() {
            let Some(name) = archive.name_for_index(i).and_then(member_basename) else {
                continue;
            };
            let sheet = Sheet::from_file_name(&name);
            let wanted = match sheet {
                Some(s) => sheets[s.index()].is_none(),
                None => is_text_asset(&name) && !texts.iter().any(|(n, _)| *n == name),
            };
            if !wanted {
                continue;
            }
            let Ok(f) = archive.by_index(i) else { continue };
            let size = f.size();
            let Some(bytes) = read_member(f, size) else {
                continue;
            };
            match sheet {
                Some(s) => {
                    if let Ok(d) = bmp::decode(&bytes) {
                        sheets[s.index()] = Some(d.bitmap);
                        palettes[s.index()] = d.palette;
                    }
                }
                None => texts.push((name, bytes)),
            }
        }

        let text_of = |n: &str| {
            texts
                .iter()
                .find(|(k, _)| k == n)
                .map(|(_, v)| v.as_slice())
        };
        let (name, author, description) = text::parse_metadata(text_of, fallback);
        let pledit = text_of("PLEDIT.TXT")
            .map(text::parse_pledit)
            .unwrap_or_default();
        let viscolor = text_of("VISCOLOR.TXT")
            .map(text::parse_viscolor)
            .unwrap_or(text::DEFAULT_VISCOLOR);
        let colors = derive_colors(&sheets, &palettes, &pledit, &viscolor);

        Ok(Self {
            name,
            author,
            description,
            sheets,
            pledit,
            viscolor,
            colors,
        })
    }

    /// Sprite sheet, if the skin ships (and we could decode) it.
    pub fn sheet(&self, sheet: Sheet) -> Option<&Bitmap> {
        self.sheets[sheet.index()].as_ref()
    }

    /// True when the skin has the main window bitmap needed for pixel rendering.
    pub fn has_bitmaps(&self) -> bool {
        self.sheet(Sheet::Main).is_some()
    }

    /// Read name/author/description without decoding any bitmaps.
    pub fn peek_metadata(path: &Path) -> anyhow::Result<(String, Option<String>, Option<String>)> {
        let file = std::fs::File::open(path)?;
        let mut archive = zip::ZipArchive::new(std::io::BufReader::new(file))?;
        let mut texts: Vec<(String, Vec<u8>)> = Vec::new();
        for i in 0..archive.len() {
            let Some(name) = archive.name_for_index(i).and_then(member_basename) else {
                continue;
            };
            if !text::METADATA_FILES.contains(&name.as_str())
                || texts.iter().any(|(n, _)| *n == name)
            {
                continue;
            }
            let Ok(f) = archive.by_index(i) else { continue };
            let size = f.size();
            if let Some(bytes) = read_member(f, size) {
                texts.push((name, bytes));
            }
        }
        let get = |n: &str| {
            texts
                .iter()
                .find(|(k, _)| k == n)
                .map(|(_, v)| v.as_slice())
        };
        Ok(text::parse_metadata(get, &fallback_name(path)))
    }

    /// Built-in fallback: classic colours, no bitmaps (text layout only).
    pub fn default_skin() -> Self {
        Self {
            name: "Winamp Classic".into(),
            author: Some("Nullsoft".into()),
            description: Some("Built-in fallback Winamp 2.x classic colors".into()),
            sheets: Default::default(),
            pledit: PleditColors::default(),
            viscolor: text::DEFAULT_VISCOLOR,
            colors: SkinColors::DEFAULT,
        }
    }
}

fn fallback_name(path: &Path) -> String {
    path.file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("unknown")
        .to_string()
}

/// First pixel inside `region` (x, y, w, h) that differs from `bg`.
fn first_other(bmp: &Bitmap, bg: Rgba, region: (u32, u32, u32, u32)) -> Option<Rgba> {
    let (x0, y0, w, h) = region;
    (y0..y0 + h)
        .flat_map(|y| (x0..x0 + w).map(move |x| (x, y)))
        .filter_map(|(x, y)| bmp.get(x, y))
        .find(|p| *p != bg)
}

/// Terminal colours for text layouts: palette entries when the sheet is
/// palettised (classic skin convention), otherwise sampled pixels.
fn derive_colors(
    sheets: &[Option<Bitmap>; Sheet::COUNT],
    palettes: &[Vec<Rgba>; Sheet::COUNT],
    pledit: &PleditColors,
    viscolor: &[Rgba; 24],
) -> SkinColors {
    let d = SkinColors::DEFAULT;
    let pal = |s: Sheet, i: usize, fallback: Color| {
        palettes[s.index()]
            .get(i)
            .copied()
            .map(rgba_color)
            .unwrap_or(fallback)
    };
    let sheet = |s: Sheet| sheets[s.index()].as_ref();

    // TEXT.BMP: background from the space glyph, foreground from the letters.
    let (text_fg, text_bg) = match sheet(Sheet::Text) {
        Some(t) => {
            let bg = t.get(152, 2).unwrap_or([0, 0, 165, 255]);
            let fg = first_other(t, bg, (0, 0, 130, 6)).unwrap_or([0, 226, 0, 255]);
            (rgba_color(fg), rgba_color(bg))
        }
        None => (d.text_fg, d.text_bg),
    };
    // NUMBERS / NUMS_EX: unlit background and the lit segments of an "8".
    let (led_on, led_off) = match sheet(Sheet::NumsEx).or(sheet(Sheet::Numbers)) {
        Some(n) => {
            let off = n.get(0, 0).unwrap_or([24, 33, 41, 255]);
            let on = first_other(n, off, (72, 0, 9, 13)).unwrap_or([0, 248, 0, 255]);
            (rgba_color(on), rgba_color(off))
        }
        None => (d.led_on, d.led_off),
    };

    let mut vis_colors = d.vis_colors;
    for (dst, src) in vis_colors.iter_mut().zip(viscolor) {
        *dst = rgba_color(*src);
    }

    SkinColors {
        chrome_dark: pal(Sheet::Main, 5, d.chrome_dark),
        chrome_mid: pal(Sheet::Main, 3, d.chrome_mid),
        chrome_light: pal(Sheet::Main, 2, d.chrome_light),
        body_bg: pal(Sheet::Main, 4, d.body_bg),
        titlebar_bg: pal(Sheet::Main, 6, d.titlebar_bg),
        led_on,
        led_off,
        led_bg: d.led_bg,
        text_fg,
        text_bg,
        plist_normal: rgba_color(pledit.normal),
        plist_current: rgba_color(pledit.current),
        plist_normal_bg: rgba_color(pledit.normal_bg),
        plist_selected_bg: rgba_color(pledit.selected_bg),
        vis_colors,
        btn_normal: pal(Sheet::Cbuttons, 2, d.btn_normal),
        btn_pressed: pal(Sheet::Cbuttons, 4, d.btn_pressed),
        btn_text: pal(Sheet::Cbuttons, 1, d.btn_text),
        seek_track: pal(Sheet::Posbar, 0, d.seek_track),
        seek_thumb: pal(Sheet::Posbar, 1, d.seek_thumb),
        seek_filled: pal(Sheet::Posbar, 2, d.seek_filled),
        play_indicator: pal(Sheet::Playpaus, 0, d.play_indicator),
        pause_indicator: pal(Sheet::Playpaus, 1, d.pause_indicator),
        indicator_on: pal(Sheet::Monoster, 0, d.indicator_on),
        indicator_off: pal(Sheet::Monoster, 2, d.indicator_off),
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::bmp::tests::{build_bmp, BmpSpec};
    use super::*;
    use std::io::Write;

    /// A solid-colour 8-bit BMP of the given size.
    pub(crate) fn solid_bmp(w: i32, h: i32, rgb: [u8; 3]) -> Vec<u8> {
        let row = (w as usize).div_ceil(4) * 4;
        build_bmp(&BmpSpec {
            width: w,
            height: h,
            bpp: 8,
            compression: 0,
            header_size: 40,
            palette: std::slice::from_ref(&rgb),
            masks: None,
            pixel_data: vec![0; row * h as usize],
        })
    }

    /// Build an in-memory .wsz from (path, bytes) members.
    pub(crate) fn build_wsz(members: &[(&str, Vec<u8>)]) -> Vec<u8> {
        let mut w = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
        let opts = zip::write::SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Stored);
        for (name, data) in members {
            w.start_file(*name, opts).unwrap();
            w.write_all(data).unwrap();
        }
        w.finish().unwrap().into_inner()
    }

    #[test]
    fn loads_nested_mixed_case_members_and_skips_resource_forks() {
        let wsz = build_wsz(&[
            ("__MACOSX/MySkin/._main.bmp", b"junk".to_vec()),
            ("MySkin/Main.bmp", solid_bmp(275, 116, [10, 20, 30])),
            ("MySkin/sub\\numbers.BMP", solid_bmp(99, 13, [1, 2, 3])),
            ("MySkin/eqmain_iso.bmp", solid_bmp(4, 4, [9, 9, 9])),
            ("MySkin/plEdit.txt", b"[Text]\nNormal=#112233\n".to_vec()),
            ("MySkin/VisColor.TXT", b"5,6,7\n".to_vec()),
            (
                "MySkin/skin.ini",
                b"[Skin]\nName=Nested\nAuthor=Tester\n".to_vec(),
            ),
        ]);
        let skin = WinampSkin::from_wsz_bytes(&wsz, "fallback").unwrap();
        assert_eq!(skin.name, "Nested");
        assert_eq!(skin.author.as_deref(), Some("Tester"));
        let main = skin.sheet(Sheet::Main).unwrap();
        assert_eq!((main.width, main.height), (275, 116));
        assert_eq!(main.get(0, 0), Some([10, 20, 30, 255]));
        assert!(skin.sheet(Sheet::Numbers).is_some());
        assert!(skin.sheet(Sheet::Eqmain).is_none());
        assert!(skin.sheet(Sheet::Balance).is_none());
        assert_eq!(skin.pledit.normal, [0x11, 0x22, 0x33, 255]);
        assert_eq!(skin.viscolor[0], [5, 6, 7, 255]);
        assert_eq!(skin.colors.plist_normal, Color::Rgb(0x11, 0x22, 0x33));
        assert!(skin.has_bitmaps());
    }

    #[test]
    fn corrupt_sheet_is_skipped_not_fatal() {
        let wsz = build_wsz(&[
            ("MAIN.BMP", b"BMgarbage".to_vec()),
            ("TEXT.BMP", solid_bmp(155, 74, [0, 0, 0])),
        ]);
        let skin = WinampSkin::from_wsz_bytes(&wsz, "x").unwrap();
        assert!(skin.sheet(Sheet::Main).is_none());
        assert!(skin.sheet(Sheet::Text).is_some());
        assert!(!skin.has_bitmaps());
        assert_eq!(skin.name, "x");
    }

    #[test]
    fn peek_metadata_strips_directory_prefixes() {
        let dir = std::env::temp_dir().join(format!("rustune-peek-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("Some Skin.wsz");
        let wsz = build_wsz(&[("Some Skin/genex.ini", b"[x]\nname=Peeked\n".to_vec())]);
        std::fs::write(&path, wsz).unwrap();
        let (name, _, _) = WinampSkin::peek_metadata(&path).unwrap();
        std::fs::remove_dir_all(&dir).ok();
        assert_eq!(name, "Peeked");
    }

    #[test]
    fn not_a_zip_is_an_error() {
        assert!(WinampSkin::from_wsz_bytes(b"nope", "x").is_err());
    }

    #[test]
    fn member_names() {
        assert_eq!(member_basename("a/b/Main.Bmp").as_deref(), Some("MAIN.BMP"));
        assert_eq!(
            member_basename("a\\pledit.txt").as_deref(),
            Some("PLEDIT.TXT")
        );
        assert_eq!(member_basename("dir/"), None);
        assert_eq!(member_basename("__MACOSX/x/MAIN.BMP"), None);
        assert_eq!(member_basename("x/._MAIN.BMP"), None);
    }
}
