//! Skin preview renderer used by `.github/workflows/skin-preview.yml`:
//! composites the main, equalizer and playlist windows of every `.wsz` in
//! `$SKIN_PREVIEW_DIR` with a fixed sample `PlayerView` and writes PNGs to
//! `$SKIN_PREVIEW_OUT`. Run with `cargo test skin_previews -- --ignored`.

use std::time::Duration;

use crate::gfx::encode::{png, Deflater};
use crate::skin::{Rgba, Sheet, WinampSkin};
use crate::view::PlayStatus;

use super::canvas::Canvas;
use super::compose::{self, RowSpan};
use super::sprites as sp;
use super::tests::sample_view;
use super::{WinampUiState, Window};

/// Draw `text` with TEXT.BMP glyph shapes recoloured to `color` (the
/// terminal draws real playlist text; the preview approximates it).
fn draw_tinted(
    c: &mut Canvas,
    skin: &WinampSkin,
    text: &str,
    x: i32,
    y: i32,
    max_x: i32,
    color: Rgba,
) {
    let Some(font) = skin.sheet(Sheet::Text) else {
        return;
    };
    let bg = font.get(152, 2).unwrap_or([0, 0, 0, 255]);
    let mut cx = x;
    for ch in text.chars() {
        if cx + sp::CHAR_W as i32 > max_x {
            break;
        }
        let (row, col) = sp::glyph_cell(ch);
        for gy in 0..sp::CHAR_H {
            for gx in 0..sp::CHAR_W {
                let p = font.get(col * sp::CHAR_W + gx, row * sp::CHAR_H + gy);
                if p.is_some_and(|p| p != bg) {
                    c.set(cx + gx as i32, y + gy as i32, color);
                }
            }
        }
        cx += sp::CHAR_W as i32;
    }
}

fn render_skin(skin: &WinampSkin) -> Canvas {
    let view = sample_view(PlayStatus::Playing);
    let mut ui = WinampUiState::new();
    // Two frames so the analyzer has risen and peaks exist.
    ui.compose_main(skin, &view, Duration::ZERO);
    ui.compose_main(skin, &view, Duration::from_millis(1_100));

    ui.compose_eq(skin, &view);

    let h = compose::playlist_height(4);
    let area = compose::playlist_track_area(h);
    ui.pl_rows.clear();
    let mut y = area.y + 3;
    let mut i = 0;
    while y + sp::PL_TRACK_H as i32 <= area.y + area.h as i32 {
        ui.pl_rows.push(RowSpan {
            y0: y,
            y1: y + sp::PL_TRACK_H as i32,
            track: (i < view.playlist.len()).then_some(i),
        });
        y += sp::PL_TRACK_H as i32;
        i += 1;
    }
    let visible = ui.pl_rows.len();
    ui.sync_playlist_scroll(visible, &view);
    ui.compose_playlist(skin, &view, h);

    let mut out = Canvas::new();
    out.resize(sp::WINDOW_W, sp::WINDOW_H * 2 + h);
    out.fill([0, 0, 0, 255]);
    for (window, dy) in [
        (Window::Main, 0),
        (Window::Equalizer, sp::WINDOW_H as i32),
        (Window::Playlist, sp::WINDOW_H as i32 * 2),
    ] {
        let c = ui.canvas(window);
        for yy in 0..c.height() {
            for xx in 0..c.width() {
                if let Some(p) = c.get(xx, yy) {
                    out.set(xx as i32, dy + yy as i32, p);
                }
            }
        }
    }
    // Playlist text, as the terminal would draw it.
    let dy = sp::WINDOW_H as i32 * 2;
    let mut label = String::new();
    for row in &ui.pl_rows {
        let Some(idx) = row.track else { continue };
        let item = &view.playlist[idx];
        let (fg, _) = compose::row_colors(skin, &view, idx);
        label.clear();
        compose::write_time(&mut label, item.duration.unwrap_or(0));
        let dur_x = area.x + area.w as i32 - 3 - label.chars().count() as i32 * sp::CHAR_W as i32;
        draw_tinted(
            &mut out,
            skin,
            &label,
            dur_x,
            dy + row.y0 + 4,
            area.x + area.w as i32,
            fg,
        );
        label.clear();
        label.push_str(&format!("{}. ", idx + 1));
        if let Some(a) = &item.artist {
            label.push_str(a);
            label.push_str(" - ");
        }
        label.push_str(&item.title);
        draw_tinted(
            &mut out,
            skin,
            &label,
            area.x + 2,
            dy + row.y0 + 4,
            dur_x - 4,
            fg,
        );
    }
    out
}

fn write_png(path: &std::path::Path, c: &Canvas, scale: u32) -> std::io::Result<()> {
    let (w, h) = (c.width() * scale, c.height() * scale);
    let mut px_buf = Vec::with_capacity((w * h) as usize);
    for y in 0..h {
        for x in 0..w {
            px_buf.push(c.get(x / scale, y / scale).unwrap_or([0, 0, 0, 255]));
        }
    }
    let mut bytes = Vec::new();
    png(
        w,
        h,
        &px_buf,
        &mut Deflater::default(),
        &mut Vec::new(),
        &mut bytes,
    );
    std::fs::write(path, bytes)
}

#[test]
#[ignore = "needs downloaded skins; run by the skin-preview workflow"]
fn skin_previews() {
    let dir = std::env::var("SKIN_PREVIEW_DIR").unwrap_or_else(|_| "skin-preview/skins".into());
    let out = std::env::var("SKIN_PREVIEW_OUT").unwrap_or_else(|_| "skin-preview/out".into());
    std::fs::create_dir_all(&out).unwrap();
    let mut entries: Vec<_> = std::fs::read_dir(&dir)
        .unwrap_or_else(|e| panic!("cannot read {dir}: {e}"))
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().is_some_and(|e| e.eq_ignore_ascii_case("wsz")))
        .collect();
    entries.sort();
    assert!(!entries.is_empty(), "no .wsz files in {dir}");
    for path in entries {
        let skin =
            WinampSkin::from_wsz(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
        let missing: Vec<&str> = Sheet::ALL
            .iter()
            .filter(|s| skin.sheet(**s).is_none())
            .map(|s| s.file_name())
            .collect();
        println!("{} — {:?}; missing: {missing:?}", path.display(), skin.name);
        let canvas = render_skin(&skin);
        let stem = path.file_stem().and_then(|s| s.to_str()).unwrap_or("skin");
        let name: String = stem
            .chars()
            .map(|c| {
                if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                    c
                } else {
                    '_'
                }
            })
            .collect();
        let file = std::path::Path::new(&out).join(format!("{name}.png"));
        write_png(&file, &canvas, 2).unwrap();
        // Sanity: the main window must not be blank.
        let first = canvas.get(0, 0);
        assert!(
            (0..sp::WINDOW_W).any(|x| canvas.get(x, 50) != first),
            "{} rendered a flat main window",
            path.display()
        );
    }
}

#[test]
fn preview_renderer_handles_synthetic_skin() {
    use crate::skin::tests::{build_wsz, solid_bmp};
    let wsz = build_wsz(&[
        ("MAIN.BMP", solid_bmp(275, 116, [40, 40, 60])),
        ("TEXT.BMP", solid_bmp(155, 18, [1, 2, 3])),
    ]);
    let skin = WinampSkin::from_wsz_bytes(&wsz, "synthetic").unwrap();
    let c = render_skin(&skin);
    assert_eq!(c.width(), 275);
    assert_eq!(c.height(), 116 * 2 + compose::playlist_height(4));
    assert_eq!(c.get(150, 60), Some([40, 40, 60, 255]));
}
