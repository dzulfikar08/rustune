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

/// Parsed Winamp skin data, ready for the TUI renderer.
#[derive(Debug, Clone)]
#[allow(dead_code)]
pub struct WinampSkin {
    pub name: String,

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

        let name = path
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("unknown")
            .to_string();

        // Collect all file contents into a HashMap
        let mut files: HashMap<String, Vec<u8>> = HashMap::new();
        for i in 0..archive.len() {
            let mut f = archive.by_index(i)?;
            let fname = f.name().to_uppercase();
            let mut buf = Vec::new();
            f.read_to_end(&mut buf)?;
            files.insert(fname, buf);
        }

        // Parse MAIN.BMP palette
        let main_pal = parse_bmp_palette(files.get("MAIN.BMP"));
        let chrome_dark = main_pal.get(5).copied().unwrap_or(Color::Rgb(8, 8, 16));
        let chrome_mid = main_pal.get(3).copied().unwrap_or(Color::Rgb(123, 140, 156));
        let chrome_light = main_pal.get(2).copied().unwrap_or(Color::Rgb(189, 206, 214));
        let body_bg = main_pal.get(4).copied().unwrap_or(Color::Rgb(57, 57, 90));
        let titlebar_bg = main_pal.get(6).copied().unwrap_or(Color::Rgb(0, 198, 255));

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

    /// Built-in default skin (the classic Winamp 2.x "base" skin colors).
    pub fn default_skin() -> Self {
        Self {
            name: "Winamp Classic".into(),
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
