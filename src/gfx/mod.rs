//! Terminal image output: protocol detection plus a painter that turns
//! RGBA framebuffers into kitty / iTerm2 / sixel escape sequences.
//!
//! Integration with ratatui: during `terminal.draw`, renderers call
//! [`GfxPainter::begin_frame`], [`GfxPainter::place`] (which blanks the covered
//! buffer cells, and for iTerm2/sixel marks them `skip` so ratatui never
//! overwrites the image), optionally [`GfxPainter::overlay`] for text that must
//! appear above the image, and [`GfxPainter::end_frame`]. After `draw`
//! returns, call [`GfxPainter::flush`] with the backend writer.
//!
//! Only changed pixels are re-sent: each image keeps a copy of what is on
//! screen and emits the cell-aligned bounding box of the difference, at most
//! every [`MIN_INTERVAL`].

pub mod encode;

use std::hash::{Hash, Hasher};
use std::io::{self, Write};
use std::time::{Duration, Instant};

use ratatui::buffer::{Buffer, Cell};
use ratatui::layout::{Position, Rect};
use ratatui::style::{Color, Modifier};

use encode::{Deflater, Rgba, SixelScratch};

/// Graphics transport.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Protocol {
    Kitty,
    Iterm2,
    Sixel,
    /// No pixel protocol: renderers draw `▀` cells themselves.
    HalfBlock,
}

/// Minimum time between content updates (~25 fps).
pub const MIN_INTERVAL: Duration = Duration::from_millis(40);

/// Resolve a config value (`auto|kitty|iterm2|sixel|halfblock`) to a protocol.
pub fn detect(choice: &str) -> Protocol {
    match choice.trim().to_ascii_lowercase().as_str() {
        "kitty" => Protocol::Kitty,
        "iterm2" | "iterm" => Protocol::Iterm2,
        "sixel" => Protocol::Sixel,
        "halfblock" | "half" | "blocks" | "text" | "none" => Protocol::HalfBlock,
        _ => detect_from_env(|k| std::env::var(k).ok()),
    }
}

/// Environment-based detection; `get` reads an environment variable.
pub fn detect_from_env(get: impl Fn(&str) -> Option<String>) -> Protocol {
    let has = |k: &str| get(k).is_some_and(|v| !v.is_empty());
    let term = get("TERM").unwrap_or_default().to_ascii_lowercase();
    let program = get("TERM_PROGRAM").unwrap_or_default().to_ascii_lowercase();
    // Multiplexers swallow or mangle graphics escapes unless specially configured.
    if has("TMUX") || term.starts_with("screen") || term.starts_with("tmux") || has("ZELLIJ") {
        return Protocol::HalfBlock;
    }
    if has("KITTY_WINDOW_ID") || term.contains("kitty") {
        return Protocol::Kitty;
    }
    if has("GHOSTTY_RESOURCES_DIR") || program == "ghostty" || term.contains("ghostty") {
        return Protocol::Kitty;
    }
    if program == "iterm.app"
        || get("LC_TERMINAL").is_some_and(|v| v.eq_ignore_ascii_case("iterm2"))
        || program == "wezterm"
        || has("WEZTERM_EXECUTABLE")
    {
        return Protocol::Iterm2;
    }
    if has("KONSOLE_VERSION")
        || term.contains("foot")
        || term.contains("mlterm")
        || term.contains("contour")
        || program == "mintty"
        || has("WT_SESSION")
    {
        return Protocol::Sixel;
    }
    Protocol::HalfBlock
}

/// What a terminal answered to [`probe`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct ProbeResult {
    pub kitty: bool,
    pub sixel: bool,
    /// Cell size in pixels from `CSI 16 t`.
    pub cell: Option<(u16, u16)>,
}

impl ProbeResult {
    /// Best protocol given the answers (kitty beats sixel), else `fallback`.
    pub fn protocol(&self, fallback: Protocol) -> Protocol {
        if self.kitty {
            Protocol::Kitty
        } else if self.sixel && fallback == Protocol::HalfBlock {
            Protocol::Sixel
        } else {
            fallback
        }
    }
}

/// Parse the replies to the probe queries.
pub fn parse_probe(reply: &[u8]) -> ProbeResult {
    let s = String::from_utf8_lossy(reply);
    let kitty = s.contains("\x1b_Gi=31;OK");
    let mut cell = None;
    if let Some(start) = s.find("\x1b[6;") {
        let rest = &s[start + 4..];
        if let Some(end) = rest.find('t') {
            let mut it = rest[..end].split(';').map(|v| v.parse::<u16>().ok());
            if let (Some(Some(h)), Some(Some(w))) = (it.next(), it.next()) {
                if w > 0 && h > 0 {
                    cell = Some((w, h));
                }
            }
        }
    }
    // DA1: ESC [ ? 62 ; 4 ; ... c — attribute 4 means sixel.
    let sixel = s
        .find("\x1b[?")
        .and_then(|i| {
            let rest = &s[i + 3..];
            rest.find('c').map(|e| &rest[..e])
        })
        .is_some_and(|attrs| attrs.split(';').any(|a| a == "4"));
    ProbeResult { kitty, sixel, cell }
}

/// Ask the terminal about kitty graphics, sixel and its cell size.
///
/// Call once at startup **with raw mode enabled and before any input reader
/// starts**. Every terminal answers the trailing DA1 query, which terminates
/// the read; if nothing arrives within `timeout` the result is empty.
pub fn probe(timeout: Duration) -> ProbeResult {
    use std::io::Read;
    use std::sync::mpsc;

    let mut stdout = io::stdout();
    let query = b"\x1b_Gi=31,s=1,v=1,a=q,t=d,f=24;AAAA\x1b\\\x1b[16t\x1b[c";
    if stdout
        .write_all(query)
        .and_then(|_| stdout.flush())
        .is_err()
    {
        return ProbeResult::default();
    }
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let mut stdin = io::stdin().lock();
        let mut buf = Vec::with_capacity(128);
        let mut byte = [0u8; 1];
        while buf.len() < 1024 {
            match stdin.read(&mut byte) {
                Ok(1) => {
                    buf.push(byte[0]);
                    // DA1 reply ("ESC [ ? … c") is always last.
                    // (it is the only reply ending in 'c').
                    if byte[0] == b'c' && buf.windows(3).any(|w| w == b"\x1b[?") {
                        break;
                    }
                }
                _ => break,
            }
        }
        let _ = tx.send(buf);
    });
    rx.recv_timeout(timeout)
        .map(|reply| parse_probe(&reply))
        .unwrap_or_default()
}

/// Cell size in pixels from the tty, if the terminal reports it.
pub fn query_cell_size() -> Option<(u16, u16)> {
    let ws = crossterm::terminal::window_size().ok()?;
    if ws.columns == 0 || ws.rows == 0 || ws.width == 0 || ws.height == 0 {
        return None;
    }
    Some((ws.width / ws.columns, ws.height / ws.rows)).filter(|(w, h)| *w > 0 && *h > 0)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Geometry {
    width: u32,
    height: u32,
    scale: u32,
    at: (u16, u16),
    cells: Rect,
}

#[derive(Default)]
struct Slot {
    placed: bool,
    dirty: bool,
    geom: Option<Geometry>,
    current: Vec<Rgba>,
    /// Geometry and pixels currently on screen. For kitty `sent` is the base
    /// image; the patch image covers its differences.
    shown: Option<Geometry>,
    sent: Vec<Rgba>,
    patch: bool,
}

/// Half-open pixel rectangle.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct PxRect {
    x0: u32,
    y0: u32,
    x1: u32,
    y1: u32,
}

/// Bounding box of pixels that differ between `a` and `b` (same size).
fn diff_bbox(a: &[Rgba], b: &[Rgba], width: u32) -> Option<PxRect> {
    if a.len() != b.len() || width == 0 {
        return None;
    }
    let w = width as usize;
    let mut r: Option<PxRect> = None;
    for (y, (ra, rb)) in a.chunks_exact(w).zip(b.chunks_exact(w)).enumerate() {
        if ra == rb {
            continue;
        }
        let first = ra.iter().zip(rb).position(|(p, q)| p != q).unwrap_or(0);
        let last = w - ra
            .iter()
            .rev()
            .zip(rb.iter().rev())
            .position(|(p, q)| p != q)
            .unwrap_or(0);
        let y = y as u32;
        r = Some(match r {
            None => PxRect {
                x0: first as u32,
                y0: y,
                x1: last as u32,
                y1: y + 1,
            },
            Some(o) => PxRect {
                x0: o.x0.min(first as u32),
                y0: o.y0,
                x1: o.x1.max(last as u32),
                y1: y + 1,
            },
        });
    }
    r
}

/// Expand an image-pixel rect to whole terminal cells: returns the cell
/// offset/size relative to the image origin.
fn align_to_cells(r: PxRect, scale: u32, cell: (u16, u16)) -> (u16, u16, u16, u16) {
    let (cw, ch) = (u32::from(cell.0.max(1)), u32::from(cell.1.max(1)));
    let cx0 = r.x0 * scale / cw;
    let cy0 = r.y0 * scale / ch;
    let cx1 = (r.x1 * scale).div_ceil(cw);
    let cy1 = (r.y1 * scale).div_ceil(ch);
    (
        cx0 as u16,
        cy0 as u16,
        (cx1 - cx0) as u16,
        (cy1 - cy0) as u16,
    )
}

/// Nearest-neighbour upscale of the scaled-pixel window (rx, ry, rw, rh);
/// pixels outside the image are transparent.
fn scale_region(
    src: &[Rgba],
    width: u32,
    height: u32,
    scale: u32,
    region: (u32, u32, u32, u32),
    out: &mut Vec<Rgba>,
) {
    let (rx, ry, rw, rh) = region;
    out.clear();
    out.reserve(rw as usize * rh as usize);
    let s = scale.max(1);
    for y in ry..ry + rh {
        let sy = y / s;
        for x in rx..rx + rw {
            let sx = x / s;
            out.push(if sx < width && sy < height {
                src[(sy * width + sx) as usize]
            } else {
                [0, 0, 0, 0]
            });
        }
    }
}

const KITTY_ID_BASE: u32 = 0x5275_0000;
const KITTY_Z_BASE: i32 = -2;
const KITTY_Z_PATCH: i32 = -1;

fn kitty_ids(slot: usize) -> (u32, u32) {
    let base = KITTY_ID_BASE + slot as u32 * 2;
    (base, base + 1)
}

pub struct GfxPainter {
    protocol: Protocol,
    cell: Option<(u16, u16)>,
    /// Re-read the cell size from the tty on resize (not for fixed sizes).
    auto_cell: bool,
    screen: Rect,
    slots: Vec<Slot>,
    invalid: bool,
    last_emit: Option<Instant>,
    pending: bool,
    overlay_rects: Vec<Rect>,
    overlay_cells: Vec<(u16, u16, Cell)>,
    overlay_hash: u64,
    overlay_sent_hash: Option<u64>,
    out: Vec<u8>,
    scaled: Vec<Rgba>,
    /// Created on first use: half-block painters never compress.
    deflater: Option<Deflater>,
    b64: Vec<u8>,
    png: Vec<u8>,
    png_scratch: Vec<u8>,
    sixel: SixelScratch,
}

impl GfxPainter {
    /// Painter for `protocol`, reading the cell pixel size from the tty.
    pub fn new(protocol: Protocol) -> Self {
        let cell = if protocol == Protocol::HalfBlock {
            None
        } else {
            query_cell_size()
        };
        let mut p = Self::with_cell_size(protocol, cell);
        p.auto_cell = true;
        p
    }

    /// Painter with a known cell size (e.g. from [`probe`]).
    pub fn with_cell_size(protocol: Protocol, cell: Option<(u16, u16)>) -> Self {
        Self {
            protocol,
            cell,
            auto_cell: false,
            screen: Rect::default(),
            slots: Vec::new(),
            invalid: true,
            last_emit: None,
            pending: false,
            overlay_rects: Vec::new(),
            overlay_cells: Vec::new(),
            overlay_hash: 0,
            overlay_sent_hash: None,
            out: Vec::new(),
            scaled: Vec::new(),
            deflater: None,
            b64: Vec::new(),
            png: Vec::new(),
            png_scratch: Vec::new(),
            sixel: SixelScratch::default(),
        }
    }

    /// Effective protocol: pixel protocols need a known cell size.
    pub fn protocol(&self) -> Protocol {
        match self.cell {
            Some(_) => self.protocol,
            None => Protocol::HalfBlock,
        }
    }

    /// Terminal cell size in pixels (1×2 "pixels" in half-block mode).
    pub fn cell_size(&self) -> (u16, u16) {
        match (self.protocol(), self.cell) {
            (Protocol::HalfBlock, _) | (_, None) => (1, 2),
            (_, Some(c)) => c,
        }
    }

    /// Start a frame drawn into a buffer of size `screen`.
    pub fn begin_frame(&mut self, screen: Rect) {
        if screen != self.screen {
            self.screen = screen;
            self.invalid = true;
            if self.auto_cell && self.protocol != Protocol::HalfBlock {
                // Font size changes usually come with a resize.
                self.cell = query_cell_size().or(self.cell);
            }
        }
        for s in &mut self.slots {
            s.placed = false;
        }
        self.overlay_rects.clear();
    }

    /// Place image `slot` (a small stable integer per window) showing
    /// `pixels` (width×height RGBA) upscaled by `scale`, with its top-left at
    /// cell `at`. Returns the covered cells. Covered cells are blanked; for
    /// iTerm2/sixel they are also marked `skip`.
    pub fn place(
        &mut self,
        buf: &mut Buffer,
        slot: usize,
        pixels: &[Rgba],
        width: u32,
        height: u32,
        scale: u32,
        at: (u16, u16),
    ) -> Rect {
        let protocol = self.protocol();
        if protocol == Protocol::HalfBlock || pixels.len() != width as usize * height as usize {
            return Rect::default();
        }
        let (cw, ch) = self.cell_size();
        let scale = scale.max(1);
        let cols = (width * scale)
            .div_ceil(u32::from(cw))
            .min(u32::from(u16::MAX)) as u16;
        let rows = (height * scale)
            .div_ceil(u32::from(ch))
            .min(u32::from(u16::MAX)) as u16;
        let cells = Rect::new(at.0, at.1, cols, rows).intersection(buf.area);
        if self.slots.len() <= slot {
            self.slots.resize_with(slot + 1, Slot::default);
        }
        let s = &mut self.slots[slot];
        let geom = Geometry {
            width,
            height,
            scale,
            at,
            cells,
        };
        if s.geom != Some(geom) || s.current.as_slice() != pixels {
            s.dirty = true;
            s.current.clear();
            s.current.extend_from_slice(pixels);
        }
        s.geom = Some(geom);
        s.placed = true;
        let skip = protocol != Protocol::Kitty;
        for y in cells.top()..cells.bottom() {
            for x in cells.left()..cells.right() {
                if let Some(c) = buf.cell_mut(Position::new(x, y)) {
                    c.reset();
                    c.set_skip(skip);
                }
            }
        }
        cells
    }

    /// Text drawn into `rect` after `place` must stay visible above the image.
    /// Kitty draws images under text (negative z), so this only matters for
    /// iTerm2/sixel, where the painter re-emits those cells after images.
    pub fn overlay(&mut self, rect: Rect) {
        if matches!(self.protocol(), Protocol::Iterm2 | Protocol::Sixel) {
            self.overlay_rects.push(rect);
        }
    }

    /// Finish the frame (captures overlay text).
    pub fn end_frame(&mut self, buf: &Buffer) {
        self.overlay_cells.clear();
        let mut h = std::collections::hash_map::DefaultHasher::new();
        for r in &self.overlay_rects {
            let r = r.intersection(buf.area);
            for y in r.top()..r.bottom() {
                for x in r.left()..r.right() {
                    if let Some(c) = buf.cell(Position::new(x, y)) {
                        (x, y, c).hash(&mut h);
                        self.overlay_cells.push((x, y, c.clone()));
                    }
                }
            }
        }
        self.overlay_hash = h.finish();
    }

    /// Re-send everything on the next flush (after `terminal.clear()`).
    pub fn invalidate(&mut self) {
        self.invalid = true;
    }

    /// When the caller should draw again to flush rate-limited changes.
    pub fn next_flush_in(&self) -> Option<Duration> {
        if !self.pending {
            return None;
        }
        Some(
            self.last_emit
                .map_or(Duration::ZERO, |t| MIN_INTERVAL.saturating_sub(t.elapsed())),
        )
    }

    /// Write pending image updates. Call right after `terminal.draw`.
    pub fn flush<W: Write>(&mut self, out: &mut W) -> io::Result<()> {
        self.flush_at(out, Instant::now())
    }

    fn flush_at<W: Write>(&mut self, out: &mut W, now: Instant) -> io::Result<()> {
        let protocol = self.protocol();
        if protocol == Protocol::HalfBlock {
            return Ok(());
        }
        let cell = self.cell_size();
        let rate_ok = self
            .last_emit
            .is_none_or(|t| now.duration_since(t) >= MIN_INTERVAL);
        let invalid = std::mem::take(&mut self.invalid);
        self.pending = false;
        let Self {
            slots,
            out: buf,
            scaled,
            deflater,
            b64,
            png,
            png_scratch,
            sixel,
            ..
        } = self;
        buf.clear();
        let deflater = deflater.get_or_insert_with(Deflater::default);
        let mut emitted = false;
        let mut content_update = false;

        for (i, s) in slots.iter_mut().enumerate() {
            let (base_id, patch_id) = kitty_ids(i);
            let Some(geom) = s.geom.filter(|_| s.placed) else {
                if s.shown.take().is_some() && protocol == Protocol::Kitty {
                    encode::kitty_delete(buf, base_id);
                    if std::mem::take(&mut s.patch) {
                        encode::kitty_delete(buf, patch_id);
                    }
                }
                continue;
            };
            if geom.cells.is_empty() {
                continue;
            }
            let full = invalid || s.shown != Some(geom);
            if !full && !s.dirty {
                continue;
            }
            if !full && !rate_ok {
                self.pending = true;
                continue;
            }
            let region_full = (0, 0, geom.cells.width, geom.cells.height);
            let region = if full {
                Some(region_full)
            } else {
                diff_bbox(&s.current, &s.sent, geom.width)
                    .map(|d| align_to_cells(d, geom.scale, cell))
            };
            s.dirty = false;
            match protocol {
                Protocol::Kitty => {
                    let Some(r) = region else {
                        // Content reverted to the base image.
                        if std::mem::take(&mut s.patch) {
                            encode::kitty_delete(buf, patch_id);
                        }
                        continue;
                    };
                    let area = u32::from(r.2) * u32::from(r.3);
                    let total = u32::from(geom.cells.width) * u32::from(geom.cells.height);
                    if full || area * 2 >= total {
                        if s.shown.is_some() {
                            encode::kitty_delete(buf, base_id);
                        }
                        if std::mem::take(&mut s.patch) {
                            encode::kitty_delete(buf, patch_id);
                        }
                        let (w, h) = (geom.width * geom.scale, geom.height * geom.scale);
                        scale_region(
                            &s.current,
                            geom.width,
                            geom.height,
                            geom.scale,
                            (0, 0, w, h),
                            scaled,
                        );
                        encode::cursor_to(buf, geom.at.0, geom.at.1);
                        let z = deflater.zlib(scaled.as_flattened());
                        encode::kitty_transmit(buf, base_id, w, h, z, KITTY_Z_BASE, b64);
                        s.sent.clone_from(&s.current);
                    } else {
                        if std::mem::take(&mut s.patch) {
                            encode::kitty_delete(buf, patch_id);
                        }
                        let px = cell_region_px(r, cell);
                        scale_region(&s.current, geom.width, geom.height, geom.scale, px, scaled);
                        encode::cursor_to(buf, geom.at.0 + r.0, geom.at.1 + r.1);
                        let z = deflater.zlib(scaled.as_flattened());
                        encode::kitty_transmit(buf, patch_id, px.2, px.3, z, KITTY_Z_PATCH, b64);
                        s.patch = true;
                    }
                }
                Protocol::Iterm2 | Protocol::Sixel => {
                    let Some(r) = region else { continue };
                    // Never draw past the visible cell area.
                    let r = (
                        r.0,
                        r.1,
                        r.2.min(geom.cells.width.saturating_sub(r.0)),
                        r.3.min(geom.cells.height.saturating_sub(r.1)),
                    );
                    if r.2 == 0 || r.3 == 0 {
                        continue;
                    }
                    let px = cell_region_px(r, cell);
                    scale_region(&s.current, geom.width, geom.height, geom.scale, px, scaled);
                    encode::cursor_to(buf, geom.at.0 + r.0, geom.at.1 + r.1);
                    if protocol == Protocol::Iterm2 {
                        png.clear();
                        encode::png(px.2, px.3, scaled, deflater, png_scratch, png);
                        encode::iterm2_image(buf, png, r.2, r.3, b64);
                    } else {
                        encode::sixel(buf, px.2 as usize, px.3 as usize, scaled, sixel);
                    }
                    s.sent.clone_from(&s.current);
                }
                Protocol::HalfBlock => {}
            }
            s.shown = Some(geom);
            emitted = true;
            content_update |= !full;
        }

        // Overlay text (iTerm2/sixel): after images, or when it changed.
        let overlay_changed = self.overlay_sent_hash != Some(self.overlay_hash);
        if !self.overlay_cells.is_empty() && (emitted || overlay_changed || invalid) {
            write_cells(&mut self.out, &self.overlay_cells);
            self.overlay_sent_hash = Some(self.overlay_hash);
        } else if self.overlay_cells.is_empty() {
            self.overlay_sent_hash = None;
        }

        if self.out.is_empty() {
            return Ok(());
        }
        if emitted && (content_update || self.last_emit.is_none()) {
            self.last_emit = Some(now);
        }
        out.write_all(b"\x1b7")?;
        out.write_all(&self.out)?;
        out.write_all(b"\x1b8")?;
        out.flush()
    }

    /// Remove every image (kitty deletes; iTerm2/sixel pixels disappear when
    /// ratatui repaints the formerly skipped cells on the next draw).
    pub fn clear<W: Write>(&mut self, out: &mut W) -> io::Result<()> {
        self.out.clear();
        for (i, s) in self.slots.iter_mut().enumerate() {
            if s.shown.take().is_some() && self.protocol == Protocol::Kitty {
                let (base, patch) = kitty_ids(i);
                encode::kitty_delete(&mut self.out, base);
                if s.patch {
                    encode::kitty_delete(&mut self.out, patch);
                }
            }
            s.patch = false;
            s.placed = false;
            s.geom = None;
        }
        self.invalid = true;
        self.overlay_sent_hash = None;
        if self.out.is_empty() {
            return Ok(());
        }
        out.write_all(&self.out)?;
        out.flush()
    }
}

/// Pixel rectangle (x, y, w, h) of a cell region inside the scaled image.
fn cell_region_px(r: (u16, u16, u16, u16), cell: (u16, u16)) -> (u32, u32, u32, u32) {
    let (cw, ch) = (u32::from(cell.0), u32::from(cell.1));
    (
        u32::from(r.0) * cw,
        u32::from(r.1) * ch,
        u32::from(r.2) * cw,
        u32::from(r.3) * ch,
    )
}

fn sgr_color(out: &mut Vec<u8>, c: Color, fg: bool) {
    let base = if fg { 30 } else { 40 };
    let _ = match c {
        Color::Reset => write!(out, "\x1b[{}m", base + 9),
        Color::Rgb(r, g, b) => write!(out, "\x1b[{};2;{r};{g};{b}m", base + 8),
        Color::Indexed(i) => write!(out, "\x1b[{};5;{i}m", base + 8),
        named => {
            let i = match named {
                Color::Black => 0,
                Color::Red => 1,
                Color::Green => 2,
                Color::Yellow => 3,
                Color::Blue => 4,
                Color::Magenta => 5,
                Color::Cyan => 6,
                Color::Gray => 7,
                Color::DarkGray => 8,
                Color::LightRed => 9,
                Color::LightGreen => 10,
                Color::LightYellow => 11,
                Color::LightBlue => 12,
                Color::LightMagenta => 13,
                Color::LightCyan => 14,
                _ => 15,
            };
            write!(out, "\x1b[{};5;{i}m", base + 8)
        }
    };
}

/// Write captured buffer cells (row-major) with their colours.
fn write_cells(out: &mut Vec<u8>, cells: &[(u16, u16, Cell)]) {
    let mut cursor: Option<(u16, u16)> = None;
    for (x, y, c) in cells {
        match cursor {
            // Right half of a wide glyph written just before.
            Some((cx, cy)) if cy == *y && *x < cx => continue,
            Some(pos) if pos == (*x, *y) => {}
            _ => encode::cursor_to(out, *x, *y),
        }
        out.extend_from_slice(b"\x1b[0m");
        if c.modifier.contains(Modifier::BOLD) {
            out.extend_from_slice(b"\x1b[1m");
        }
        sgr_color(out, c.fg, true);
        sgr_color(out, c.bg, false);
        let sym = c.symbol();
        out.extend_from_slice(if sym.is_empty() { b" " } else { sym.as_bytes() });
        let width = unicode_width(sym);
        cursor = Some((x.saturating_add(width), *y));
    }
    out.extend_from_slice(b"\x1b[0m");
}

/// Display width of a cell symbol (wide CJK glyphs occupy two cells).
fn unicode_width(s: &str) -> u16 {
    let wide = s.chars().next().is_some_and(|c| {
        matches!(u32::from(c),
            0x1100..=0x115F
            | 0x2E80..=0xA4CF
            | 0xAC00..=0xD7A3
            | 0xF900..=0xFAFF
            | 0xFE30..=0xFE4F
            | 0xFF00..=0xFF60
            | 0xFFE0..=0xFFE6
            | 0x1F300..=0x1F64F
            | 0x1F900..=0x1F9FF
            | 0x20000..=0x3FFFD)
    });
    if wide {
        2
    } else {
        1
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn env(pairs: &[(&str, &str)]) -> impl Fn(&str) -> Option<String> {
        let owned: Vec<(String, String)> = pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect();
        move |k| owned.iter().find(|(n, _)| n == k).map(|(_, v)| v.clone())
    }

    #[test]
    fn env_detection_matrix() {
        assert_eq!(
            detect_from_env(env(&[("KITTY_WINDOW_ID", "1")])),
            Protocol::Kitty
        );
        assert_eq!(
            detect_from_env(env(&[("TERM", "xterm-kitty")])),
            Protocol::Kitty
        );
        assert_eq!(
            detect_from_env(env(&[("TERM", "xterm-ghostty")])),
            Protocol::Kitty
        );
        assert_eq!(
            detect_from_env(env(&[("TERM_PROGRAM", "iTerm.app")])),
            Protocol::Iterm2
        );
        assert_eq!(
            detect_from_env(env(&[("LC_TERMINAL", "iTerm2")])),
            Protocol::Iterm2
        );
        assert_eq!(
            detect_from_env(env(&[("TERM_PROGRAM", "WezTerm")])),
            Protocol::Iterm2
        );
        assert_eq!(detect_from_env(env(&[("TERM", "foot")])), Protocol::Sixel);
        assert_eq!(
            detect_from_env(env(&[("KONSOLE_VERSION", "230800")])),
            Protocol::Sixel
        );
        assert_eq!(
            detect_from_env(env(&[("TERM", "xterm-256color")])),
            Protocol::HalfBlock
        );
        assert_eq!(
            detect_from_env(env(&[("KITTY_WINDOW_ID", "1"), ("TMUX", "/tmp/x")])),
            Protocol::HalfBlock
        );
        assert_eq!(detect("  Sixel "), Protocol::Sixel);
        assert_eq!(detect("halfblock"), Protocol::HalfBlock);
        assert_eq!(detect("kitty"), Protocol::Kitty);
        assert_eq!(detect("iterm2"), Protocol::Iterm2);
    }

    #[test]
    fn probe_reply_parsing() {
        let r = parse_probe(b"\x1b_Gi=31;OK\x1b\\\x1b[6;20;10t\x1b[?62;4;22c");
        assert_eq!(
            r,
            ProbeResult {
                kitty: true,
                sixel: true,
                cell: Some((10, 20))
            }
        );
        assert_eq!(r.protocol(Protocol::HalfBlock), Protocol::Kitty);
        let r = parse_probe(b"\x1b[?62;22c");
        assert_eq!(r, ProbeResult::default());
        let r = parse_probe(b"\x1b[?64;4c");
        assert_eq!(r.protocol(Protocol::HalfBlock), Protocol::Sixel);
        assert_eq!(r.protocol(Protocol::Iterm2), Protocol::Iterm2);
    }

    #[test]
    fn diff_and_alignment() {
        let a = vec![[0u8; 4]; 20];
        let mut b = a.clone();
        assert_eq!(diff_bbox(&a, &b, 5), None);
        b[5 + 2] = [1, 0, 0, 0]; // (2,1)
        b[15 + 3] = [1, 0, 0, 0]; // (3,3)
        let d = diff_bbox(&a, &b, 5).unwrap();
        assert_eq!(
            d,
            PxRect {
                x0: 2,
                y0: 1,
                x1: 4,
                y1: 4
            }
        );
        // scale 3, cells 4x8 px: x 6..12 → cols 1..3, y 3..12 → rows 0..2.
        assert_eq!(align_to_cells(d, 3, (4, 8)), (1, 0, 2, 2));
    }

    #[test]
    fn scaling_pads_outside_with_transparency() {
        let src = vec![[1, 1, 1, 255], [2, 2, 2, 255]];
        let mut out = Vec::new();
        scale_region(&src, 2, 1, 2, (2, 0, 4, 3), &mut out);
        assert_eq!(out.len(), 12);
        assert_eq!(out[0], [2, 2, 2, 255]);
        assert_eq!(out[1], [2, 2, 2, 255]);
        assert_eq!(out[2], [0, 0, 0, 0]);
        assert_eq!(out[4 * 2], [0, 0, 0, 0]); // row 2 is below the image
    }

    fn frame(p: &mut GfxPainter, buf: &mut Buffer, pixels: &[Rgba]) -> Rect {
        p.begin_frame(buf.area);
        let r = p.place(buf, 0, pixels, 4, 4, 2, (1, 1));
        p.end_frame(buf);
        r
    }

    #[test]
    fn kitty_painter_sends_full_then_patches_then_deletes() {
        let mut p = GfxPainter::with_cell_size(Protocol::Kitty, Some((4, 4)));
        let mut buf = Buffer::empty(Rect::new(0, 0, 10, 10));
        let mut px = vec![[9, 9, 9, 255]; 16];
        let cells = frame(&mut p, &mut buf, &px);
        assert_eq!(cells, Rect::new(1, 1, 2, 2));
        assert!(!buf.cell(Position::new(1, 1)).unwrap().skip);
        let t0 = Instant::now();
        let mut out = Vec::new();
        p.flush_at(&mut out, t0).unwrap();
        let s = String::from_utf8_lossy(&out).to_string();
        assert!(
            s.starts_with("\x1b7\x1b[2;2H\x1b_Ga=T,f=32,o=z,q=2,C=1,i=1383399424,p=1,s=8,v=8,z=-2")
        );
        assert!(s.ends_with("\x1b8"));

        // Unchanged frame: nothing to send.
        frame(&mut p, &mut buf, &px);
        let mut out = Vec::new();
        p.flush_at(&mut out, t0 + MIN_INTERVAL).unwrap();
        assert!(out.is_empty());

        // One pixel changes: a one-cell patch with the patch id, above the base.
        px[0] = [1, 2, 3, 255];
        frame(&mut p, &mut buf, &px);
        let mut out = Vec::new();
        // Too soon: rate limited.
        p.flush_at(&mut out, t0 + Duration::from_millis(1)).unwrap();
        assert!(out.is_empty());
        assert!(p.next_flush_in().is_some());
        p.flush_at(&mut out, t0 + MIN_INTERVAL).unwrap();
        let s = String::from_utf8_lossy(&out).to_string();
        assert!(s.contains("i=1383399425,p=1,s=4,v=4,z=-1"));

        // Image no longer placed: deleted.
        p.begin_frame(buf.area);
        p.end_frame(&buf);
        let mut out = Vec::new();
        p.flush_at(&mut out, t0 + MIN_INTERVAL * 3).unwrap();
        let s = String::from_utf8_lossy(&out).to_string();
        assert!(s.contains("a=d,d=I,i=1383399424"));
        assert!(s.contains("a=d,d=I,i=1383399425"));
    }

    #[test]
    fn sixel_painter_skips_cells_and_reemits_overlay() {
        let mut p = GfxPainter::with_cell_size(Protocol::Sixel, Some((2, 2)));
        let mut buf = Buffer::empty(Rect::new(0, 0, 10, 10));
        let px = vec![[9, 9, 9, 255]; 16];
        p.begin_frame(buf.area);
        let cells = p.place(&mut buf, 0, &px, 4, 4, 1, (0, 0));
        assert_eq!(cells, Rect::new(0, 0, 2, 2));
        assert!(buf.cell(Position::new(1, 1)).unwrap().skip);
        buf.set_string(
            0,
            1,
            "A",
            ratatui::style::Style::default().fg(Color::Rgb(1, 2, 3)),
        );
        p.overlay(Rect::new(0, 1, 1, 1));
        p.end_frame(&buf);
        let mut out = Vec::new();
        p.flush_at(&mut out, Instant::now()).unwrap();
        let s = String::from_utf8_lossy(&out).to_string();
        let img = s.find("\x1bP0;1;0q").unwrap();
        let text = s.find("\x1b[38;2;1;2;3m\x1b[49mA").unwrap();
        assert!(text > img, "overlay text must follow the image");
    }

    #[test]
    fn halfblock_painter_is_inert() {
        let mut p = GfxPainter::with_cell_size(Protocol::Kitty, None);
        assert_eq!(p.protocol(), Protocol::HalfBlock);
        assert_eq!(p.cell_size(), (1, 2));
        let mut buf = Buffer::empty(Rect::new(0, 0, 4, 4));
        assert_eq!(
            p.place(&mut buf, 0, &[[0; 4]; 4], 2, 2, 1, (0, 0)),
            Rect::default()
        );
        let mut out = Vec::new();
        p.flush(&mut out).unwrap();
        assert!(out.is_empty());
    }
}
