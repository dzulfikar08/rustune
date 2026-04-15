/// WSZ (Winamp Skin Zip) loader.
///
/// Parses any .wsz file to extract colors from BMP palettes and TXT config files,
/// then produces a `WinampSkin` struct that the Winamp renderer uses to paint
/// the TUI in the exact style of the loaded skin.
///
/// Key files in a WSZ:
///   MAIN.BMP      — main window chrome (275x116), palette has all chrome colors
///   NUMBERS.BMP   — LED digit bitmaps (99x13), palette[0]=dim, palette[1]=LED
///   TEXT.BMP      — marquee font (155x74), palette[0]=bg, palette[1]=fg
///   PLEDIT.TXT    — playlist text/bg colors
///   VISCOLOR.TXT  — 24 spectrum analyzer colors
///   POSBAR.BMP    — seek bar (307x10)
///   CBUTTONS.BMP  — transport buttons (136x36)
///   TITLEBAR.BMP  — title bar (344x87)
///   SHUFREP.BMP   — shuffle/repeat buttons
///   PLAYPAUS.BMP  — play/pause indicator (42x9)
///   MONOSTER.BMP  — mono/stereo indicator (58x24)
///   VOLUME.BMP    — volume slider (68x433)

use std::collections::HashMap;
use std::io::Read;

use ratatui::style::Color;

#[derive(Debug, Clone)]
pub struct BmpImage {
    pub width: u32,
    pub height: u32,
    pub palette: Vec<Color>,
    /// Pixel indices into `palette`, in row-major, top-down order.
    /// For 24bpp images without a real palette, each byte stores a palette index.
    pub pixels: Vec<u8>,
    /// For 24bpp images: raw BGR triplets per pixel (row-major, top-down).
    /// If present, `color_at` uses this instead of palette lookup.
    pub raw_rgb: Option<Vec<u8>>,
}

impl BmpImage {
    pub fn color_at(&self, x: u32, y: u32) -> Color {
        let x = x.min(self.width.saturating_sub(1));
        let y = y.min(self.height.saturating_sub(1));
        let idx = (y as usize)
            .saturating_mul(self.width as usize)
            .saturating_add(x as usize);

        if let Some(ref rgb) = self.raw_rgb {
            let off = idx * 3;
            if off + 2 < rgb.len() {
                return Color::Rgb(rgb[off + 2], rgb[off + 1], rgb[off]); // BGR -> RGB
            }
            return Color::Black;
        }

        let p = self.pixels.get(idx).copied().unwrap_or(0) as usize;
        self.palette.get(p).copied().unwrap_or(Color::Black)
    }
}

/// Parsed Winamp skin data, ready for the TUI renderer.
#[derive(Debug, Clone)]
#[allow(dead_code)]
pub struct WinampSkin {
    pub name: String,
    pub author: Option<String>,
    pub description: Option<String>,
    pub main_bitmap: Option<BmpImage>,

    // Additional BMP images for bitmap-mode rendering
    pub numbers_bitmap: Option<BmpImage>,
    pub cbuttons_bitmap: Option<BmpImage>,
    pub posbar_bitmap: Option<BmpImage>,
    pub text_bitmap: Option<BmpImage>,
    pub playpaus_bitmap: Option<BmpImage>,
    pub titlebar_bitmap: Option<BmpImage>,
    pub monoster_bitmap: Option<BmpImage>,
    pub shufrep_bitmap: Option<BmpImage>,
    pub volume_bitmap: Option<BmpImage>,

    // Colors extracted from MAIN.BMP palette (indices are well-known)
    pub chrome_dark: Color,       // palette[5] — darkest chrome
    pub chrome_mid: Color,        // palette[3] — mid chrome
    pub chrome_light: Color,      // palette[2] — light chrome border
    pub body_bg: Color,           // palette[4] — main body background
    pub titlebar_bg: Color,       // palette[6] — title bar accent

    // LED display (from NUMBERS.BMP palette)
    pub led_on: Color,            // palette[1] — bright LED
    pub led_off: Color,           // palette[0] — dim LED background

    // Text colors (from TEXT.BMP palette)
    pub text_fg: Color,           // palette[1] — text foreground
    pub text_bg: Color,           // palette[0] — text background

    // Playlist colors (from PLEDIT.TXT)
    pub plist_normal: Color,      // Normal text
    pub plist_current: Color,     // Current/selected text
    pub plist_normal_bg: Color,   // Normal background
    pub plist_selected_bg: Color, // Selected background

    // Visualization (24 colors from VISCOLOR.TXT)
    pub vis_colors: Vec<Color>,

    // Transport button colors (from CBUTTONS.BMP palette)
    pub btn_normal: Color,        // button face normal
    pub btn_pressed: Color,       // button face pressed
    pub btn_text: Color,          // button icon color

    // Seek bar (from POSBAR.BMP palette)
    pub seek_track: Color,        // seek bar track
    pub seek_thumb: Color,        // seek bar thumb
    pub seek_filled: Color,       // filled portion

    // Play/pause indicator (from PLAYPAUS.BMP)
    pub play_indicator: Color,
    pub pause_indicator: Color,

    // Mono/Stereo (from MONOSTER.BMP)
    pub indicator_on: Color,
    pub indicator_off: Color,
}

impl WinampSkin {
    /// Load a skin from a .wsz file path.
    pub fn from_wsz(path: &std::path::Path) -> anyhow::Result<Self> {
        let file = std::fs::File::open(path)?;
        let mut archive = zip::ZipArchive::new(file)?;

        let fallback_name = path
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("unknown")
            .to_string();

        // Collect all file contents into a HashMap.
        // Strip directory prefixes so that e.g. "skin_dir/MAIN.BMP" → "MAIN.BMP"
        let mut files: HashMap<String, Vec<u8>> = HashMap::new();
        for i in 0..archive.len() {
            let mut f = archive.by_index(i)?;
            let raw_name = f.name().to_uppercase();
            // Take only the filename portion (after last '/')
            let fname = raw_name.rsplit('/').next().unwrap_or(&raw_name).to_string();
            let mut buf = Vec::new();
            f.read_to_end(&mut buf)?;
            files.insert(fname, buf);
        }

        let (name, author, description) = parse_wsz_metadata(&files, &fallback_name);

        // Parse MAIN.BMP palette
        let main_pal = parse_bmp_palette(files.get("MAIN.BMP"));
        let chrome_dark = main_pal.get(5).copied().unwrap_or(Color::Rgb(8, 8, 16));
        let chrome_mid = main_pal.get(3).copied().unwrap_or(Color::Rgb(123, 140, 156));
        let chrome_light = main_pal.get(2).copied().unwrap_or(Color::Rgb(189, 206, 214));
        let body_bg = main_pal.get(4).copied().unwrap_or(Color::Rgb(57, 57, 90));
        let titlebar_bg = main_pal.get(6).copied().unwrap_or(Color::Rgb(0, 198, 255));
        let main_bitmap = parse_bmp_8bit(files.get("MAIN.BMP"));

        // Parse additional BMP images for bitmap-mode rendering
        let numbers_bitmap = parse_bmp_8bit(files.get("NUMBERS.BMP"));
        let cbuttons_bitmap = parse_bmp_8bit(files.get("CBUTTONS.BMP"));
        let posbar_bitmap = parse_bmp_8bit(files.get("POSBAR.BMP"));
        let text_bitmap = parse_bmp_8bit(files.get("TEXT.BMP"));
        let playpaus_bitmap = parse_bmp_8bit(files.get("PLAYPAUS.BMP"));
        let titlebar_bitmap = parse_bmp_8bit(files.get("TITLEBAR.BMP"));
        let monoster_bitmap = parse_bmp_8bit(files.get("MONOSTER.BMP"));
        let shufrep_bitmap = parse_bmp_8bit(files.get("SHUFREP.BMP"));
        let volume_bitmap = parse_bmp_8bit(files.get("VOLUME.BMP"));

        // Parse NUMBERS.BMP for LED colors
        let num_pal = parse_bmp_palette(files.get("NUMBERS.BMP"));
        let led_on = num_pal.get(1).copied().unwrap_or(Color::Rgb(0, 248, 0));
        let led_off = num_pal.get(0).copied().unwrap_or(Color::Rgb(24, 33, 41));

        // Parse TEXT.BMP for marquee text colors
        let text_pal = parse_bmp_palette(files.get("TEXT.BMP"));
        let text_fg = text_pal.get(1).copied().unwrap_or(Color::Rgb(0, 226, 0));
        let text_bg = text_pal.get(0).copied().unwrap_or(Color::Rgb(0, 0, 165));

        // Parse PLEDIT.TXT for playlist colors
        let (plist_normal, plist_current, plist_normal_bg, plist_selected_bg) =
            parse_pledit_txt(files.get("PLEDIT.TXT"));

        // Parse VISCOLOR.TXT for spectrum analyzer
        let vis_colors = parse_viscolor_txt(files.get("VISCOLOR.TXT"));

        // Parse CBUTTONS.BMP for transport button colors
        let cbtn_pal = parse_bmp_palette(files.get("CBUTTONS.BMP"));
        let btn_normal = cbtn_pal.get(2).copied().unwrap_or(Color::Rgb(34, 33, 51));
        let btn_pressed = cbtn_pal.get(4).copied().unwrap_or(Color::Rgb(48, 47, 76));
        let btn_text = cbtn_pal.get(1).copied().unwrap_or(Color::White);

        // Parse POSBAR.BMP for seek bar colors
        let pos_pal = parse_bmp_palette(files.get("POSBAR.BMP"));
        let seek_track = pos_pal.first().copied().unwrap_or(Color::Rgb(16, 15, 24));
        let seek_thumb = pos_pal.get(1).copied().unwrap_or(Color::Rgb(20, 19, 31));
        let seek_filled = pos_pal.get(2).copied().unwrap_or(Color::Rgb(22, 21, 33));

        // Parse PLAYPAUS.BMP
        let pp_pal = parse_bmp_palette(files.get("PLAYPAUS.BMP"));
        let play_indicator = pp_pal.first().copied().unwrap_or(Color::Rgb(0, 232, 0));
        let pause_indicator = pp_pal.get(1).copied().unwrap_or(Color::Rgb(255, 40, 51));

        // Parse MONOSTER.BMP
        let ms_pal = parse_bmp_palette(files.get("MONOSTER.BMP"));
        let indicator_on = ms_pal.first().copied().unwrap_or(Color::Rgb(0, 255, 0));
        let indicator_off = ms_pal.get(2).copied().unwrap_or(Color::Rgb(52, 52, 82));

        Ok(Self {
            name,
            author,
            description,
            main_bitmap,
            numbers_bitmap,
            cbuttons_bitmap,
            posbar_bitmap,
            text_bitmap,
            playpaus_bitmap,
            titlebar_bitmap,
            monoster_bitmap,
            shufrep_bitmap,
            volume_bitmap,
            chrome_dark,
            chrome_mid,
            chrome_light,
            body_bg,
            titlebar_bg,
            led_on,
            led_off,
            text_fg,
            text_bg,
            plist_normal,
            plist_current,
            plist_normal_bg,
            plist_selected_bg,
            vis_colors,
            btn_normal,
            btn_pressed,
            btn_text,
            seek_track,
            seek_thumb,
            seek_filled,
            play_indicator,
            pause_indicator,
            indicator_on,
            indicator_off,
        })
    }

    /// Read metadata (name/author/description) from a .wsz without parsing BMP palettes.
    pub fn peek_metadata(path: &std::path::Path) -> anyhow::Result<(String, Option<String>, Option<String>)> {
        let file = std::fs::File::open(path)?;
        let mut archive = zip::ZipArchive::new(file)?;

        let fallback_name = path
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("unknown")
            .to_string();

        let wanted = [
            "GENEX.INI",
            "SKIN.INI",
            "SKIN.XML",
            "README.TXT",
            "INFO.TXT",
        ];

        let mut files: HashMap<String, Vec<u8>> = HashMap::new();
        for i in 0..archive.len() {
            let mut f = archive.by_index(i)?;
            let fname = f.name().to_uppercase();
            if !wanted.contains(&fname.as_str()) {
                continue;
            }
            let mut buf = Vec::new();
            f.read_to_end(&mut buf)?;
            files.insert(fname, buf);
        }

        Ok(parse_wsz_metadata(&files, &fallback_name))
    }

    /// Built-in default skin (the classic Winamp 2.x "base" skin colors).
    pub fn default_skin() -> Self {
        Self {
            name: "Winamp Classic".into(),
            author: Some("Nullsoft".into()),
            description: Some("Built-in fallback Winamp 2.x classic colors".into()),
            main_bitmap: None,
            numbers_bitmap: None,
            cbuttons_bitmap: None,
            posbar_bitmap: None,
            text_bitmap: None,
            playpaus_bitmap: None,
            titlebar_bitmap: None,
            monoster_bitmap: None,
            shufrep_bitmap: None,
            volume_bitmap: None,
            chrome_dark: Color::Rgb(8, 8, 16),
            chrome_mid: Color::Rgb(123, 140, 156),
            chrome_light: Color::Rgb(189, 206, 214),
            body_bg: Color::Rgb(57, 57, 90),
            titlebar_bg: Color::Rgb(0, 198, 255),
            led_on: Color::Rgb(0, 248, 0),
            led_off: Color::Rgb(24, 33, 41),
            text_fg: Color::Rgb(0, 226, 0),
            text_bg: Color::Rgb(0, 0, 165),
            plist_normal: Color::Rgb(0, 255, 0),
            plist_current: Color::White,
            plist_normal_bg: Color::Black,
            plist_selected_bg: Color::Rgb(0, 0, 198),
            vis_colors: vec![
                Color::Rgb(0, 0, 0),
                Color::Rgb(24, 33, 41),
                Color::Rgb(239, 49, 16),
                Color::Rgb(206, 41, 16),
                Color::Rgb(214, 90, 0),
                Color::Rgb(214, 102, 0),
                Color::Rgb(214, 115, 0),
                Color::Rgb(198, 123, 8),
                Color::Rgb(222, 165, 24),
                Color::Rgb(214, 181, 33),
                Color::Rgb(189, 222, 41),
                Color::Rgb(148, 222, 33),
                Color::Rgb(41, 206, 16),
                Color::Rgb(50, 190, 16),
                Color::Rgb(57, 181, 16),
                Color::Rgb(49, 156, 8),
                Color::Rgb(41, 148, 0),
                Color::Rgb(24, 132, 8),
            ],
            btn_normal: Color::Rgb(34, 33, 51),
            btn_pressed: Color::Rgb(48, 47, 76),
            btn_text: Color::White,
            seek_track: Color::Rgb(16, 15, 24),
            seek_thumb: Color::Rgb(20, 19, 31),
            seek_filled: Color::Rgb(22, 21, 33),
            play_indicator: Color::Rgb(0, 232, 0),
            pause_indicator: Color::Rgb(255, 40, 51),
            indicator_on: Color::Rgb(0, 255, 0),
            indicator_off: Color::Rgb(52, 52, 82),
        }
    }

    /// List all .wsz files in the skins directory.
    pub fn available_skins() -> Vec<std::path::PathBuf> {
        let skin_dir = dirs::config_dir()
            .unwrap_or_else(|| std::path::PathBuf::from("~/.config"))
            .join("rustune")
            .join("skins");

        if !skin_dir.exists() {
            return Vec::new();
        }

        let mut skins: Vec<std::path::PathBuf> = std::fs::read_dir(&skin_dir)
            .unwrap_or_else(|_| panic!("Cannot read skins dir"))
            .filter_map(|e| e.ok())
            .map(|e| e.path())
            .filter(|p| {
                p.extension()
                    .and_then(|e| e.to_str())
                    .map(|e| e.eq_ignore_ascii_case("wsz"))
                    .unwrap_or(false)
            })
            .collect();
        skins.sort();
        skins
    }
}

fn parse_wsz_metadata(
    files: &HashMap<String, Vec<u8>>,
    fallback_name: &str,
) -> (String, Option<String>, Option<String>) {
    // Prefer explicit metadata files if present; otherwise fall back to filename stem.
    let candidates = [
        "GENEX.INI",
        "SKIN.INI",
        "SKIN.XML",
        "README.TXT",
        "INFO.TXT",
    ];

    for key in candidates {
        if let Some(bytes) = files.get(key) {
            let s = String::from_utf8_lossy(bytes);
            let (n, a, d) = match key {
                "SKIN.XML" => parse_xmlish_metadata(&s),
                _ => parse_iniish_metadata(&s),
            };
            let name = n
                .as_deref()
                .map(str::trim)
                .filter(|v| !v.is_empty())
                .unwrap_or(fallback_name)
                .to_string();
            let author = a.and_then(|v| {
                let t = v.trim().to_string();
                (!t.is_empty()).then_some(t)
            });
            let description = d.and_then(|v| {
                let t = v.trim().to_string();
                (!t.is_empty()).then_some(t)
            });
            return (name, author, description);
        }
    }

    (fallback_name.to_string(), None, None)
}

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
            continue; // section header
        }
        let Some((k, v)) = line.split_once('=') else { continue };
        let key = k.trim().to_lowercase();
        let mut val = v.trim().trim_matches('"').trim_matches('\'').trim().to_string();
        if val.is_empty() {
            continue;
        }

        // Heuristics: common keys seen in Winamp skin metadata.
        match key.as_str() {
            "name" | "skinname" | "title" | "skin_title" => {
                if name.is_none() {
                    name = Some(std::mem::take(&mut val));
                }
            }
            "author" | "skin_author" => {
                if author.is_none() {
                    author = Some(std::mem::take(&mut val));
                }
            }
            "comment" | "description" | "skin_description" => {
                if desc.is_none() {
                    desc = Some(std::mem::take(&mut val));
                }
            }
            _ => {}
        }
    }

    // README/INFO files often just contain prose; take the first non-empty line as name if unset.
    if name.is_none() {
        if let Some(first) = s.lines().map(str::trim).find(|l| !l.is_empty()) {
            // Avoid treating obvious "readme" boilerplate as name.
            let lower = first.to_lowercase();
            if !lower.starts_with("readme")
                && !lower.starts_with("skin")
                && first.len() <= 80
            {
                name = Some(first.to_string());
            }
        }
    }

    (name, author, desc)
}

fn parse_xmlish_metadata(s: &str) -> (Option<String>, Option<String>, Option<String>) {
    // Minimal tag extraction without adding an XML dependency.
    fn between(hay: &str, open: &str, close: &str) -> Option<String> {
        let start = hay.find(open)? + open.len();
        let rest = &hay[start..];
        let end = rest.find(close)?;
        Some(rest[..end].trim().to_string())
    }

    let lower = s.to_lowercase();
    let mut name = between(&lower, "<name>", "</name>");
    let mut author = between(&lower, "<author>", "</author>");
    let mut desc = between(&lower, "<description>", "</description>");

    // If we pulled from lowercased text, re-extract from original by searching case-insensitively.
    // (good enough for typical ASCII metadata)
    if name.is_some() || author.is_some() || desc.is_some() {
        if name.is_some() {
            name = between(s, "<name>", "</name>")
                .or_else(|| between(s, "<Name>", "</Name>"));
        }
        if author.is_some() {
            author = between(s, "<author>", "</author>")
                .or_else(|| between(s, "<Author>", "</Author>"));
        }
        if desc.is_some() {
            desc = between(s, "<description>", "</description>")
                .or_else(|| between(s, "<Description>", "</Description>"));
        }
    }

    (name, author, desc)
}

fn parse_bmp_8bit(data: Option<&Vec<u8>>) -> Option<BmpImage> {
    let data = data?;
    if data.len() < 54 {
        return None;
    }
    if &data[0..2] != b"BM" {
        return None;
    }

    let pixel_offset = u32::from_le_bytes([data[10], data[11], data[12], data[13]]) as usize;
    let dib_size = u32::from_le_bytes([data[14], data[15], data[16], data[17]]) as usize;
    if data.len() < 14 + dib_size {
        return None;
    }

    let width = i32::from_le_bytes([data[18], data[19], data[20], data[21]]);
    let height = i32::from_le_bytes([data[22], data[23], data[24], data[25]]);
    let planes = u16::from_le_bytes([data[26], data[27]]);
    let bpp = u16::from_le_bytes([data[28], data[29]]);
    let compression = u32::from_le_bytes([data[30], data[31], data[32], data[33]]);
    let colors_used = u32::from_le_bytes([data[46], data[47], data[48], data[49]]);

    if planes != 1 {
        return None;
    }
    if bpp != 8 && bpp != 24 && bpp != 32 {
        return None;
    }
    if compression > 1 {
        return None;
    }
    if width <= 0 || height == 0 {
        return None;
    }

    let w = width as u32;
    let top_down = height < 0;
    let h = height.unsigned_abs();

    // 24bpp/32bpp path: no palette, store raw BGR triplets
    if bpp == 24 || bpp == 32 {
        let bytes_per_pixel = (bpp / 8) as usize; // 3 for 24bpp, 4 for 32bpp
        let row_bytes = (((w as usize) * bytes_per_pixel + 3) / 4) * 4;
        let needed = row_bytes.saturating_mul(h as usize);
        if data.len() < pixel_offset + needed {
            return None;
        }

        // Always store as BGR triplets (3 bytes/pixel), skip alpha for 32bpp
        let mut raw_rgb = vec![0u8; (w as usize * 3).saturating_mul(h as usize)];
        let pix = &data[pixel_offset..pixel_offset + needed];
        for y in 0..h as usize {
            let src_y = if top_down { y } else { (h as usize - 1).saturating_sub(y) };
            let src_off = src_y * row_bytes;
            let dst_off = y * w as usize * 3;
            for x in 0..w as usize {
                let si = src_off + x * bytes_per_pixel;
                let di = dst_off + x * 3;
                raw_rgb[di] = pix[si];         // B
                raw_rgb[di + 1] = pix[si + 1]; // G
                raw_rgb[di + 2] = pix[si + 2]; // R
                // pix[si + 3] for 32bpp is alpha — ignored
            }
        }

        return Some(BmpImage {
            width: w,
            height: h,
            palette: vec![Color::Black],
            pixels: vec![0u8; (w as usize).saturating_mul(h as usize)],
            raw_rgb: Some(raw_rgb),
        });
    }

    // 8bpp path (original)
    let palette_start = 14 + dib_size;
    if pixel_offset <= palette_start || data.len() < pixel_offset {
        return None;
    }

    let palette_bytes = &data[palette_start..pixel_offset];
    let mut num_colors = (palette_bytes.len() / 4) as u32;
    if colors_used != 0 {
        num_colors = num_colors.min(colors_used);
    }
    if num_colors == 0 {
        return None;
    }

    let mut palette = Vec::with_capacity(num_colors as usize);
    for i in 0..num_colors as usize {
        let b = palette_bytes[i * 4];
        let g = palette_bytes[i * 4 + 1];
        let r = palette_bytes[i * 4 + 2];
        palette.push(Color::Rgb(r, g, b));
    }

    let mut pixels = vec![0u8; (w as usize).saturating_mul(h as usize)];

    if compression == 0 {
        // Uncompressed: rows are padded to 4 bytes
        let row_bytes = ((w as usize + 3) / 4) * 4;
        let needed = row_bytes.saturating_mul(h as usize);
        if data.len() < pixel_offset + needed {
            return None;
        }
        let pix = &data[pixel_offset..pixel_offset + needed];
        for y in 0..h as usize {
            let src_y = if top_down { y } else { (h as usize - 1).saturating_sub(y) };
            let src_row = &pix[src_y * row_bytes..src_y * row_bytes + w as usize];
            let dst_row = &mut pixels[y * w as usize..y * w as usize + w as usize];
            dst_row.copy_from_slice(src_row);
        }
    } else {
        // RLE8 decompression (compression == 1)
        // RLE8 is always bottom-up
        if data.len() <= pixel_offset {
            return None;
        }
        let rle_data = &data[pixel_offset..];
        let mut pos = 0usize;
        let mut x = 0usize;
        let mut y = 0usize;

        loop {
            if pos + 1 >= rle_data.len() {
                break;
            }
            let count = rle_data[pos] as usize;
            let val = rle_data[pos + 1];
            pos += 2;

            if count > 0 {
                // Encoded run: repeat `val` count times
                for _ in 0..count {
                    if x < w as usize && y < h as usize {
                        pixels[y * w as usize + x] = val;
                    }
                    x += 1;
                }
            } else {
                // Escape sequence
                match val {
                    0 => {
                        // End of line
                        x = 0;
                        y += 1;
                    }
                    1 => {
                        // End of bitmap
                        break;
                    }
                    2 => {
                        // Delta: move position
                        if pos + 1 >= rle_data.len() {
                            break;
                        }
                        x += rle_data[pos] as usize;
                        y += rle_data[pos + 1] as usize;
                        pos += 2;
                    }
                    _ => {
                        // Absolute mode: `val` literal pixels follow
                        let n = val as usize;
                        if pos + n > rle_data.len() {
                            break;
                        }
                        for i in 0..n {
                            if x < w as usize && y < h as usize {
                                pixels[y * w as usize + x] = rle_data[pos + i];
                            }
                            x += 1;
                        }
                        pos += n;
                        // Absolute runs are padded to word boundary
                        if n % 2 != 0 {
                            pos += 1;
                        }
                    }
                }
            }
        }
    }

    Some(BmpImage {
        width: w,
        height: h,
        palette,
        pixels,
        raw_rgb: None,
    })
}

/// Parse the palette (color table) from 8-bit BMP data.
/// Returns a Vec of ratatui Colors from the palette entries.
fn parse_bmp_palette(data: Option<&Vec<u8>>) -> Vec<Color> {
    let data = match data {
        Some(d) => d,
        None => return Vec::new(),
    };

    if data.len() < 54 {
        return Vec::new();
    }

    // BMP header: pixel offset at bytes 10-13
    let pixel_offset = u32::from_le_bytes([data[10], data[11], data[12], data[13]]) as usize;
    let header_size = u32::from_le_bytes([data[14], data[15], data[16], data[17]]) as usize;

    let palette_start = 14 + header_size;
    if pixel_offset <= palette_start || data.len() < pixel_offset {
        return Vec::new();
    }

    let palette_bytes = &data[palette_start..pixel_offset];
    let num_colors = palette_bytes.len() / 4;

    let mut colors = Vec::with_capacity(num_colors);
    for i in 0..num_colors {
        let b = palette_bytes[i * 4];
        let g = palette_bytes[i * 4 + 1];
        let r = palette_bytes[i * 4 + 2];
        // Skip _a (padding byte)
        colors.push(Color::Rgb(r, g, b));
    }

    colors
}

/// Parse PLEDIT.TXT for playlist colors.
/// Format:
///   [Text]
///   Normal=#RRGGBB
///   Current=#RRGGBB
///   NormalBG=#RRGGBB
///   SelectedBG=#RRGGBB
fn parse_pledit_txt(data: Option<&Vec<u8>>) -> (Color, Color, Color, Color) {
    let defaults = (
        Color::Rgb(0, 255, 0),
        Color::White,
        Color::Black,
        Color::Rgb(0, 0, 198),
    );

    let data = match data {
        Some(d) => d,
        None => return defaults,
    };

    let text = String::from_utf8_lossy(data);
    let mut normal = None;
    let mut current = None;
    let mut normal_bg = None;
    let mut selected_bg = None;

    for line in text.lines() {
        let line = line.trim();
        if let Some(val) = line.strip_prefix("Normal=") {
            normal = parse_hex_color(val);
        } else if let Some(val) = line.strip_prefix("Current=") {
            current = parse_hex_color(val);
        } else if let Some(val) = line.strip_prefix("NormalBG=") {
            normal_bg = parse_hex_color(val);
        } else if let Some(val) = line.strip_prefix("SelectedBG=") {
            selected_bg = parse_hex_color(val);
        }
    }

    (
        normal.unwrap_or(defaults.0),
        current.unwrap_or(defaults.1),
        normal_bg.unwrap_or(defaults.2),
        selected_bg.unwrap_or(defaults.3),
    )
}

/// Parse a #RRGGBB hex color string.
fn parse_hex_color(s: &str) -> Option<Color> {
    let s = s.trim().trim_start_matches('#');
    if s.len() != 6 {
        return None;
    }
    let r = u8::from_str_radix(&s[0..2], 16).ok()?;
    let g = u8::from_str_radix(&s[2..4], 16).ok()?;
    let b = u8::from_str_radix(&s[4..6], 16).ok()?;
    Some(Color::Rgb(r, g, b))
}

/// Parse VISCOLOR.TXT — 24 lines of "r,g,b, // comment" for spectrum analyzer.
fn parse_viscolor_txt(data: Option<&Vec<u8>>) -> Vec<Color> {
    let data = match data {
        Some(d) => d,
        None => return Vec::new(),
    };

    let text = String::from_utf8_lossy(data);
    let mut colors = Vec::new();

    for line in text.lines() {
        let line = line.split("//").next().unwrap_or("").trim();
        if line.is_empty() {
            continue;
        }
        let parts: Vec<&str> = line.split(',').map(|s| s.trim()).collect();
        if parts.len() >= 3 {
            if let (Ok(r), Ok(g), Ok(b)) = (
                parts[0].parse::<u8>(),
                parts[1].parse::<u8>(),
                parts[2].parse::<u8>(),
            ) {
                colors.push(Color::Rgb(r, g, b));
            }
        }
    }

    colors
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Read;

    #[test]
    fn test_parse_all_skins() {
        let skin_dir = dirs::config_dir()
            .unwrap_or_else(|| std::path::PathBuf::from("~/.config"))
            .join("rustune")
            .join("skins");

        if !skin_dir.exists() {
            eprintln!("SKIP: skin dir not found");
            return;
        }

        let entries = std::fs::read_dir(&skin_dir).expect("read dir");
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()).map(|e| e.eq_ignore_ascii_case("wsz")) != Some(true) {
                continue;
            }
            let name = path.file_name().unwrap_or_default().to_string_lossy().to_string();
            match WinampSkin::from_wsz(&path) {
                Ok(skin) => {
                    let has_main = skin.main_bitmap.is_some();
                    let bmp_info = skin.main_bitmap.as_ref()
                        .map(|b| format!("{}x{}", b.width, b.height))
                        .unwrap_or_else(|| "None".into());
                    eprintln!("OK {}: main_bitmap={} {}", name, has_main, bmp_info);
                    assert!(has_main, "Skin {} should have main_bitmap", name);
                }
                Err(e) => {
                    eprintln!("FAIL {}: {}", name, e);
                    // Don't fail the test for broken skins, just log
                }
            }
        }
    }

    #[test]
    fn test_parse_main_bmp_from_wsz() {
        let skin_path = dirs::config_dir()
            .unwrap_or_else(|| std::path::PathBuf::from("~/.config"))
            .join("rustune")
            .join("skins")
            .join("base-2.91.wsz");

        if !skin_path.exists() {
            eprintln!("SKIP: skin not found at {:?}", skin_path);
            return;
        }

        let file = std::fs::File::open(&skin_path).expect("open wsz");
        let mut archive = zip::ZipArchive::new(file).expect("zip archive");

        let mut files: HashMap<String, Vec<u8>> = HashMap::new();
        for i in 0..archive.len() {
            let mut f = archive.by_index(i).expect("by_index");
            let fname = f.name().to_uppercase();
            let mut buf = Vec::new();
            f.read_to_end(&mut buf).expect("read");
            files.insert(fname, buf);
        }

        // Check MAIN.BMP exists
        let main_data = files.get("MAIN.BMP").expect("MAIN.BMP must be in WSZ");
        eprintln!("MAIN.BMP data length: {}", main_data.len());

        // Parse header manually
        let magic = &main_data[0..2];
        eprintln!("Magic: {:?}", std::str::from_utf8(magic));
        let bpp = u16::from_le_bytes([main_data[28], main_data[29]]);
        let comp = u32::from_le_bytes([main_data[30], main_data[31], main_data[32], main_data[33]]);
        let w = i32::from_le_bytes([main_data[18], main_data[19], main_data[20], main_data[21]]);
        let h = i32::from_le_bytes([main_data[22], main_data[23], main_data[24], main_data[25]]);
        let planes = u16::from_le_bytes([main_data[26], main_data[27]]);
        eprintln!("MAIN.BMP: {}x{} planes={} bpp={} comp={}", w, h, planes, bpp, comp);

        // Try parsing
        let result = parse_bmp_8bit(files.get("MAIN.BMP"));
        match &result {
            Some(bmp) => {
                eprintln!("SUCCESS: {}x{} palette={} pixels={}", bmp.width, bmp.height, bmp.palette.len(), bmp.pixels.len());
            }
            None => {
                eprintln!("FAILED: parse_bmp_8bit returned None");
            }
        }
        assert!(result.is_some(), "MAIN.BMP should parse successfully");

        // Also check all other BMPs
        for name in &["NUMBERS.BMP", "CBUTTONS.BMP", "POSBAR.BMP", "TEXT.BMP", "PLAYPAUS.BMP", "TITLEBAR.BMP", "VOLUME.BMP", "MONOSTER.BMP", "SHUFREP.BMP"] {
            if let Some(data) = files.get(*name) {
                let bpp = u16::from_le_bytes([data[28], data[29]]);
                let comp = u32::from_le_bytes([data[30], data[31], data[32], data[33]]);
                let result = parse_bmp_8bit(Some(data));
                eprintln!("{}: len={} bpp={} comp={} => {}", name, data.len(), bpp, comp, result.is_some());
            }
        }
    }
}
