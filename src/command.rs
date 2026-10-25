//! Every user-visible player action. Keys, mouse/skin hit-tests, the IPC
//! socket and OS media controls all translate into `Command`s; a single
//! dispatcher in the app applies them.

use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::view::RepeatMode;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "cmd", rename_all = "snake_case")]
pub enum Command {
    Play,
    Pause,
    TogglePause,
    Stop,
    Next,
    Prev,
    /// Absolute position.
    SeekTo(Duration),
    /// Relative seek in seconds (negative = back).
    SeekBy(i64),
    /// Fraction of track length, 0.0..=1.0.
    SeekFrac(f32),
    /// 0.0..=1.0
    SetVolume(f32),
    /// Relative, e.g. +0.05
    VolumeBy(f32),
    /// -1.0..=1.0
    SetBalance(f32),
    ToggleShuffle,
    CycleRepeat,
    SetRepeat(RepeatMode),
    ToggleEq,
    ToggleEqAuto,
    SetEqBand {
        band: usize,
        db: f32,
    },
    SetPreamp(f32),
    EqPreset(String),
    /// Play queue entry by index.
    PlayIndex(usize),
    /// Move the playlist cursor.
    Select(usize),
    /// Open the file/URL prompt (Winamp "eject").
    Eject,
    ToggleEqWindow,
    TogglePlaylistWindow,
    Quit,
}
