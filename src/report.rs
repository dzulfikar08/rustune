//! Playback reporting to the providers: radio-browser click counts, Subsonic
//! now-playing/scrobbles and Jellyfin play sessions. [`Tracker`] is a pure
//! state machine producing [`Call`]s; [`send`] performs one (blocking).

use std::time::Duration;

use crate::browse::Services;
use crate::media::{MediaItem, SourceKind};
use crate::providers::subsonic::SubsonicClient;
use crate::providers::Result;

/// A track counts as played after half its length, at most this long.
const SCROBBLE_CAP: Duration = Duration::from_secs(4 * 60);
/// Minimum position change between Jellyfin progress reports.
const JELLYFIN_INTERVAL: Duration = Duration::from_secs(10);

/// Where the playing item gets reported.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Target {
    Radio { uuid: String },
    Subsonic { song_id: String },
    Jellyfin { item_id: String },
}

/// Finds the reporting target of `item`: a radio station when the browser
/// knows its UUID, a Subsonic song when scrobbling is enabled, or a track of
/// the connected Jellyfin server. No network.
pub fn classify(
    item: &MediaItem,
    services: &Services,
    browsers_radio_uuid: Option<String>,
) -> Option<Target> {
    if item.source != SourceKind::Url {
        return None;
    }
    if let Some(uuid) = browsers_radio_uuid {
        return Some(Target::Radio { uuid });
    }
    let subsonic = services
        .config()
        .subsonic
        .as_ref()
        .filter(|c| c.is_configured() && c.scrobble);
    if let Some(song_id) = subsonic.and_then(|cfg| SubsonicClient::new(cfg).song_id(item)) {
        return Some(Target::Subsonic { song_id });
    }
    services
        .jellyfin_item_id(item)
        .map(|item_id| Target::Jellyfin { item_id })
}

/// One report to send with [`send`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Call {
    RadioClick(String),
    SubsonicNowPlaying(String),
    SubsonicSubmit(String),
    JellyfinStart {
        id: String,
        pos: Duration,
    },
    JellyfinProgress {
        id: String,
        pos: Duration,
        paused: bool,
    },
    JellyfinStopped {
        id: String,
        pos: Duration,
    },
}

/// Reporting state of the playing track.
#[derive(Debug)]
pub struct Tracker {
    target: Target,
    submitted: bool,
    reported_pos: Duration,
    paused: bool,
}

fn scrobble_threshold(duration: Option<Duration>) -> Duration {
    duration
        .filter(|d| !d.is_zero())
        .map_or(SCROBBLE_CAP, |d| (d / 2).min(SCROBBLE_CAP))
}

impl Tracker {
    /// Playback started: radio click, Subsonic now-playing or Jellyfin start.
    pub fn start(target: Target) -> (Self, Vec<Call>) {
        let call = match &target {
            Target::Radio { uuid } => Call::RadioClick(uuid.clone()),
            Target::Subsonic { song_id } => Call::SubsonicNowPlaying(song_id.clone()),
            Target::Jellyfin { item_id } => Call::JellyfinStart {
                id: item_id.clone(),
                pos: Duration::ZERO,
            },
        };
        let tracker = Self {
            target,
            submitted: false,
            reported_pos: Duration::ZERO,
            paused: false,
        };
        (tracker, vec![call])
    }

    /// Periodic tick: the Subsonic submission once past
    /// `min(duration / 2, 4 min)` (4 min when the length is unknown);
    /// Jellyfin progress every 10 s of position change or on pause/resume.
    pub fn progress(
        &mut self,
        elapsed: Duration,
        duration: Option<Duration>,
        paused: bool,
    ) -> Vec<Call> {
        match &self.target {
            Target::Radio { .. } => Vec::new(),
            Target::Subsonic { song_id } => {
                if self.submitted || elapsed < scrobble_threshold(duration) {
                    return Vec::new();
                }
                self.submitted = true;
                vec![Call::SubsonicSubmit(song_id.clone())]
            }
            Target::Jellyfin { item_id } => {
                let moved = if elapsed >= self.reported_pos {
                    elapsed - self.reported_pos
                } else {
                    self.reported_pos - elapsed
                };
                if paused == self.paused && moved < JELLYFIN_INTERVAL {
                    return Vec::new();
                }
                self.paused = paused;
                self.reported_pos = elapsed;
                vec![Call::JellyfinProgress {
                    id: item_id.clone(),
                    pos: elapsed,
                    paused,
                }]
            }
        }
    }

    /// Playback ended (finished, skipped or stopped).
    pub fn stop(self, elapsed: Duration) -> Vec<Call> {
        match self.target {
            Target::Jellyfin { item_id } => vec![Call::JellyfinStopped {
                id: item_id,
                pos: elapsed,
            }],
            Target::Radio { .. } | Target::Subsonic { .. } => Vec::new(),
        }
    }
}

/// Sends one report (blocking; run it on a blocking thread).
pub fn send(services: &Services, call: &Call) -> Result<()> {
    let agent = crate::net::agent();
    match call {
        Call::RadioClick(uuid) => services.radio().report_click(agent, uuid).map(|_| ()),
        Call::SubsonicNowPlaying(id) => services.subsonic()?.scrobble(agent, id, false),
        Call::SubsonicSubmit(id) => services.subsonic()?.scrobble(agent, id, true),
        Call::JellyfinStart { id, pos } => services.jellyfin()?.report_start(agent, id, *pos),
        Call::JellyfinProgress { id, pos, paused } => services
            .jellyfin()?
            .report_progress(agent, id, *pos, *paused),
        Call::JellyfinStopped { id, pos } => services.jellyfin()?.report_stopped(agent, id, *pos),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::providers::subsonic::SubsonicConfig;
    use crate::providers::ProvidersConfig;

    const fn secs(s: u64) -> Duration {
        Duration::from_secs(s)
    }

    fn subsonic() -> Tracker {
        let (tracker, calls) = Tracker::start(Target::Subsonic {
            song_id: "s1".into(),
        });
        assert_eq!(calls, vec![Call::SubsonicNowPlaying("s1".into())]);
        tracker
    }

    fn url_item(id: &str) -> MediaItem {
        MediaItem {
            id: id.to_string(),
            title: "t".into(),
            artist: None,
            album: None,
            duration: None,
            subtitle: None,
            source: SourceKind::Url,
        }
    }

    #[test]
    fn subsonic_submits_at_half_once() {
        let mut t = subsonic();
        let len = Some(secs(200));
        assert!(t.progress(secs(99), len, false).is_empty());
        assert_eq!(
            t.progress(secs(100), len, false),
            vec![Call::SubsonicSubmit("s1".into())]
        );
        assert!(t.progress(secs(150), len, false).is_empty());
        assert!(t.progress(secs(199), len, true).is_empty());
        assert!(t.stop(secs(200)).is_empty());
    }

    #[test]
    fn subsonic_threshold_caps_at_four_minutes() {
        let mut t = subsonic();
        let len = Some(secs(3600));
        assert!(t.progress(secs(239), len, false).is_empty());
        assert_eq!(t.progress(secs(240), len, false).len(), 1);

        let mut unknown = subsonic();
        assert!(unknown.progress(secs(239), None, false).is_empty());
        assert_eq!(unknown.progress(secs(240), None, false).len(), 1);

        let mut zero = subsonic();
        assert!(zero
            .progress(secs(1), Some(Duration::ZERO), false)
            .is_empty());
    }

    #[test]
    fn jellyfin_throttles_and_reports_pause_changes() {
        let (mut t, calls) = Tracker::start(Target::Jellyfin {
            item_id: "j1".into(),
        });
        assert_eq!(
            calls,
            vec![Call::JellyfinStart {
                id: "j1".into(),
                pos: Duration::ZERO,
            }]
        );
        let progress = |pos, paused| Call::JellyfinProgress {
            id: "j1".into(),
            pos,
            paused,
        };
        assert!(t.progress(secs(5), None, false).is_empty());
        assert_eq!(
            t.progress(secs(10), None, false),
            vec![progress(secs(10), false)]
        );
        assert!(t.progress(secs(19), None, false).is_empty());
        assert_eq!(
            t.progress(secs(12), None, true),
            vec![progress(secs(12), true)]
        );
        assert!(t.progress(secs(12), None, true).is_empty());
        assert_eq!(
            t.progress(secs(12), None, false),
            vec![progress(secs(12), false)]
        );
        assert_eq!(
            t.progress(secs(1), None, false),
            vec![progress(secs(1), false)],
            "seeking back 10 s reports"
        );
        assert_eq!(
            t.stop(secs(30)),
            vec![Call::JellyfinStopped {
                id: "j1".into(),
                pos: secs(30),
            }]
        );
    }

    #[test]
    fn radio_only_clicks() {
        let (mut t, calls) = Tracker::start(Target::Radio { uuid: "u".into() });
        assert_eq!(calls, vec![Call::RadioClick("u".into())]);
        assert!(t.progress(secs(600), None, false).is_empty());
        assert!(t.stop(secs(600)).is_empty());
    }

    #[test]
    fn classify_picks_target() {
        let sub_cfg = SubsonicConfig {
            url: "https://music.example.com".into(),
            user: "me".into(),
            password: "pw".into(),
            scrobble: true,
            ..SubsonicConfig::default()
        };
        let stream = SubsonicClient::new(&sub_cfg).stream_url("song-7");
        let services = Services::new(ProvidersConfig {
            subsonic: Some(sub_cfg.clone()),
            ..ProvidersConfig::default()
        });

        assert_eq!(
            classify(&url_item(&stream), &services, None),
            Some(Target::Subsonic {
                song_id: "song-7".into()
            })
        );
        assert_eq!(
            classify(
                &url_item("http://radio/stream"),
                &services,
                Some("u".into())
            ),
            Some(Target::Radio { uuid: "u".into() })
        );
        assert_eq!(classify(&url_item("http://other/x"), &services, None), None);
        let mut local = url_item("/music/a.mp3");
        local.source = SourceKind::Local;
        assert_eq!(classify(&local, &services, Some("u".into())), None);

        let quiet = Services::new(ProvidersConfig {
            subsonic: Some(SubsonicConfig {
                scrobble: false,
                ..sub_cfg
            }),
            ..ProvidersConfig::default()
        });
        assert_eq!(classify(&url_item(&stream), &quiet, None), None);
    }
}
