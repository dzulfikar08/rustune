# Bitmap Skin Layout Design

## Summary

Redesign the Winamp renderer to use actual BMP pixel data from `.wsz` skin files as the visual layout, overlaying dynamic content (track title, elapsed time, playlist, etc.) on top. When all required BMPs are present, the player renders genuine skin artwork; otherwise it falls back to the current styled-text renderer.

## Context

Rustune currently extracts **palette colors** from Winamp skin BMPs and uses them to style text/unicode elements. The `skin_bitmap.rs` module can render BMP pixels as terminal graphics using upper-half block characters, but it's only used for the MAIN.BMP background. The goal is to make the layout itself driven by the skin's bitmap resources — so each skin looks visually distinct, not just color-swapped.

## Decision: Layered Bitmap Renderer

**Approach:** Each Winamp BMP gets its own fixed-size zone in the terminal layout. The renderer paints bitmap backgrounds first, then overlays dynamic text on top at the correct pixel positions (matching real Winamp 2.x coordinates).

**Why this over alternatives:**
- Most faithful to real Winamp — each skin's visual design is fully honored
- Well-documented Winamp 2.x pixel coordinates available
- Fixed layout is appropriate for a replica UI
- Tiled or full-frame approaches either distort or break the skin's look

## Skin Layout Zones

The Winamp 2.x main window (MAIN.BMP: 275x116) has well-documented regions:

```
+------------------- 275px -------------------+
| TITLEBAR (0,0 -> 275,20)                   | row 0
+---------------------------------------------+
| CLUTTERBAR  LED_TIME       SPECTRUM_VIS     |
| (9,6,16,16) (9,26->71,38) (78,22->275,50)  | row 1
|                                             |
| MARQUEE (9,53 -> 266,65)                    | row 2
| SEEKBAR (16,72 -> 260,78)                   | row 3
| CBUTTONS (0,57->136,93) + VOL + BAL        | row 4
| STATUS (0,93 -> 275,116)                    | row 5
+---------------------------------------------+
```

Each zone maps to:
- **Source rectangle** in the BMP (x, y, width, height in pixels)
- **Destination** in the terminal (which row(s), which columns)
- **Overlay content** (dynamic text rendered on top with skin colors)

Additional BMPs rendered in their respective zones:
- **NUMBERS.BMP** (99x13) — LED digit glyphs for time display
- **CBUTTONS.BMP** (136x36) — transport button graphics
- **POSBAR.BMP** (307x10) — seek bar track + thumb
- **TEXT.BMP** (155x74) — songticker marquee font
- **PLAYPAUS.BMP** (42x9) — play/pause indicator
- **MONOSTER.BMP** (58x24) — mono/stereo indicator
- **SHUFREP.BMP** (62x38) — shuffle/repeat buttons
- **VOLUME.BMP** (68x433) — volume slider

## Rendering Architecture

### New Module: `src/ui/skin_layout.rs`

```
SkinLayout (struct)
+-- zones: HashMap<ZoneKind, SkinZone>
|   Each zone has: src_rect (x,y,w,h in BMP pixels), dest_rows (terminal rows)
|
+-- Zone variants:
|   TitleBar, LedTime, Spectrum, Marquee, SeekBar,
|   Transport, Volume, Balance, Status, Shuffle, Repeat,
|   MonoStereo, EqButton, PlButton
|
+-- from_skin(skin: &WinampSkin) -> Option<SkinLayout>
|   Returns None if any required BMP is missing (triggers text fallback)
|
+-- render_zone(frame, area, zone, bmp, overlay)
    Renders the BMP sub-rectangle into the terminal area,
    then paints overlay text on top
```

### Rendering Flow

1. `render()` checks `SkinLayout::from_skin(skin)` — if `None`, use current text renderer
2. If layout exists, split terminal into fixed-size main window + flexible playlist + footer
3. For each zone, call `render_zone()`:
   - Extract the sub-rectangle from the source BMP
   - Scale to the terminal area using `skin_bitmap::render_scaled_bitmap()`
   - Overlay dynamic content (time digits, track title, etc.) with skin colors
4. Playlist and footer stay as styled text (variable-length content)

### Key Addition to `skin_bitmap.rs`

Add `render_bitmap_region()` that crops a sub-rectangle from a BMP before rendering, instead of rendering the entire image.

## Dynamic Overlays

Each bitmap zone has specific areas where Winamp renders dynamic content. We overlay styled text using the skin's own colors.

| Zone | Overlay Content | Colors Source | Position |
|------|----------------|---------------|----------|
| TitleBar | Skin name, window controls | titlebar colors from MAIN.BMP palette | centered in titlebar region |
| LedTime | Elapsed time digits (e.g. "12:34") | `led_on` from NUMBERS.BMP | over the LED region |
| Marquee | Scrolling track title | `text_fg`/`text_bg` from TEXT.BMP | songticker rectangle |
| SeekBar | Progress position | `seek_filled`/`seek_track` from POSBAR.BMP | seekbar track area |
| Transport | Active button highlight (play/pause state) | `play_indicator`/`pause_indicator` | over the active button |
| Volume | Volume level fill | `led_on` for filled, `led_off` for empty | volume slider region |
| Status | Mono/stereo, bitrate, source label | `indicator_on`/`indicator_off` | status row positions |

The bitmap provides **chrome and decoration** (borders, gradients, background patterns), while text overlays provide **readable dynamic content**.

No change to: playlist body, input bar, hints bar — these remain styled text with skin colors.

## Required BMPs (All-or-Nothing)

Bitmap mode activates only when **all** of these are present:
- `MAIN.BMP`, `NUMBERS.BMP`, `CBUTTONS.BMP`, `POSBAR.BMP`, `TEXT.BMP`, `PLAYPAUS.BMP`, `TITLEBAR.BMP`

If any is missing, the entire renderer falls back to the current styled-text mode.

Optional BMPs that enhance the display when present:
- `MONOSTER.BMP`, `SHUFREP.BMP`, `VOLUME.BMP`

## Changes to `src/skin.rs`

Add BmpImage fields to `WinampSkin`:

```rust
pub numbers_bitmap: Option<BmpImage>,
pub cbuttons_bitmap: Option<BmpImage>,
pub posbar_bitmap: Option<BmpImage>,
pub text_bitmap: Option<BmpImage>,
pub playpaus_bitmap: Option<BmpImage>,
pub titlebar_bitmap: Option<BmpImage>,
pub monoster_bitmap: Option<BmpImage>,
pub shufrep_bitmap: Option<BmpImage>,
pub volume_bitmap: Option<BmpImage>,
```

`from_wsz()` already has the raw BMP data — call `parse_bmp_8bit()` for each additional file.

## Changes to `src/ui/winamp.rs`

- Current `render()` renamed to `render_text_mode()`
- New `render()` checks for bitmap mode availability and dispatches
- New `render_bitmap_mode()` implements the layered bitmap rendering path
- `SC` struct and color resolution unchanged — still needed for overlay colors
- `render_playlist_body()` and `render_footer()` unchanged
- Mouse hit-testing and `LayoutRects` same concept, adjusted coordinates
- All keyboard handling in `app.rs` unchanged

## Layout Sizing

Main window gets fixed rows matching MAIN.BMP aspect ratio. Playlist fills remaining space below. Footer stays at 2 rows. Extra terminal space beyond the bitmap width gets `body_bg` fill.

## File Summary

| File | Change |
|------|--------|
| `src/skin.rs` | Store additional BmpImage fields, parse them in `from_wsz()` |
| `src/ui/skin_bitmap.rs` | Add `render_bitmap_region()` for sub-rectangle cropping and rendering |
| `src/ui/skin_layout.rs` | **New** — zone definitions, SkinLayout struct, Winamp 2.x pixel coordinates |
| `src/ui/winamp.rs` | Add bitmap mode path, rename current render to text fallback |
