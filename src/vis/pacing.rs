//! Redraw pacing: the visualizer only asks for frames while something moves.

use std::time::Duration;

pub const DEFAULT_FPS: u32 = 30;
/// While playing but silent, poll this often to notice audio resuming.
pub const PROBE_INTERVAL: Duration = Duration::from_millis(250);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FramePacer {
    fps: u32,
}

impl Default for FramePacer {
    fn default() -> Self {
        Self::new(DEFAULT_FPS)
    }
}

impl FramePacer {
    /// `fps` is clamped to 1..=120.
    pub fn new(fps: u32) -> Self {
        Self {
            fps: fps.clamp(1, 120),
        }
    }

    pub fn frame_interval(&self) -> Duration {
        Duration::from_secs(1) / self.fps
    }

    /// Desired redraw interval for the visualizer, or `None` when it needs no
    /// frames (hidden, or idle while not playing). `idle` is
    /// [`Analyzer::is_idle`](super::analyzer::Analyzer::is_idle).
    pub fn interval(&self, visible: bool, playing: bool, idle: bool) -> Option<Duration> {
        if !visible {
            return None;
        }
        match (playing, idle) {
            (_, false) => Some(self.frame_interval()),
            (true, true) => Some(PROBE_INTERVAL),
            (false, true) => None,
        }
    }

    /// Time left until the next frame is due, given how long ago the last
    /// one was drawn (`Duration::ZERO` = draw now). Suitable as an event-poll
    /// timeout; `None` means wait for input only.
    pub fn timeout(
        &self,
        since_last_frame: Duration,
        visible: bool,
        playing: bool,
        idle: bool,
    ) -> Option<Duration> {
        self.interval(visible, playing, idle)
            .map(|i| i.saturating_sub(since_last_frame))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn no_frames_when_hidden_or_idle_and_stopped() {
        let p = FramePacer::default();
        assert_eq!(p.interval(false, true, false), None);
        assert_eq!(p.interval(true, false, true), None);
        assert_eq!(p.interval(true, true, true), Some(PROBE_INTERVAL));
        // Still decaying after pause: keep animating.
        assert_eq!(
            p.interval(true, false, false),
            Some(Duration::from_secs(1) / 30)
        );
    }

    #[test]
    fn fps_is_clamped() {
        assert_eq!(FramePacer::new(0).frame_interval(), Duration::from_secs(1));
        assert_eq!(
            FramePacer::new(1000).frame_interval(),
            Duration::from_secs(1) / 120
        );
    }

    #[test]
    fn timeout_counts_down_and_saturates() {
        let p = FramePacer::new(20);
        let ms = Duration::from_millis;
        assert_eq!(p.timeout(ms(10), true, true, false), Some(ms(40)));
        assert_eq!(p.timeout(ms(80), true, true, false), Some(Duration::ZERO));
        assert_eq!(p.timeout(ms(10), true, false, true), None);
    }
}
