//! Small BMP decoder covering every variant found in Winamp 2.x skins:
//! 1/4/8/16/24/32 bpp, `BI_RGB`, `BI_RLE8`, `BI_RLE4`, `BI_BITFIELDS` /
//! `BI_ALPHABITFIELDS`, bottom-up and top-down rows, and OS/2
//! `BITMAPCOREHEADER` files. Output is always top-down, opaque RGBA.
//!
//! The decoder is lenient the way Winamp is: truncated pixel data leaves the
//! remaining pixels at palette entry 0 instead of failing, because a lot of
//! real-world skins ship slightly short files.

use std::fmt;

/// One RGBA pixel.
pub type Rgba = [u8; 4];

/// Opaque black.
pub const BLACK: Rgba = [0, 0, 0, 255];

/// Decoded image: row-major, top-down RGBA.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Bitmap {
    pub width: u32,
    pub height: u32,
    pub pixels: Vec<Rgba>,
}

impl Bitmap {
    pub fn new(width: u32, height: u32, fill: Rgba) -> Self {
        Self {
            width,
            height,
            pixels: vec![fill; width as usize * height as usize],
        }
    }

    /// Pixel at (x, y), or `None` outside the image.
    pub fn get(&self, x: u32, y: u32) -> Option<Rgba> {
        if x >= self.width || y >= self.height {
            return None;
        }
        self.pixels
            .get(y as usize * self.width as usize + x as usize)
            .copied()
    }
}

/// A decoded BMP plus its colour table (empty for true-colour images).
#[derive(Debug, Clone)]
pub struct Decoded {
    pub bitmap: Bitmap,
    pub palette: Vec<Rgba>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BmpError {
    NotBmp,
    Truncated,
    TooLarge,
    Unsupported(&'static str),
}

impl fmt::Display for BmpError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NotBmp => f.write_str("not a BMP file"),
            Self::Truncated => f.write_str("truncated BMP header"),
            Self::TooLarge => f.write_str("BMP dimensions too large"),
            Self::Unsupported(what) => write!(f, "unsupported BMP: {what}"),
        }
    }
}

impl std::error::Error for BmpError {}

const BI_RGB: u32 = 0;
const BI_RLE8: u32 = 1;
const BI_RLE4: u32 = 2;
const BI_BITFIELDS: u32 = 3;
const BI_ALPHABITFIELDS: u32 = 6;

/// Skins are tiny; anything bigger is corrupt or hostile.
const MAX_DIM: u32 = 4096;

fn u16le(d: &[u8], off: usize) -> Result<u16, BmpError> {
    d.get(off..off + 2)
        .map(|b| u16::from_le_bytes([b[0], b[1]]))
        .ok_or(BmpError::Truncated)
}

fn u32le(d: &[u8], off: usize) -> Result<u32, BmpError> {
    d.get(off..off + 4)
        .map(|b| u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
        .ok_or(BmpError::Truncated)
}

/// Channel masks for 16/32 bpp images.
#[derive(Debug, Clone, Copy)]
struct Masks {
    r: u32,
    g: u32,
    b: u32,
}

/// Extract a masked channel and scale it to 8 bits.
fn channel(value: u32, mask: u32) -> u8 {
    if mask == 0 {
        return 0;
    }
    let shift = mask.trailing_zeros();
    let bits = (mask >> shift).count_ones();
    let v = (value & mask) >> shift;
    if bits >= 8 {
        (v >> (bits - 8)) as u8
    } else {
        let max = (1u32 << bits) - 1;
        ((v * 255 + max / 2) / max) as u8
    }
}

/// Decode a BMP file into RGBA.
pub fn decode(data: &[u8]) -> Result<Decoded, BmpError> {
    if data.len() < 2 || &data[0..2] != b"BM" {
        return Err(BmpError::NotBmp);
    }
    let pixel_offset = u32le(data, 10)? as usize;
    let header_size = u32le(data, 14)? as usize;

    let (width, height_raw, bpp, compression, colors_used, entry_size) = if header_size == 12 {
        (
            i32::from(u16le(data, 18)?),
            i32::from(u16le(data, 20)? as i16),
            u16le(data, 24)?,
            BI_RGB,
            0u32,
            3usize,
        )
    } else if header_size >= 40 {
        (
            u32le(data, 18)? as i32,
            u32le(data, 22)? as i32,
            u16le(data, 28)?,
            u32le(data, 30)?,
            u32le(data, 46)?,
            4usize,
        )
    } else {
        return Err(BmpError::Unsupported("header size"));
    };

    if width <= 0 || height_raw == 0 {
        return Err(BmpError::Unsupported("dimensions"));
    }
    let top_down = height_raw < 0;
    let width = width as u32;
    let height = height_raw.unsigned_abs();
    if width > MAX_DIM || height > MAX_DIM {
        return Err(BmpError::TooLarge);
    }

    // Bitfield masks live inside V2+ headers, or right after a 40-byte header.
    let mut extra_after_header = 0usize;
    let masks = match compression {
        BI_BITFIELDS | BI_ALPHABITFIELDS => {
            if header_size == 40 {
                extra_after_header = if compression == BI_ALPHABITFIELDS {
                    16
                } else {
                    12
                };
            }
            Masks {
                r: u32le(data, 54)?,
                g: u32le(data, 58)?,
                b: u32le(data, 62)?,
            }
        }
        _ if bpp == 16 => Masks {
            r: 0x7C00,
            g: 0x03E0,
            b: 0x001F,
        },
        _ => Masks {
            r: 0x00FF_0000,
            g: 0x0000_FF00,
            b: 0x0000_00FF,
        },
    };

    let mut palette = Vec::new();
    if bpp <= 8 {
        let max_entries = 1usize << bpp;
        let n = if colors_used > 0 {
            (colors_used as usize).min(256)
        } else {
            max_entries
        };
        let start = 14 + header_size + extra_after_header;
        // The colour table never overlaps the pixel array.
        let end = if pixel_offset > start {
            pixel_offset.min(data.len())
        } else {
            data.len()
        };
        palette.reserve_exact(n);
        for i in 0..n {
            let off = start + i * entry_size;
            match data.get(off..off + 3) {
                Some(e) if off + 3 <= end => palette.push([e[2], e[1], e[0], 255]),
                _ => break,
            }
        }
    }

    let pixels = data.get(pixel_offset..).unwrap_or(&[]);
    let base = palette.first().copied().unwrap_or(BLACK);
    let mut out = Bitmap::new(width, height, base);
    let w = width as usize;
    let h = height as usize;

    match (bpp, compression) {
        (1 | 4 | 8, BI_RGB) => {
            let row_bytes = (w * bpp as usize).div_ceil(32) * 4;
            let per_byte = 8 / bpp as usize;
            let mask = ((1u16 << bpp) - 1) as u8;
            for y in 0..h {
                let src_row = if top_down { y } else { h - 1 - y };
                let Some(row) = pixels.get(src_row * row_bytes..(src_row + 1) * row_bytes) else {
                    break;
                };
                let dst = &mut out.pixels[y * w..(y + 1) * w];
                for (x, px) in dst.iter_mut().enumerate() {
                    let byte = row[x / per_byte];
                    let shift = 8 - bpp as usize * (x % per_byte + 1);
                    let idx = (byte >> shift) & mask;
                    *px = palette.get(idx as usize).copied().unwrap_or(BLACK);
                }
            }
        }
        (8, BI_RLE8) => decode_rle(pixels, &palette, &mut out, top_down, false),
        (4, BI_RLE4) => decode_rle(pixels, &palette, &mut out, top_down, true),
        (24, BI_RGB) => {
            let row_bytes = (w * 3).div_ceil(4) * 4;
            for y in 0..h {
                let src_row = if top_down { y } else { h - 1 - y };
                let Some(row) = pixels.get(src_row * row_bytes..(src_row + 1) * row_bytes) else {
                    break;
                };
                let dst = &mut out.pixels[y * w..(y + 1) * w];
                for (px, bgr) in dst.iter_mut().zip(row.as_chunks::<3>().0) {
                    *px = [bgr[2], bgr[1], bgr[0], 255];
                }
            }
        }
        (16 | 32, BI_RGB | BI_BITFIELDS | BI_ALPHABITFIELDS) => {
            let bytes = bpp as usize / 8;
            let row_bytes = (w * bytes).div_ceil(4) * 4;
            for y in 0..h {
                let src_row = if top_down { y } else { h - 1 - y };
                let Some(row) = pixels.get(src_row * row_bytes..(src_row + 1) * row_bytes) else {
                    break;
                };
                let dst = &mut out.pixels[y * w..(y + 1) * w];
                for (px, raw) in dst.iter_mut().zip(row.chunks_exact(bytes)) {
                    let v = if bytes == 2 {
                        u32::from(u16::from_le_bytes([raw[0], raw[1]]))
                    } else {
                        u32::from_le_bytes([raw[0], raw[1], raw[2], raw[3]])
                    };
                    // Winamp ignores alpha; skins are opaque.
                    *px = [
                        channel(v, masks.r),
                        channel(v, masks.g),
                        channel(v, masks.b),
                        255,
                    ];
                }
            }
        }
        _ => return Err(BmpError::Unsupported("bit depth / compression")),
    }

    Ok(Decoded {
        bitmap: out,
        palette,
    })
}

/// RLE8 / RLE4 decoder. Rows are counted from the bottom unless `top_down`.
fn decode_rle(data: &[u8], palette: &[Rgba], out: &mut Bitmap, top_down: bool, four_bit: bool) {
    let w = out.width as usize;
    let h = out.height as usize;
    let (mut x, mut y, mut i) = (0usize, 0usize, 0usize);
    let mut put = |x: usize, y: usize, idx: u8| {
        if x < w && y < h {
            let row = if top_down { y } else { h - 1 - y };
            out.pixels[row * w + x] = palette.get(idx as usize).copied().unwrap_or(BLACK);
        }
    };
    while i + 1 < data.len() && y < h {
        let (count, value) = (data[i] as usize, data[i + 1]);
        i += 2;
        if count > 0 {
            for k in 0..count {
                let idx = if four_bit {
                    if k % 2 == 0 {
                        value >> 4
                    } else {
                        value & 0x0F
                    }
                } else {
                    value
                };
                put(x, y, idx);
                x += 1;
            }
            continue;
        }
        match value {
            0 => {
                x = 0;
                y += 1;
            }
            1 => break,
            2 => {
                let (Some(&dx), Some(&dy)) = (data.get(i), data.get(i + 1)) else {
                    break;
                };
                x += dx as usize;
                y += dy as usize;
                i += 2;
            }
            n => {
                let n = n as usize;
                let bytes = if four_bit { n.div_ceil(2) } else { n };
                for k in 0..n {
                    let idx = if four_bit {
                        let Some(&b) = data.get(i + k / 2) else { break };
                        if k % 2 == 0 {
                            b >> 4
                        } else {
                            b & 0x0F
                        }
                    } else {
                        let Some(&b) = data.get(i + k) else { break };
                        b
                    };
                    put(x, y, idx);
                    x += 1;
                }
                // Absolute runs are padded to a 16-bit boundary.
                i += bytes + (bytes & 1);
            }
        }
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// Test-only BMP writer: builds files for every layout the decoder supports.
    pub(crate) struct BmpSpec<'a> {
        pub width: i32,
        pub height: i32,
        pub bpp: u16,
        pub compression: u32,
        pub header_size: u32,
        pub palette: &'a [[u8; 3]],
        pub masks: Option<[u32; 3]>,
        /// Raw pixel array exactly as stored in the file.
        pub pixel_data: Vec<u8>,
    }

    pub(crate) fn build_bmp(spec: &BmpSpec<'_>) -> Vec<u8> {
        let core = spec.header_size == 12;
        let entry = if core { 3 } else { 4 };
        let masks_len = if spec.header_size == 40 && spec.masks.is_some() {
            12
        } else {
            0
        };
        let pal_len = spec.palette.len() * entry;
        let offset = 14 + spec.header_size as usize + masks_len + pal_len;
        let mut d = Vec::new();
        d.extend_from_slice(b"BM");
        d.extend_from_slice(&((offset + spec.pixel_data.len()) as u32).to_le_bytes());
        d.extend_from_slice(&[0; 4]);
        d.extend_from_slice(&(offset as u32).to_le_bytes());
        d.extend_from_slice(&spec.header_size.to_le_bytes());
        if core {
            d.extend_from_slice(&(spec.width as u16).to_le_bytes());
            d.extend_from_slice(&(spec.height as i16).to_le_bytes());
            d.extend_from_slice(&1u16.to_le_bytes());
            d.extend_from_slice(&spec.bpp.to_le_bytes());
        } else {
            d.extend_from_slice(&spec.width.to_le_bytes());
            d.extend_from_slice(&spec.height.to_le_bytes());
            d.extend_from_slice(&1u16.to_le_bytes());
            d.extend_from_slice(&spec.bpp.to_le_bytes());
            d.extend_from_slice(&spec.compression.to_le_bytes());
            d.extend_from_slice(&(spec.pixel_data.len() as u32).to_le_bytes());
            d.extend_from_slice(&[0; 8]); // resolution
            d.extend_from_slice(&(spec.palette.len() as u32).to_le_bytes());
            d.extend_from_slice(&[0; 4]); // important colours
            let mut written = 40;
            if spec.header_size > 40 {
                let m = spec.masks.unwrap_or([0; 3]);
                for v in m {
                    d.extend_from_slice(&v.to_le_bytes());
                }
                written += 12;
            }
            d.resize(d.len() + spec.header_size as usize - written, 0);
            if spec.header_size == 40 {
                if let Some(m) = spec.masks {
                    for v in m {
                        d.extend_from_slice(&v.to_le_bytes());
                    }
                }
            }
        }
        for c in spec.palette {
            d.extend_from_slice(&[c[2], c[1], c[0]]);
            if !core {
                d.push(0);
            }
        }
        d.extend_from_slice(&spec.pixel_data);
        d
    }

    const PAL: [[u8; 3]; 4] = [[0, 0, 0], [255, 0, 0], [0, 255, 0], [0, 0, 255]];

    fn rgba(c: [u8; 3]) -> Rgba {
        [c[0], c[1], c[2], 255]
    }

    fn spec(
        bpp: u16,
        compression: u32,
        width: i32,
        height: i32,
        data: Vec<u8>,
    ) -> BmpSpec<'static> {
        BmpSpec {
            width,
            height,
            bpp,
            compression,
            header_size: 40,
            palette: &PAL,
            masks: None,
            pixel_data: data,
        }
    }

    #[test]
    fn decodes_1bpp_bottom_up_with_row_padding() {
        // 3x2; bottom row stored first. Rows padded to 4 bytes.
        let data = vec![0b1010_0000, 0, 0, 0, 0b0110_0000, 0, 0, 0];
        let d = decode(&build_bmp(&spec(1, BI_RGB, 3, 2, data))).unwrap();
        let p = &d.bitmap.pixels;
        // top row = second stored row = 0,1,1
        assert_eq!(p[0], rgba(PAL[0]));
        assert_eq!(p[1], rgba(PAL[1]));
        assert_eq!(p[2], rgba(PAL[1]));
        // bottom row = 1,0,1
        assert_eq!(p[3], rgba(PAL[1]));
        assert_eq!(p[4], rgba(PAL[0]));
        assert_eq!(p[5], rgba(PAL[1]));
    }

    #[test]
    fn decodes_4bpp_top_down() {
        let data = vec![0x12, 0x30, 0, 0, 0x32, 0x10, 0, 0];
        let d = decode(&build_bmp(&spec(4, BI_RGB, 3, -2, data))).unwrap();
        let p = &d.bitmap.pixels;
        assert_eq!(&p[0..3], &[rgba(PAL[1]), rgba(PAL[2]), rgba(PAL[3])]);
        assert_eq!(&p[3..6], &[rgba(PAL[3]), rgba(PAL[2]), rgba(PAL[1])]);
    }

    #[test]
    fn decodes_8bpp_and_out_of_range_index_is_black() {
        let data = vec![1, 2, 200, 0, 3, 0, 1, 0];
        let d = decode(&build_bmp(&spec(8, BI_RGB, 3, 2, data))).unwrap();
        let p = &d.bitmap.pixels;
        assert_eq!(&p[0..3], &[rgba(PAL[3]), rgba(PAL[0]), rgba(PAL[1])]);
        assert_eq!(&p[3..6], &[rgba(PAL[1]), rgba(PAL[2]), BLACK]);
        assert_eq!(d.palette.len(), 4);
    }

    #[test]
    fn decodes_rle8_runs_absolute_delta_and_eol() {
        // 4x3 bottom-up.
        let data = vec![
            4, 1, // row0 (bottom): 4 x red
            0, 0, // EOL
            0, 3, 2, 3, 2, 0, // absolute: green blue green (+pad)
            0, 0, // EOL
            0, 2, 1, 0, // delta: x+1
            2, 3, // two blue at x=1,2
            0, 1, // EOB
        ];
        let d = decode(&build_bmp(&spec(8, BI_RLE8, 4, 3, data))).unwrap();
        let p = &d.bitmap.pixels;
        // top row (y=2 from bottom): idx0, blue, blue, idx0
        assert_eq!(
            &p[0..4],
            &[rgba(PAL[0]), rgba(PAL[3]), rgba(PAL[3]), rgba(PAL[0])]
        );
        // middle row: green blue green, then untouched (palette 0)
        assert_eq!(
            &p[4..8],
            &[rgba(PAL[2]), rgba(PAL[3]), rgba(PAL[2]), rgba(PAL[0])]
        );
        assert_eq!(&p[8..12], &[rgba(PAL[1]); 4]);
    }

    #[test]
    fn decodes_rle4_alternating_and_absolute() {
        // 4x2 bottom-up.
        let data = vec![
            4, 0x12, // row0: 1 2 1 2
            0, 0, // EOL
            0, 3, 0x32, 0x10, // absolute 3 nibbles: 3 2 1 (2 bytes, already even)
            0, 1,
        ];
        let d = decode(&build_bmp(&spec(4, BI_RLE4, 4, 2, data))).unwrap();
        let p = &d.bitmap.pixels;
        assert_eq!(
            &p[0..4],
            &[rgba(PAL[3]), rgba(PAL[2]), rgba(PAL[1]), rgba(PAL[0])]
        );
        assert_eq!(
            &p[4..8],
            &[rgba(PAL[1]), rgba(PAL[2]), rgba(PAL[1]), rgba(PAL[2])]
        );
    }

    #[test]
    fn decodes_24bpp_bgr_with_padding() {
        // 1x2 bottom-up: rows are 3 bytes + 1 pad.
        let data = vec![10, 20, 30, 0, 40, 50, 60, 0];
        let mut s = spec(24, BI_RGB, 1, 2, data);
        s.palette = &[];
        let d = decode(&build_bmp(&s)).unwrap();
        assert_eq!(d.bitmap.pixels, vec![[60, 50, 40, 255], [30, 20, 10, 255]]);
        assert!(d.palette.is_empty());
    }

    #[test]
    fn decodes_32bpp_rgb_ignoring_alpha() {
        let data = vec![1, 2, 3, 0, 4, 5, 6, 0x80];
        let mut s = spec(32, BI_RGB, 2, -1, data);
        s.palette = &[];
        let d = decode(&build_bmp(&s)).unwrap();
        assert_eq!(d.bitmap.pixels, vec![[3, 2, 1, 255], [6, 5, 4, 255]]);
    }

    #[test]
    fn decodes_16bpp_bitfields_565_after_40_byte_header() {
        // 565: pure red, pure green, pure blue, white.
        let px: [u16; 4] = [0xF800, 0x07E0, 0x001F, 0xFFFF];
        let mut data = Vec::new();
        for v in px {
            data.extend_from_slice(&v.to_le_bytes());
        }
        let mut s = spec(16, BI_BITFIELDS, 4, -1, data);
        s.palette = &[];
        s.masks = Some([0xF800, 0x07E0, 0x001F]);
        let d = decode(&build_bmp(&s)).unwrap();
        assert_eq!(
            d.bitmap.pixels,
            vec![
                [255, 0, 0, 255],
                [0, 255, 0, 255],
                [0, 0, 255, 255],
                [255, 255, 255, 255]
            ]
        );
    }

    #[test]
    fn decodes_32bpp_bitfields_in_v4_header() {
        let mut data = Vec::new();
        data.extend_from_slice(&0x00AA_BBCCu32.to_le_bytes());
        let mut s = spec(32, BI_BITFIELDS, 1, 1, data);
        s.palette = &[];
        s.header_size = 108;
        // Swapped layout: R in low byte, B in high byte.
        s.masks = Some([0x0000_00FF, 0x0000_FF00, 0x00FF_0000]);
        let d = decode(&build_bmp(&s)).unwrap();
        assert_eq!(d.bitmap.pixels, vec![[0xCC, 0xBB, 0xAA, 255]]);
    }

    #[test]
    fn decodes_16bpp_default_555() {
        let data = 0x7C00u16.to_le_bytes().to_vec(); // red in 555
        let mut s = spec(16, BI_RGB, 1, 1, [data, vec![0, 0]].concat());
        s.palette = &[];
        let d = decode(&build_bmp(&s)).unwrap();
        assert_eq!(d.bitmap.pixels, vec![[255, 0, 0, 255]]);
    }

    #[test]
    fn decodes_os2_core_header_with_rgbtriple_palette() {
        let mut s = spec(8, BI_RGB, 2, 1, vec![2, 3, 0, 0]);
        s.header_size = 12;
        let d = decode(&build_bmp(&s)).unwrap();
        assert_eq!(d.bitmap.pixels, vec![rgba(PAL[2]), rgba(PAL[3])]);
    }

    #[test]
    fn truncated_pixel_data_is_lenient() {
        // Only the bottom row is present.
        let d = decode(&build_bmp(&spec(8, BI_RGB, 2, 2, vec![1, 1, 0, 0]))).unwrap();
        assert_eq!(d.bitmap.pixels[2], rgba(PAL[1]));
        assert_eq!(d.bitmap.pixels[0], rgba(PAL[0]));
    }

    #[test]
    fn rejects_garbage_and_huge_images() {
        assert_eq!(decode(b"PK\x03\x04").unwrap_err(), BmpError::NotBmp);
        assert_eq!(decode(b"BM").unwrap_err(), BmpError::Truncated);
        let s = spec(8, BI_RGB, 100_000, 1, vec![]);
        assert_eq!(decode(&build_bmp(&s)).unwrap_err(), BmpError::TooLarge);
        let s = spec(2, BI_RGB, 1, 1, vec![0; 4]);
        assert!(matches!(
            decode(&build_bmp(&s)).unwrap_err(),
            BmpError::Unsupported(_)
        ));
    }
}
