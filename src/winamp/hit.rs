//! Window-pixel hit testing and slider value math. Coordinates are window
//! pixels (0..275 × 0..116, playlist 0..275 × 0..height); fractional values
//! come from mapping a terminal cell centre into the window.

use crate::command::Command;
use crate::view::{PlayStatus, PlayerView, EQ_BANDS, EQ_MAX_DB};

use super::compose::playlist_track_area;
use super::sprites::{self as sp, eq_pos, main_pos, pl_pos, Px};
use super::{Element, Window};

const MAIN_TABLE: &[(Px, Element)] = &[
    (main_pos::OPTIONS, Element::Options),
    (main_pos::MINIMIZE, Element::Minimize),
    (main_pos::SHADE, Element::Shade),
    (main_pos::CLOSE, Element::Close),
    (main_pos::TIME, Element::Time),
    (main_pos::VIS, Element::Vis),
    (main_pos::MARQUEE, Element::Marquee),
    (main_pos::VOLUME, Element::Volume),
    (main_pos::BALANCE, Element::Balance),
    (main_pos::EQ_BUTTON, Element::EqButton),
    (main_pos::PL_BUTTON, Element::PlButton),
    (main_pos::POSITION, Element::Position),
    (main_pos::PREVIOUS, Element::Prev),
    (main_pos::PLAY, Element::Play),
    (main_pos::PAUSE, Element::Pause),
    (main_pos::STOP, Element::Stop),
    (main_pos::NEXT, Element::Next),
    (main_pos::EJECT, Element::Eject),
    (main_pos::SHUFFLE, Element::Shuffle),
    (main_pos::REPEAT, Element::Repeat),
];

const EQ_TABLE: &[(Px, Element)] = &[
    (eq_pos::CLOSE, Element::EqClose),
    (eq_pos::SHADE, Element::EqShade),
    (eq_pos::ON, Element::EqOn),
    (eq_pos::AUTO, Element::EqAuto),
    (eq_pos::PRESETS, Element::EqPresets),
    (eq_pos::PREAMP, Element::Preamp),
];

/// Element under (x, y) in `window`. `pl_height` is the playlist height in px.
pub fn hit(window: Window, x: f32, y: f32, pl_height: u32) -> Option<Element> {
    let table = match window {
        Window::Main => MAIN_TABLE,
        Window::Equalizer => {
            if let Some(i) = band_at(x, y) {
                return Some(Element::EqBand(i as u8));
            }
            EQ_TABLE
        }
        Window::Playlist => return hit_playlist(x, y, pl_height),
    };
    table
        .iter()
        .find(|(r, _)| r.contains(x, y))
        .map(|(_, e)| *e)
}

fn band_at(x: f32, y: f32) -> Option<usize> {
    (0..EQ_BANDS).find(|i| {
        eq_pos::BAND0
            .offset(*i as i32 * eq_pos::BAND_DX, 0)
            .contains(x, y)
    })
}

fn hit_playlist(x: f32, y: f32, h: u32) -> Option<Element> {
    if pl_pos::CLOSE.contains(x, y) {
        return Some(Element::PlClose);
    }
    if pl_pos::add(h).contains(x, y) {
        return Some(Element::PlAdd);
    }
    let ab = pl_pos::action_buttons(h);
    if ab.contains(x, y) {
        return Some(Element::PlAction(((x - ab.x as f32) / 10.0) as u8));
    }
    let area = playlist_track_area(h);
    if area.contains(x, y) {
        return Some(Element::PlTracks);
    }
    let scroll = Px {
        x: sp::WINDOW_W as i32 - sp::PL_RIGHT_W as i32,
        ..area
    };
    let scroll = Px {
        w: sp::PL_RIGHT_W,
        ..scroll
    };
    scroll.contains(x, y).then_some(Element::PlScroll)
}

/// Volume for a pointer at window x (thumb centred under the pointer).
pub fn volume_at(x: f32) -> f32 {
    let start = main_pos::VOLUME.x as f32 + 7.0;
    ((x - start) / sp::VOLUME_TRAVEL).clamp(0.0, 1.0)
}

/// Balance −1..=1 for a pointer at window x; snaps to centre like Winamp.
pub fn balance_at(x: f32) -> f32 {
    let start = main_pos::BALANCE.x as f32 + 7.0;
    let t = ((x - start) / sp::BALANCE_TRAVEL).clamp(0.0, 1.0);
    let b = t * 2.0 - 1.0;
    if b.abs() < 0.08 {
        0.0
    } else {
        b
    }
}

/// Seek fraction for a pointer at window x on the position bar.
pub fn position_at(x: f32) -> f32 {
    let start = main_pos::POSITION.x as f32 + sp::POSBAR_THUMB.w as f32 / 2.0;
    ((x - start) / sp::POSBAR_TRAVEL).clamp(0.0, 1.0)
}

/// EQ gain (dB) for a pointer at window y on any band slider; snaps to 0 dB.
pub fn eq_db_at(y: f32) -> f32 {
    let top = eq_pos::PREAMP.y as f32 + sp::EQ_SLIDER_THUMB.h as f32 / 2.0;
    let v = 1.0 - ((y - top) / sp::EQ_THUMB_TRAVEL).clamp(0.0, 1.0);
    let db = (v * 2.0 - 1.0) * EQ_MAX_DB;
    if db.abs() < 0.75 {
        0.0
    } else {
        db
    }
}

/// Command for a completed click (press and release on the same element).
pub fn click_command(el: Element, view: &PlayerView) -> Option<Command> {
    Some(match el {
        Element::Prev | Element::PlAction(0) => Command::Prev,
        Element::Play | Element::PlAction(1) => Command::Play,
        Element::Pause | Element::PlAction(2) => {
            if view.status == PlayStatus::Stopped {
                return None;
            }
            Command::TogglePause
        }
        Element::Stop | Element::PlAction(3) => Command::Stop,
        Element::Next | Element::PlAction(4) => Command::Next,
        Element::Eject | Element::PlAction(5) | Element::PlAdd => Command::Eject,
        Element::Shuffle => Command::ToggleShuffle,
        Element::Repeat => Command::CycleRepeat,
        Element::EqButton | Element::EqClose => Command::ToggleEqWindow,
        Element::PlButton | Element::PlClose => Command::TogglePlaylistWindow,
        Element::EqOn => Command::ToggleEq,
        Element::EqAuto => Command::ToggleEqAuto,
        Element::Close => Command::Quit,
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn main_window_buttons() {
        assert_eq!(hit(Window::Main, 20.0, 90.0, 0), Some(Element::Prev));
        assert_eq!(hit(Window::Main, 50.0, 100.0, 0), Some(Element::Play));
        assert_eq!(hit(Window::Main, 129.0, 95.0, 0), Some(Element::Next));
        assert_eq!(hit(Window::Main, 140.0, 95.0, 0), Some(Element::Eject));
        assert_eq!(hit(Window::Main, 170.0, 95.0, 0), Some(Element::Shuffle));
        assert_eq!(hit(Window::Main, 220.0, 95.0, 0), Some(Element::Repeat));
        assert_eq!(hit(Window::Main, 225.0, 60.0, 0), Some(Element::EqButton));
        assert_eq!(hit(Window::Main, 250.0, 60.0, 0), Some(Element::PlButton));
        assert_eq!(hit(Window::Main, 100.0, 75.0, 0), Some(Element::Position));
        assert_eq!(hit(Window::Main, 268.0, 7.0, 0), Some(Element::Close));
        assert_eq!(hit(Window::Main, 5.0, 50.0, 0), None);
    }

    #[test]
    fn eq_window_elements() {
        assert_eq!(hit(Window::Equalizer, 20.0, 22.0, 0), Some(Element::EqOn));
        assert_eq!(hit(Window::Equalizer, 50.0, 22.0, 0), Some(Element::EqAuto));
        assert_eq!(hit(Window::Equalizer, 25.0, 60.0, 0), Some(Element::Preamp));
        assert_eq!(
            hit(Window::Equalizer, 80.0, 60.0, 0),
            Some(Element::EqBand(0))
        );
        assert_eq!(
            hit(Window::Equalizer, 245.0, 60.0, 0),
            Some(Element::EqBand(9))
        );
        // Gap between bands.
        assert_eq!(hit(Window::Equalizer, 93.0, 60.0, 0), None);
    }

    #[test]
    fn playlist_elements_follow_height() {
        let h = 174;
        assert_eq!(
            hit(Window::Playlist, 100.0, 50.0, h),
            Some(Element::PlTracks)
        );
        assert_eq!(
            hit(Window::Playlist, 265.0, 50.0, h),
            Some(Element::PlScroll)
        );
        assert_eq!(hit(Window::Playlist, 268.0, 6.0, h), Some(Element::PlClose));
        assert_eq!(hit(Window::Playlist, 20.0, 150.0, h), Some(Element::PlAdd));
        let ab = pl_pos::action_buttons(h);
        assert_eq!(
            hit(Window::Playlist, ab.x as f32 + 25.0, ab.y as f32 + 5.0, h),
            Some(Element::PlAction(2))
        );
    }

    #[test]
    fn slider_math() {
        assert_eq!(volume_at(0.0), 0.0);
        assert_eq!(volume_at(114.0), 0.0);
        assert_eq!(volume_at(114.0 + 51.0), 1.0);
        assert!((volume_at(114.0 + 25.5) - 0.5).abs() < 1e-6);
        assert_eq!(balance_at(184.0), -1.0);
        assert_eq!(balance_at(184.0 + 24.0), 1.0);
        assert_eq!(balance_at(196.5), 0.0); // snapped
        assert_eq!(position_at(30.5), 0.0);
        assert_eq!(position_at(30.5 + 219.0), 1.0);
        assert!((position_at(30.5 + 109.5) - 0.5).abs() < 1e-6);
        assert_eq!(eq_db_at(43.5), 12.0);
        assert_eq!(eq_db_at(43.5 + 51.0), -12.0);
        assert_eq!(eq_db_at(43.5 + 25.0), 0.0); // snapped near centre
        assert!((eq_db_at(43.5 + 12.75) - 6.0).abs() < 1e-4);
    }

    #[test]
    fn click_commands() {
        let view = crate::winamp::tests::sample_view(PlayStatus::Stopped);
        assert_eq!(click_command(Element::Play, &view), Some(Command::Play));
        assert_eq!(click_command(Element::Pause, &view), None);
        let view = crate::winamp::tests::sample_view(PlayStatus::Playing);
        assert_eq!(
            click_command(Element::Pause, &view),
            Some(Command::TogglePause)
        );
        assert_eq!(
            click_command(Element::PlAction(4), &view),
            Some(Command::Next)
        );
        assert_eq!(
            click_command(Element::EqButton, &view),
            Some(Command::ToggleEqWindow)
        );
        assert_eq!(click_command(Element::Volume, &view), None);
    }
}
