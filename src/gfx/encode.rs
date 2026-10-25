//! Byte-level encoders for terminal graphics: base64, zlib, PNG, and the
//! kitty / iTerm2 / sixel escape sequences. All writers append to caller
//! owned buffers so the painter can reuse allocations across frames.

use std::io::Write as _;

use flate2::{Compress, Compression, Crc, FlushCompress, Status};

pub type Rgba = [u8; 4];

const B64: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

/// Standard base64 with padding, appended to `out`.
pub fn base64(input: &[u8], out: &mut Vec<u8>) {
    out.reserve(input.len().div_ceil(3) * 4);
    let mut chunks = input.chunks_exact(3);
    for c in &mut chunks {
        let n = (u32::from(c[0]) << 16) | (u32::from(c[1]) << 8) | u32::from(c[2]);
        out.extend_from_slice(&[
            B64[(n >> 18) as usize & 63],
            B64[(n >> 12) as usize & 63],
            B64[(n >> 6) as usize & 63],
            B64[n as usize & 63],
        ]);
    }
    match *chunks.remainder() {
        [a] => {
            let n = u32::from(a) << 16;
            out.extend_from_slice(&[
                B64[(n >> 18) as usize & 63],
                B64[(n >> 12) as usize & 63],
                b'=',
                b'=',
            ]);
        }
        [a, b] => {
            let n = (u32::from(a) << 16) | (u32::from(b) << 8);
            out.extend_from_slice(&[
                B64[(n >> 18) as usize & 63],
                B64[(n >> 12) as usize & 63],
                B64[(n >> 6) as usize & 63],
                b'=',
            ]);
        }
        _ => {}
    }
}

/// Reusable zlib compressor (fast level; skins compress extremely well).
pub struct Deflater {
    c: Compress,
    out: Vec<u8>,
}

impl Default for Deflater {
    fn default() -> Self {
        Self {
            c: Compress::new(Compression::fast(), true),
            out: Vec::new(),
        }
    }
}

impl Deflater {
    /// zlib-compress `data`; the result borrows the internal buffer.
    pub fn zlib(&mut self, data: &[u8]) -> &[u8] {
        self.c.reset();
        self.out.clear();
        self.out.reserve(data.len() / 4 + 64);
        loop {
            let consumed = (self.c.total_in() as usize).min(data.len());
            match self
                .c
                .compress_vec(&data[consumed..], &mut self.out, FlushCompress::Finish)
            {
                Ok(Status::StreamEnd) => break,
                Ok(_) => {
                    let grow = self.out.capacity().max(1024);
                    self.out.reserve(grow);
                }
                Err(_) => {
                    self.out.clear();
                    break;
                }
            }
        }
        &self.out
    }
}

fn png_chunk(out: &mut Vec<u8>, ty: &[u8; 4], data: &[u8]) {
    out.extend_from_slice(&(data.len() as u32).to_be_bytes());
    out.extend_from_slice(ty);
    out.extend_from_slice(data);
    let mut crc = Crc::new();
    crc.update(ty);
    crc.update(data);
    out.extend_from_slice(&crc.sum().to_be_bytes());
}

/// Minimal RGBA8 PNG encoder (filter 0 on every scanline).
pub fn png(
    width: u32,
    height: u32,
    rgba: &[Rgba],
    deflater: &mut Deflater,
    scratch: &mut Vec<u8>,
    out: &mut Vec<u8>,
) {
    scratch.clear();
    scratch.reserve((width as usize * 4 + 1) * height as usize);
    for row in rgba
        .chunks_exact(width.max(1) as usize)
        .take(height as usize)
    {
        scratch.push(0);
        scratch.extend_from_slice(row.as_flattened());
    }
    out.extend_from_slice(b"\x89PNG\r\n\x1a\n");
    let mut ihdr = [0u8; 13];
    ihdr[0..4].copy_from_slice(&width.to_be_bytes());
    ihdr[4..8].copy_from_slice(&height.to_be_bytes());
    ihdr[8] = 8; // bit depth
    ihdr[9] = 6; // colour type RGBA
    png_chunk(out, b"IHDR", &ihdr);
    png_chunk(out, b"IDAT", deflater.zlib(scratch));
    png_chunk(out, b"IEND", &[]);
}

/// Move the cursor to the 0-based cell (col, row).
pub fn cursor_to(out: &mut Vec<u8>, col: u16, row: u16) {
    let _ = write!(out, "\x1b[{};{}H", u32::from(row) + 1, u32::from(col) + 1);
}

/// Kitty graphics base64 payload chunk size (must be a multiple of 4).
pub const KITTY_CHUNK: usize = 4096;

/// Transmit-and-display a zlib-compressed RGBA image at the cursor position.
/// `C=1` keeps the cursor still, `q=2` silences replies, `p=1` gives the
/// placement a stable id so re-transmitting replaces it.
pub fn kitty_transmit(
    out: &mut Vec<u8>,
    id: u32,
    width: u32,
    height: u32,
    zlib: &[u8],
    z: i32,
    b64: &mut Vec<u8>,
) {
    b64.clear();
    base64(zlib, b64);
    let mut chunks = b64.chunks(KITTY_CHUNK).peekable();
    let mut first = true;
    if chunks.peek().is_none() {
        return;
    }
    while let Some(chunk) = chunks.next() {
        let more = u8::from(chunks.peek().is_some());
        if first {
            let _ = write!(
                out,
                "\x1b_Ga=T,f=32,o=z,q=2,C=1,i={id},p=1,s={width},v={height},z={z},m={more};"
            );
            first = false;
        } else {
            let _ = write!(out, "\x1b_Gm={more};");
        }
        out.extend_from_slice(chunk);
        out.extend_from_slice(b"\x1b\\");
    }
}

/// Delete a kitty image and free its data.
pub fn kitty_delete(out: &mut Vec<u8>, id: u32) {
    let _ = write!(out, "\x1b_Ga=d,d=I,i={id},q=2\x1b\\");
}

/// iTerm2 inline image (OSC 1337) sized in cells, cursor left in place.
pub fn iterm2_image(out: &mut Vec<u8>, png: &[u8], cols: u16, rows: u16, b64: &mut Vec<u8>) {
    b64.clear();
    base64(png, b64);
    let _ = write!(
        out,
        "\x1b]1337;File=inline=1;size={};width={cols};height={rows};preserveAspectRatio=0;doNotMoveCursor=1:",
        png.len()
    );
    out.extend_from_slice(b64);
    out.push(0x07);
}

/// Scratch buffers for sixel encoding.
#[derive(Default)]
pub struct SixelScratch {
    indices: Vec<u16>,
    palette: Vec<Rgba>,
    lookup: std::collections::HashMap<u32, u16>,
    present: Vec<bool>,
}

const TRANSPARENT: u16 = u16::MAX;

/// 6×7×6 colour cube index for skins with more than 256 colours.
fn cube(c: Rgba) -> Rgba {
    let q = |v: u8, n: u32| -> u8 {
        let step = (u32::from(v) * (n - 1) + 127) / 255;
        (step * 255 / (n - 1)) as u8
    };
    [q(c[0], 6), q(c[1], 7), q(c[2], 6), 255]
}

/// Encode `rgba` (w×h) as a sixel image. Pixels with alpha 0 are left
/// transparent (P2=1). Up to 256 distinct colours are exact; beyond that the
/// image is quantised to a 252-colour cube.
pub fn sixel(out: &mut Vec<u8>, w: usize, h: usize, rgba: &[Rgba], s: &mut SixelScratch) {
    s.indices.clear();
    s.palette.clear();
    s.lookup.clear();
    let distinct = {
        for p in rgba.iter().filter(|p| p[3] != 0) {
            let key = u32::from_be_bytes([p[0], p[1], p[2], 0]);
            if s.lookup.len() > 256 {
                break;
            }
            s.lookup.entry(key).or_insert(0);
        }
        s.lookup.len()
    };
    let quantise = distinct > 256;
    s.lookup.clear();
    s.indices.reserve(rgba.len());
    for p in rgba {
        if p[3] == 0 {
            s.indices.push(TRANSPARENT);
            continue;
        }
        let c = if quantise { cube(*p) } else { *p };
        let key = u32::from_be_bytes([c[0], c[1], c[2], 0]);
        let next = s.palette.len() as u16;
        let idx = *s.lookup.entry(key).or_insert(next);
        if idx == next {
            s.palette.push(c);
        }
        s.indices.push(idx);
    }

    let _ = write!(out, "\x1bP0;1;0q\"1;1;{w};{h}");
    for (i, c) in s.palette.iter().enumerate() {
        let pct = |v: u8| (u32::from(v) * 100 + 127) / 255;
        let _ = write!(out, "#{i};2;{};{};{}", pct(c[0]), pct(c[1]), pct(c[2]));
    }
    s.present.clear();
    s.present.resize(s.palette.len(), false);
    let bands = h.div_ceil(6);
    for band in 0..bands {
        let y0 = band * 6;
        let rows = (h - y0).min(6);
        s.present.iter_mut().for_each(|p| *p = false);
        for y in y0..y0 + rows {
            for &idx in &s.indices[y * w..(y + 1) * w] {
                if idx != TRANSPARENT {
                    s.present[idx as usize] = true;
                }
            }
        }
        for color in 0..s.palette.len() {
            if !s.present[color] {
                continue;
            }
            let _ = write!(out, "#{color}");
            let mut run_char = 0u8;
            let mut run_len = 0usize;
            for x in 0..w {
                let mut bits = 0u8;
                for r in 0..rows {
                    if s.indices[(y0 + r) * w + x] == color as u16 {
                        bits |= 1 << r;
                    }
                }
                let ch = 63 + bits;
                if ch == run_char {
                    run_len += 1;
                } else {
                    flush_run(out, run_char, run_len);
                    run_char = ch;
                    run_len = 1;
                }
            }
            flush_run(out, run_char, run_len);
            out.push(b'$');
        }
        if band + 1 < bands {
            out.push(b'-');
        }
    }
    out.extend_from_slice(b"\x1b\\");
}

fn flush_run(out: &mut Vec<u8>, ch: u8, len: usize) {
    match len {
        0 => {}
        1..=3 => out.extend(std::iter::repeat_n(ch, len)),
        _ => {
            let _ = write!(out, "!{len}");
            out.push(ch);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn b64(s: &[u8]) -> String {
        let mut out = Vec::new();
        base64(s, &mut out);
        String::from_utf8(out).unwrap()
    }

    #[test]
    fn base64_rfc4648_vectors() {
        assert_eq!(b64(b""), "");
        assert_eq!(b64(b"f"), "Zg==");
        assert_eq!(b64(b"fo"), "Zm8=");
        assert_eq!(b64(b"foo"), "Zm9v");
        assert_eq!(b64(b"foob"), "Zm9vYg==");
        assert_eq!(b64(b"fooba"), "Zm9vYmE=");
        assert_eq!(b64(b"foobar"), "Zm9vYmFy");
    }

    #[test]
    fn zlib_roundtrip() {
        let data: Vec<u8> = (0..10_000u32).map(|i| (i % 7) as u8).collect();
        let mut d = Deflater::default();
        let z = d.zlib(&data).to_vec();
        assert_eq!(z[0], 0x78); // zlib header
        let mut back = Vec::new();
        let mut dec = flate2::write::ZlibDecoder::new(&mut back);
        std::io::Write::write_all(&mut dec, &z).unwrap();
        dec.finish().unwrap();
        assert_eq!(back, data);
        // Reuse produces identical output.
        assert_eq!(d.zlib(&data), &z[..]);
    }

    fn be32(b: &[u8]) -> u32 {
        u32::from_be_bytes([b[0], b[1], b[2], b[3]])
    }

    #[test]
    fn png_structure_and_crcs() {
        let px = vec![
            [255, 0, 0, 255],
            [0, 255, 0, 128],
            [0, 0, 255, 0],
            [1, 2, 3, 4],
        ];
        let mut out = Vec::new();
        png(
            2,
            2,
            &px,
            &mut Deflater::default(),
            &mut Vec::new(),
            &mut out,
        );
        assert_eq!(&out[..8], b"\x89PNG\r\n\x1a\n");
        // Walk chunks and verify every CRC.
        let mut pos = 8;
        let mut types = Vec::new();
        let mut idat = Vec::new();
        while pos < out.len() {
            let len = be32(&out[pos..]) as usize;
            let ty = &out[pos + 4..pos + 8];
            let data = &out[pos + 8..pos + 8 + len];
            let crc = be32(&out[pos + 8 + len..]);
            let mut c = Crc::new();
            c.update(ty);
            c.update(data);
            assert_eq!(c.sum(), crc);
            if ty == b"IDAT" {
                idat.extend_from_slice(data);
            }
            if ty == b"IHDR" {
                assert_eq!(be32(data), 2);
                assert_eq!(be32(&data[4..]), 2);
                assert_eq!(&data[8..], &[8, 6, 0, 0, 0]);
            }
            types.push(String::from_utf8(ty.to_vec()).unwrap());
            pos += 12 + len;
        }
        assert_eq!(types, ["IHDR", "IDAT", "IEND"]);
        // IEND's CRC is the well-known constant.
        assert_eq!(&out[out.len() - 4..], &[0xAE, 0x42, 0x60, 0x82]);
        let mut raw = Vec::new();
        let mut dec = flate2::write::ZlibDecoder::new(&mut raw);
        std::io::Write::write_all(&mut dec, &idat).unwrap();
        dec.finish().unwrap();
        // Each scanline: filter byte 0 followed by the row's RGBA bytes.
        let expected: [u8; 18] = [
            0, 255, 0, 0, 255, 0, 255, 0, 128, //
            0, 0, 0, 255, 0, 1, 2, 3, 4,
        ];
        assert_eq!(raw, expected);
    }

    #[test]
    fn kitty_chunks_large_payloads() {
        let mut out = Vec::new();
        let mut b = Vec::new();
        // 7000 zlib bytes → 9336 base64 bytes → 3 chunks.
        let payload = vec![0xAB; 7000];
        kitty_transmit(&mut out, 42, 10, 20, &payload, -1, &mut b);
        let s = String::from_utf8(out).unwrap();
        let parts: Vec<&str> = s.split("\x1b\\").filter(|p| !p.is_empty()).collect();
        assert_eq!(parts.len(), 3);
        assert!(parts[0].starts_with("\x1b_Ga=T,f=32,o=z,q=2,C=1,i=42,p=1,s=10,v=20,z=-1,m=1;"));
        assert!(parts[1].starts_with("\x1b_Gm=1;"));
        assert!(parts[2].starts_with("\x1b_Gm=0;"));
        let first_payload = parts[0].split_once(';').unwrap().1;
        assert_eq!(first_payload.len(), KITTY_CHUNK);
        let total: usize = parts
            .iter()
            .map(|p| p.split_once(';').unwrap().1.len())
            .sum();
        assert_eq!(total, 7000usize.div_ceil(3) * 4);
    }

    #[test]
    fn kitty_single_chunk_and_delete() {
        let mut out = Vec::new();
        kitty_transmit(&mut out, 7, 1, 1, b"abc", 0, &mut Vec::new());
        assert_eq!(
            String::from_utf8(out).unwrap(),
            "\x1b_Ga=T,f=32,o=z,q=2,C=1,i=7,p=1,s=1,v=1,z=0,m=0;YWJj\x1b\\"
        );
        let mut out = Vec::new();
        kitty_delete(&mut out, 7);
        assert_eq!(out, b"\x1b_Ga=d,d=I,i=7,q=2\x1b\\");
    }

    #[test]
    fn iterm2_escape_format() {
        let mut out = Vec::new();
        iterm2_image(&mut out, b"PNGDATA", 3, 2, &mut Vec::new());
        let s = String::from_utf8(out).unwrap();
        assert!(s.starts_with("\x1b]1337;File=inline=1;size=7;width=3;height=2;"));
        assert!(s.ends_with(":UE5HREFUQQ==\x07"));
    }

    #[test]
    fn sixel_output_shape() {
        // 4x8: top 6 rows red, bottom 2 rows blue, one transparent pixel.
        let mut px = vec![[255, 0, 0, 255]; 4 * 6];
        px.extend(vec![[0, 0, 255, 255]; 4 * 2]);
        px[0] = [0, 0, 0, 0];
        let mut out = Vec::new();
        sixel(&mut out, 4, 8, &px, &mut SixelScratch::default());
        let s = String::from_utf8(out).unwrap();
        assert!(s.starts_with("\x1bP0;1;0q\"1;1;4;8"));
        assert!(s.ends_with("\x1b\\"));
        assert!(s.contains("#0;2;100;0;0"));
        assert!(s.contains("#1;2;0;0;100"));
        // Two bands → exactly one band separator.
        assert_eq!(s.matches('-').count(), 1);
        // Band 1, colour 0: first column misses the top pixel (0b111110 → 63+62).
        assert!(s.contains(&format!(
            "#0{}{}{}{}$",
            (63 + 62) as u8 as char,
            '~',
            '~',
            '~'
        )));
        // Band 2, colour 1: two rows set → 63 + 3 = 'B', run of 4.
        assert!(s.contains("#1!4B$"));
    }

    #[test]
    fn sixel_quantises_beyond_256_colours() {
        let px: Vec<Rgba> = (0..600u32)
            .map(|i| [(i % 256) as u8, (i / 3) as u8, 7, 255])
            .collect();
        let mut out = Vec::new();
        let mut s = SixelScratch::default();
        sixel(&mut out, 600, 1, &px, &mut s);
        assert!(s.palette.len() <= 252);
        assert!(!out.is_empty());
    }
}
