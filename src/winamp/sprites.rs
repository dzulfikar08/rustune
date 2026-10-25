//! Sprite source rectangles and on-window positions for Winamp 2.x skins.
//!
//! Source rectangles are taken verbatim from webamp's sprite table:
//! https://github.com/captbaritone/webamp/blob/master/packages/webamp/js/skinSprites.ts
//! Window positions come from webamp's stylesheets:
//! https://github.com/captbaritone/webamp/blob/master/packages/webamp/css/main-window.css
//! https://github.com/captbaritone/webamp/blob/master/packages/webamp/css/equalizer-window.css
//! https://github.com/captbaritone/webamp/blob/master/packages/webamp/css/playlist-window.css

/// A rectangle in sprite-sheet or window pixels.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Px {
    pub x: i32,
    pub y: i32,
    pub w: u32,
    pub h: u32,
}

pub const fn px(x: i32, y: i32, w: u32, h: u32) -> Px {
    Px { x, y, w, h }
}

impl Px {
    pub fn contains(self, x: f32, y: f32) -> bool {
        x >= self.x as f32
            && y >= self.y as f32
            && x < (self.x + self.w as i32) as f32
            && y < (self.y + self.h as i32) as f32
    }

    /// Same size, shifted by (dx, dy).
    pub const fn offset(self, dx: i32, dy: i32) -> Px {
        Px {
            x: self.x + dx,
            y: self.y + dy,
            w: self.w,
            h: self.h,
        }
    }
}

pub const WINDOW_W: u32 = 275;
pub const WINDOW_H: u32 = 116;

// ── MAIN.BMP / TITLEBAR.BMP ────────────────────────────────────────────
pub const MAIN_BG: Px = px(0, 0, 275, 116);
pub const TITLE_BAR: Px = px(27, 15, 275, 14);
pub const TITLE_BAR_SELECTED: Px = px(27, 0, 275, 14);
pub const OPTIONS_DEPRESSED: Px = px(0, 9, 9, 9);
pub const MINIMIZE_DEPRESSED: Px = px(9, 9, 9, 9);
pub const SHADE_DEPRESSED: Px = px(9, 18, 9, 9);
pub const CLOSE_DEPRESSED: Px = px(18, 9, 9, 9);
pub const CLUTTER_BAR: Px = px(304, 0, 8, 43);

/// Positions inside the main window.
pub mod main_pos {
    use super::{px, Px};
    pub const OPTIONS: Px = px(6, 3, 9, 9);
    pub const MINIMIZE: Px = px(244, 3, 9, 9);
    pub const SHADE: Px = px(254, 3, 9, 9);
    pub const CLOSE: Px = px(264, 3, 9, 9);
    pub const CLUTTER_BAR: Px = px(10, 22, 8, 43);
    pub const PLAY_PAUSE: Px = px(26, 28, 9, 9);
    pub const WORK_INDICATOR: Px = px(24, 28, 3, 9);
    pub const TIME: Px = px(39, 26, 59, 13);
    /// x offsets of the four digits relative to `TIME.x`.
    pub const DIGIT_X: [i32; 4] = [9, 21, 39, 51];
    pub const VIS: Px = px(24, 43, 76, 16);
    /// Click target includes webamp's 3px padding.
    pub const MARQUEE: Px = px(111, 24, 154, 12);
    pub const MARQUEE_TEXT_Y: i32 = 27;
    pub const KBPS: Px = px(111, 43, 15, 6);
    pub const KHZ: Px = px(156, 43, 10, 6);
    pub const MONO: Px = px(212, 41, 27, 12);
    pub const STEREO: Px = px(239, 41, 29, 12);
    pub const VOLUME: Px = px(107, 57, 68, 13);
    pub const BALANCE: Px = px(177, 57, 38, 13);
    pub const EQ_BUTTON: Px = px(219, 58, 23, 12);
    pub const PL_BUTTON: Px = px(242, 58, 23, 12);
    pub const POSITION: Px = px(16, 72, 248, 10);
    pub const PREVIOUS: Px = px(16, 88, 23, 18);
    pub const PLAY: Px = px(39, 88, 23, 18);
    pub const PAUSE: Px = px(62, 88, 23, 18);
    pub const STOP: Px = px(85, 88, 23, 18);
    pub const NEXT: Px = px(108, 88, 22, 18);
    pub const EJECT: Px = px(136, 89, 22, 16);
    pub const SHUFFLE: Px = px(164, 89, 47, 15);
    pub const REPEAT: Px = px(210, 89, 28, 15);
}

// ── CBUTTONS.BMP ───────────────────────────────────────────────────────
pub const PREVIOUS: Px = px(0, 0, 23, 18);
pub const PLAY: Px = px(23, 0, 23, 18);
pub const PAUSE: Px = px(46, 0, 23, 18);
pub const STOP: Px = px(69, 0, 23, 18);
pub const NEXT: Px = px(92, 0, 22, 18);
pub const EJECT: Px = px(114, 0, 22, 16);
/// Pressed variants sit directly below (eject: 16px below).
pub const CBUTTON_PRESSED_DY: i32 = 18;
pub const EJECT_PRESSED_DY: i32 = 16;

// ── SHUFREP.BMP ────────────────────────────────────────────────────────
/// Row offsets: normal, depressed, selected, selected+depressed.
pub const SHUFREP_ROWS: [i32; 4] = [0, 15, 30, 45];
pub const SHUFFLE: Px = px(28, 0, 47, 15);
pub const REPEAT: Px = px(0, 0, 28, 15);
pub const EQ_BUTTON: Px = px(0, 61, 23, 12);
pub const PL_BUTTON: Px = px(23, 61, 23, 12);
/// EQ/PL button offsets: +12 selected, +46 depressed.
pub const WINDOW_BUTTON_SELECTED_DY: i32 = 12;
pub const WINDOW_BUTTON_DEPRESSED_DX: i32 = 46;

// ── NUMBERS.BMP / NUMS_EX.BMP ──────────────────────────────────────────
pub const DIGIT_W: u32 = 9;
pub const DIGIT_H: u32 = 13;
pub const MINUS_SIGN: Px = px(20, 6, 5, 1);
pub const MINUS_SIGN_EX: Px = px(99, 0, 9, 13);

// ── PLAYPAUS.BMP ───────────────────────────────────────────────────────
pub const PLAYING_INDICATOR: Px = px(0, 0, 9, 9);
pub const PAUSED_INDICATOR: Px = px(9, 0, 9, 9);
pub const STOPPED_INDICATOR: Px = px(18, 0, 9, 9);
pub const NOT_WORKING_INDICATOR: Px = px(36, 0, 3, 9);

// ── MONOSTER.BMP ───────────────────────────────────────────────────────
pub const STEREO_SELECTED: Px = px(0, 0, 29, 12);
pub const STEREO: Px = px(0, 12, 29, 12);
pub const MONO_SELECTED: Px = px(29, 0, 27, 12);
pub const MONO: Px = px(29, 12, 27, 12);

// ── POSBAR.BMP ─────────────────────────────────────────────────────────
pub const POSBAR_BG: Px = px(0, 0, 248, 10);
pub const POSBAR_THUMB: Px = px(248, 0, 29, 10);
pub const POSBAR_THUMB_SELECTED: Px = px(278, 0, 29, 10);
/// Travel of the thumb: 248 - 29.
pub const POSBAR_TRAVEL: f32 = 219.0;

// ── VOLUME.BMP / BALANCE.BMP ───────────────────────────────────────────
/// 28 background frames, 15px apart, 13px tall.
pub const VOLUME_FRAME: Px = px(0, 0, 68, 13);
pub const BALANCE_FRAME: Px = px(9, 0, 38, 13);
pub const SLIDER_FRAME_DY: i32 = 15;
pub const SLIDER_THUMB: Px = px(15, 422, 14, 11);
pub const SLIDER_THUMB_ACTIVE: Px = px(0, 422, 14, 11);
/// Thumb travel: input width (65 / 38) minus thumb width 14.
pub const VOLUME_TRAVEL: f32 = 51.0;
pub const BALANCE_TRAVEL: f32 = 24.0;

// ── TEXT.BMP ───────────────────────────────────────────────────────────
pub const CHAR_W: u32 = 5;
pub const CHAR_H: u32 = 6;

/// (row, column) of a glyph in TEXT.BMP; webamp `FONT_LOOKUP`.
pub fn glyph_cell(c: char) -> (u32, u32) {
    let c = c.to_ascii_lowercase();
    match c {
        'a'..='z' => (0, c as u32 - 'a' as u32),
        '"' => (0, 26),
        '@' => (0, 27),
        '0'..='9' => (1, c as u32 - '0' as u32),
        '…' => (1, 10),
        '.' => (1, 11),
        ':' => (1, 12),
        '(' => (1, 13),
        ')' => (1, 14),
        '-' => (1, 15),
        '\'' => (1, 16),
        '!' => (1, 17),
        '_' => (1, 18),
        '+' => (1, 19),
        '\\' => (1, 20),
        '/' => (1, 21),
        '[' | '<' | '{' => (1, 22),
        ']' | '>' | '}' => (1, 23),
        '^' => (1, 24),
        '&' => (1, 25),
        '%' => (1, 26),
        ',' => (1, 27),
        '=' => (1, 28),
        '$' => (1, 29),
        '#' => (1, 30),
        'å' | 'Å' => (2, 0),
        'ö' | 'Ö' => (2, 1),
        'ä' | 'Ä' => (2, 2),
        '?' => (2, 3),
        '*' => (2, 4),
        _ => (0, 30), // space
    }
}

// ── EQMAIN.BMP ─────────────────────────────────────────────────────────
pub const EQ_BG: Px = px(0, 0, 275, 116);
pub const EQ_TITLE_BAR: Px = px(0, 149, 275, 14);
pub const EQ_TITLE_BAR_SELECTED: Px = px(0, 134, 275, 14);
pub const EQ_SLIDER_BG: Px = px(13, 164, 209, 129);
pub const EQ_SLIDER_THUMB: Px = px(0, 164, 11, 11);
pub const EQ_SLIDER_THUMB_SELECTED: Px = px(0, 176, 11, 11);
pub const EQ_CLOSE_ACTIVE: Px = px(0, 125, 9, 9);
pub const EQ_ON: Px = px(10, 119, 26, 12);
pub const EQ_ON_DEPRESSED: Px = px(128, 119, 26, 12);
pub const EQ_ON_SELECTED: Px = px(69, 119, 26, 12);
pub const EQ_ON_SELECTED_DEPRESSED: Px = px(187, 119, 26, 12);
pub const EQ_AUTO: Px = px(36, 119, 32, 12);
pub const EQ_AUTO_DEPRESSED: Px = px(154, 119, 32, 12);
pub const EQ_AUTO_SELECTED: Px = px(95, 119, 32, 12);
pub const EQ_AUTO_SELECTED_DEPRESSED: Px = px(213, 119, 32, 12);
pub const EQ_GRAPH_BG: Px = px(0, 294, 113, 19);
pub const EQ_GRAPH_LINE_COLORS: Px = px(115, 294, 1, 19);
pub const EQ_PRESETS: Px = px(224, 164, 44, 12);
pub const EQ_PRESETS_SELECTED: Px = px(224, 176, 44, 12);
pub const EQ_PREAMP_LINE: Px = px(0, 314, 113, 1);
/// Each band background frame is 14x63, laid out 14 per row, 15x65 apart.
pub const EQ_BAND_FRAME_W: u32 = 14;
pub const EQ_BAND_FRAME_H: u32 = 63;
/// Thumb travel inside the 62px slider: 62 - 11.
pub const EQ_THUMB_TRAVEL: f32 = 51.0;

pub mod eq_pos {
    use super::{px, Px};
    pub const CLOSE: Px = px(264, 3, 9, 9);
    pub const SHADE: Px = px(254, 3, 9, 9);
    pub const ON: Px = px(14, 18, 26, 12);
    pub const AUTO: Px = px(40, 18, 32, 12);
    pub const PRESETS: Px = px(217, 18, 44, 12);
    pub const GRAPH: Px = px(86, 17, 113, 19);
    pub const PREAMP: Px = px(21, 38, 14, 63);
    /// Band n is at x = 78 + 18n.
    pub const BAND0: Px = px(78, 38, 14, 63);
    pub const BAND_DX: i32 = 18;
}

// ── PLEDIT.BMP ─────────────────────────────────────────────────────────
pub const PL_TOP_TILE: Px = px(127, 21, 25, 20);
pub const PL_TOP_LEFT: Px = px(0, 21, 25, 20);
pub const PL_TITLE: Px = px(26, 21, 100, 20);
pub const PL_TOP_RIGHT: Px = px(153, 21, 25, 20);
pub const PL_TOP_TILE_SELECTED: Px = px(127, 0, 25, 20);
pub const PL_TOP_LEFT_SELECTED: Px = px(0, 0, 25, 20);
pub const PL_TITLE_SELECTED: Px = px(26, 0, 100, 20);
pub const PL_TOP_RIGHT_SELECTED: Px = px(153, 0, 25, 20);
pub const PL_LEFT_TILE: Px = px(0, 42, 12, 29);
pub const PL_RIGHT_TILE: Px = px(31, 42, 20, 29);
pub const PL_BOTTOM_TILE: Px = px(179, 0, 25, 38);
pub const PL_BOTTOM_LEFT: Px = px(0, 72, 125, 38);
pub const PL_BOTTOM_RIGHT: Px = px(126, 72, 150, 38);
pub const PL_SCROLL_HANDLE: Px = px(52, 53, 8, 18);
pub const PL_SCROLL_HANDLE_SELECTED: Px = px(61, 53, 8, 18);
pub const PL_CLOSE_SELECTED: Px = px(52, 42, 9, 9);

pub const PL_TOP_H: u32 = 20;
pub const PL_BOTTOM_H: u32 = 38;
pub const PL_ROW_STEP: u32 = 29;
pub const PL_LEFT_W: u32 = 12;
pub const PL_RIGHT_W: u32 = 20;
/// Track rows in Winamp are 13px tall.
pub const PL_TRACK_H: u32 = 13;

/// Playlist positions that depend on the window height `h` (width is 275).
pub mod pl_pos {
    use super::{px, Px, PL_BOTTOM_H, WINDOW_W};
    pub const TITLE_X: i32 = 87;
    pub const CLOSE: Px = px(WINDOW_W as i32 - 11, 3, 9, 9);
    pub const SCROLL_X: i32 = WINDOW_W as i32 - 15;
    pub const fn bottom_y(h: u32) -> i32 {
        h as i32 - PL_BOTTOM_H as i32
    }
    /// The ADD menu button (bottom-left).
    pub const fn add(h: u32) -> Px {
        px(14, bottom_y(h) + 8, 22, 18)
    }
    /// Running time "sel/total" text origin.
    pub const fn running_time(h: u32) -> (i32, i32) {
        (WINDOW_W as i32 - 150 + 7, bottom_y(h) + 10)
    }
    /// Mini transport: six 10x10 buttons from here.
    pub const fn action_buttons(h: u32) -> Px {
        px(WINDOW_W as i32 - 150 + 3, bottom_y(h) + 22, 60, 10)
    }
    pub const fn mini_time(h: u32) -> (i32, i32) {
        (WINDOW_W as i32 - 150 + 66, bottom_y(h) + 23)
    }
}
