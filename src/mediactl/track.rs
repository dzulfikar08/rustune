//! What the OS media widgets show, derived from `PlayerView`, plus change
//! detection so backends publish only on real changes (the observer runs
//! after every redraw; position is extrapolated, not pushed every tick).

use std::path::PathBuf;
use std::time::{Duration, Instant};

use crate::media::{MediaItem, SourceKind};
use crate::view::{PlayStatus, PlayerView, RepeatMode};

/// A position jump larger than this while playing counts as a seek.
const SEEK_TOLERANCE: Duration = Duration::from_secs(1);

/// Where the current track comes from (for `xesam:url` and cover art).
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
pub enum Location {
    File(PathBuf),
    Url(String),
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct Track {
    /// Queue item id (file path, URL or extractor id); empty = no track.
    pub id: String,
    pub title: String,
    pub artist: Option<String>,
    pub album: Option<String>,
    pub length: Option<Duration>,
    #[cfg_attr(not(target_os = "linux"), allow(dead_code))]
    pub location: Option<Location>,
}

impl Track {
    pub fn is_empty(&self) -> bool {
        self.id.is_empty() && self.title.is_empty()
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct State {
    pub status: PlayStatus,
    pub track: Track,
    #[cfg_attr(not(target_os = "linux"), allow(dead_code))]
    pub volume: f32,
    #[cfg_attr(not(target_os = "linux"), allow(dead_code))]
    pub shuffle: bool,
    #[cfg_attr(not(target_os = "linux"), allow(dead_code))]
    pub repeat: RepeatMode,
    pub can_play: bool,
    pub can_next: bool,
    pub can_prev: bool,
    pub can_seek: bool,
}

impl Default for State {
    fn default() -> Self {
        Self {
            status: PlayStatus::Stopped,
            track: Track::default(),
            volume: 1.0,
            shuffle: false,
            repeat: RepeatMode::Off,
            can_play: false,
            can_next: false,
            can_prev: false,
            can_seek: false,
        }
    }
}

/// Playback clock: the last observed position and when it was observed.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Clock {
    pub elapsed: Duration,
    pub at: Instant,
    pub playing: bool,
    pub length: Option<Duration>,
}

impl Clock {
    pub fn new(now: Instant) -> Self {
        Self {
            elapsed: Duration::ZERO,
            at: now,
            playing: false,
            length: None,
        }
    }

    /// Extrapolated position at `now`, capped at the track length.
    pub fn position(&self, now: Instant) -> Duration {
        let pos = if self.playing {
            self.elapsed + now.saturating_duration_since(self.at)
        } else {
            self.elapsed
        };
        self.length.map_or(pos, |len| pos.min(len))
    }
}

/// What changed since the last observation.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Changes {
    pub track: bool,
    pub status: bool,
    #[cfg_attr(not(target_os = "linux"), allow(dead_code))]
    pub volume: bool,
    #[cfg_attr(not(target_os = "linux"), allow(dead_code))]
    pub shuffle: bool,
    #[cfg_attr(not(target_os = "linux"), allow(dead_code))]
    pub repeat: bool,
    pub caps: bool,
    /// The position jumped (seek, restart) to this value.
    pub seeked: Option<Duration>,
}

impl Changes {
    pub fn any(&self) -> bool {
        self.track
            || self.status
            || self.volume
            || self.shuffle
            || self.repeat
            || self.caps
            || self.seeked.is_some()
    }
}

/// "Artist - Title" exactly as `app::display_title` renders `item`.
fn is_display_title(text: &str, item: &MediaItem) -> bool {
    match &item.artist {
        Some(artist) => {
            text.strip_prefix(artist.as_str())
                .and_then(|rest| rest.strip_prefix(" - "))
                == Some(item.title.as_str())
        }
        None => text == item.title,
    }
}

fn non_empty(s: &str) -> Option<String> {
    let s = s.trim();
    (!s.is_empty()).then(|| s.to_string())
}

/// Metadata for the now-playing widgets. Tags of the queue item win; a
/// stream title (ICY, extractor) replaces them and is split on " - ".
pub fn track_from_view(view: &PlayerView<'_>) -> Track {
    let Some(item) = view.current.and_then(|i| view.playlist.get(i)) else {
        return Track {
            title: view.title.to_string(),
            length: view.duration,
            ..Track::default()
        };
    };
    let (title, artist) = if view.title.is_empty() || is_display_title(view.title, item) {
        (item.title.clone(), item.artist.clone())
    } else {
        match view.title.split_once(" - ") {
            Some((artist, title)) if !artist.trim().is_empty() && !title.trim().is_empty() => {
                (title.trim().to_string(), non_empty(artist))
            }
            _ => (view.title.to_string(), item.artist.clone()),
        }
    };
    let location = match &item.source {
        SourceKind::Local => Some(Location::File(PathBuf::from(&item.id))),
        SourceKind::Url => Some(Location::Url(item.id.clone())),
        SourceKind::Extractor(_) => None,
    };
    Track {
        id: item.id.clone(),
        title,
        artist,
        album: item.album.clone(),
        length: view.duration,
        location,
    }
}

/// Remembers what was published last and reports what changed.
pub struct Tracker {
    pub state: State,
    pub clock: Clock,
    /// `PlayerView::title` the current `state.track` was derived from.
    view_title: String,
    current: Option<usize>,
}

impl Tracker {
    /// Starts as "nothing published": the first observation reports
    /// every field that differs from an idle player.
    pub fn new(now: Instant) -> Self {
        Self {
            state: State::default(),
            clock: Clock::new(now),
            view_title: String::new(),
            current: None,
        }
    }

    fn same_track(&self, view: &PlayerView<'_>) -> bool {
        let id = view
            .current
            .and_then(|i| view.playlist.get(i))
            .map_or("", |item| item.id.as_str());
        self.current == view.current
            && self.state.track.id == id
            && self.view_title == view.title
            && self.state.track.length == view.duration
    }

    pub fn observe(&mut self, view: &PlayerView<'_>, now: Instant) -> Changes {
        let mut changes = Changes::default();
        if !self.same_track(view) {
            let track = track_from_view(view);
            changes.track = track != self.state.track;
            self.state.track = track;
            self.view_title.clear();
            self.view_title.push_str(view.title);
            self.current = view.current;
        }
        if view.status != self.state.status {
            self.state.status = view.status;
            changes.status = true;
        }
        if view.volume != self.state.volume {
            self.state.volume = view.volume;
            changes.volume = true;
        }
        if view.shuffle != self.state.shuffle {
            self.state.shuffle = view.shuffle;
            changes.shuffle = true;
        }
        if view.repeat != self.state.repeat {
            self.state.repeat = view.repeat;
            changes.repeat = true;
        }

        let len = view.playlist.len();
        let can_play = len > 0;
        let can_next = can_play
            && (view.repeat == RepeatMode::All
                || view.current.is_none_or(|i| view.shuffle || i + 1 < len));
        let can_seek = view.status != PlayStatus::Stopped && view.duration.is_some();
        let caps = (can_play, can_next, can_play, can_seek);
        let old = (
            self.state.can_play,
            self.state.can_next,
            self.state.can_prev,
            self.state.can_seek,
        );
        if caps != old {
            (
                self.state.can_play,
                self.state.can_next,
                self.state.can_prev,
                self.state.can_seek,
            ) = caps;
            changes.caps = true;
        }

        let playing = view.status == PlayStatus::Playing;
        if changes.track || changes.status {
            self.clock = Clock {
                elapsed: view.elapsed,
                at: now,
                playing,
                length: view.duration,
            };
        } else if view.elapsed != self.clock.elapsed {
            // Only a fresh engine reading moves the clock; a stale value
            // (redraw between clock ticks) keeps the extrapolation.
            let tolerance = if playing {
                SEEK_TOLERANCE
            } else {
                Duration::ZERO
            };
            if view.elapsed.abs_diff(self.clock.position(now)) > tolerance {
                changes.seeked = Some(view.elapsed);
            }
            self.clock.elapsed = view.elapsed;
            self.clock.at = now;
        }
        changes
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::view::{EqSettings, StreamFormat};

    pub fn item(id: &str, title: &str, artist: Option<&str>) -> MediaItem {
        MediaItem {
            id: id.into(),
            title: title.into(),
            artist: artist.map(Into::into),
            album: Some("Album".into()),
            duration: Some(200),
            subtitle: None,
            source: SourceKind::Local,
        }
    }

    pub fn view<'a>(
        playlist: &'a [MediaItem],
        current: Option<usize>,
        title: &'a str,
        status: PlayStatus,
        elapsed: Duration,
    ) -> PlayerView<'a> {
        PlayerView {
            title,
            status,
            elapsed,
            duration: Some(Duration::from_secs(200)),
            volume: 0.5,
            balance: 0.0,
            shuffle: false,
            repeat: RepeatMode::Off,
            eq: EqSettings::default(),
            format: StreamFormat::default(),
            spectrum: &[],
            scope: &[],
            playlist,
            current,
            selected: 0,
        }
    }

    #[test]
    fn tags_win_over_display_title() {
        let items = [item("/m/a.flac", "Song", Some("Band"))];
        let v = view(
            &items,
            Some(0),
            "Band - Song",
            PlayStatus::Playing,
            Duration::ZERO,
        );
        let t = track_from_view(&v);
        assert_eq!(t.title, "Song");
        assert_eq!(t.artist.as_deref(), Some("Band"));
        assert_eq!(t.album.as_deref(), Some("Album"));
        assert_eq!(t.length, Some(Duration::from_secs(200)));
        assert_eq!(t.location, Some(Location::File("/m/a.flac".into())));
    }

    #[test]
    fn stream_title_is_split() {
        let mut station = item("http://radio/x", "Station", None);
        station.source = SourceKind::Url;
        let items = [station];
        let v = view(
            &items,
            Some(0),
            "Artist - Live Song",
            PlayStatus::Playing,
            Duration::ZERO,
        );
        let t = track_from_view(&v);
        assert_eq!(t.title, "Live Song");
        assert_eq!(t.artist.as_deref(), Some("Artist"));
        assert_eq!(t.location, Some(Location::Url("http://radio/x".into())));

        let v = view(
            &items,
            Some(0),
            "Just a title",
            PlayStatus::Playing,
            Duration::ZERO,
        );
        let t = track_from_view(&v);
        assert_eq!(t.title, "Just a title");
        assert_eq!(t.artist, None);
    }

    #[test]
    fn no_current_item_is_empty_track() {
        let v = view(&[], None, "", PlayStatus::Stopped, Duration::ZERO);
        assert!(track_from_view(&v).is_empty());
    }

    #[test]
    fn first_observation_reports_and_repeat_reports_nothing() {
        let t0 = Instant::now();
        let items = [item("a", "A", None), item("b", "B", None)];
        let mut tr = Tracker::new(t0);
        let v = view(&items, Some(0), "A", PlayStatus::Playing, Duration::ZERO);
        let c = tr.observe(&v, t0);
        assert!(c.track && c.status && c.volume && c.caps);
        assert!(tr.state.can_next && tr.state.can_prev && tr.state.can_seek);
        let c = tr.observe(&v, t0 + Duration::from_millis(300));
        assert!(!c.any(), "{c:?}");
    }

    #[test]
    fn regular_progress_is_not_a_seek_but_jumps_are() {
        let t0 = Instant::now();
        let items = [item("a", "A", None)];
        let mut tr = Tracker::new(t0);
        tr.observe(
            &view(&items, Some(0), "A", PlayStatus::Playing, Duration::ZERO),
            t0,
        );
        let s = Duration::from_secs;
        let c = tr.observe(
            &view(&items, Some(0), "A", PlayStatus::Playing, s(1)),
            t0 + s(1),
        );
        assert_eq!(c, Changes::default());
        assert_eq!(tr.clock.position(t0 + s(3)), s(3));
        let c = tr.observe(
            &view(&items, Some(0), "A", PlayStatus::Playing, s(60)),
            t0 + s(2),
        );
        assert_eq!(c.seeked, Some(s(60)));
        // A stale reading (unchanged elapsed) keeps extrapolating.
        let c = tr.observe(
            &view(&items, Some(0), "A", PlayStatus::Playing, s(60)),
            t0 + s(4),
        );
        assert!(!c.any());
        assert_eq!(tr.clock.position(t0 + s(4)), s(62));
    }

    #[test]
    fn seek_while_paused_and_clock_stops() {
        let t0 = Instant::now();
        let s = Duration::from_secs;
        let items = [item("a", "A", None)];
        let mut tr = Tracker::new(t0);
        tr.observe(&view(&items, Some(0), "A", PlayStatus::Paused, s(10)), t0);
        assert_eq!(tr.clock.position(t0 + s(5)), s(10));
        let c = tr.observe(
            &view(
                &items,
                Some(0),
                "A",
                PlayStatus::Paused,
                s(10) + Duration::from_millis(400),
            ),
            t0 + s(5),
        );
        assert_eq!(c.seeked, Some(s(10) + Duration::from_millis(400)));
    }

    #[test]
    fn track_change_resets_clock_without_seek() {
        let t0 = Instant::now();
        let s = Duration::from_secs;
        let items = [item("a", "A", None), item("b", "B", None)];
        let mut tr = Tracker::new(t0);
        tr.observe(&view(&items, Some(0), "A", PlayStatus::Playing, s(100)), t0);
        let c = tr.observe(
            &view(&items, Some(1), "B", PlayStatus::Playing, s(0)),
            t0 + s(1),
        );
        assert!(c.track);
        assert_eq!(c.seeked, None);
        assert_eq!(tr.state.track.title, "B");
        assert!(!tr.state.can_next, "last item without repeat");
        assert_eq!(tr.clock.position(t0 + s(1)), s(0));
    }

    #[test]
    fn position_is_capped_at_length() {
        let t0 = Instant::now();
        let clock = Clock {
            elapsed: Duration::from_secs(190),
            at: t0,
            playing: true,
            length: Some(Duration::from_secs(200)),
        };
        assert_eq!(
            clock.position(t0 + Duration::from_secs(60)),
            Duration::from_secs(200)
        );
    }
}
