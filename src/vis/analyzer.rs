//! Spectrum + oscilloscope analyzer fed from the audio [`Tap`].
//!
//! Each [`Analyzer::update`] snapshots the most recent window, applies a
//! cached Hann window, runs an in-place FFT, converts bin magnitudes to a
//! normalized dB scale and folds them into log-spaced bands. Smoothing is
//! time-based (`dt`), so the animation looks the same at any frame rate.
//! Nothing allocates after construction unless the band/scope sizes change.

use std::f32::consts::TAU;
use std::time::Duration;

use super::fft::Fft;
use crate::tap::Tap;

/// Rows kept for the spectrogram/waterfall history.
pub const HISTORY_ROWS: usize = 48;
/// Seconds of audio represented by one history row.
const HISTORY_ROW_SECS: f32 = 0.04;
/// Peak sample magnitude below which a window counts as silence (-80 dBFS).
const SILENCE: f32 = 1e-4;
/// Upper bound for one smoothing step, so a long stall cannot overshoot.
const MAX_DT: f32 = 0.5;

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct AnalyzerConfig {
    /// FFT window length; rounded up to a power of two in 64..=8192.
    pub fft_size: usize,
    /// Number of log-spaced output bands.
    pub bands: usize,
    pub min_hz: f32,
    /// Clamped to the Nyquist frequency of the stream.
    pub max_hz: f32,
    /// dB (re. a full-scale sine) mapped to 0.0.
    pub floor_db: f32,
    /// dB mapped to 1.0.
    pub ceil_db: f32,
    /// Spectral tilt around 1 kHz compensating music's natural high-frequency
    /// roll-off; 0 disables it.
    pub tilt_db_per_octave: f32,
    /// Rise time constant in seconds (0 = instant).
    pub attack_secs: f32,
    /// Linear fall rate in full-scale units per second.
    pub decay_per_sec: f32,
    /// How long a peak marker stays put before falling.
    pub peak_hold_secs: f32,
    /// Peak marker fall acceleration in full-scale units per second².
    pub peak_gravity: f32,
    /// Number of oscilloscope points.
    pub scope_len: usize,
    /// Most recent samples the oscilloscope covers (clamped to `fft_size`).
    pub scope_window: usize,
}

impl Default for AnalyzerConfig {
    fn default() -> Self {
        Self {
            fft_size: 2048,
            bands: 32,
            min_hz: 20.0,
            max_hz: 20_000.0,
            floor_db: -70.0,
            ceil_db: -10.0,
            tilt_db_per_octave: 3.0,
            attack_secs: 0.02,
            decay_per_sec: 1.5,
            peak_hold_secs: 0.3,
            peak_gravity: 3.0,
            scope_len: 128,
            scope_window: 1024,
        }
    }
}

impl AnalyzerConfig {
    /// Winamp 2.x main-window geometry: 19 bars, 75-px oscilloscope.
    pub fn winamp() -> Self {
        Self {
            bands: 19,
            scope_len: 75,
            scope_window: 576,
            ..Self::default()
        }
    }

    fn sanitized(mut self) -> Self {
        self.fft_size = self.fft_size.clamp(64, 8192).next_power_of_two();
        self.bands = self.bands.max(1);
        if self.min_hz.is_nan() || self.min_hz <= 0.0 {
            self.min_hz = 20.0;
        }
        if self.max_hz.is_nan() || self.max_hz <= self.min_hz {
            self.max_hz = self.min_hz * 1000.0;
        }
        if self.ceil_db.is_nan() || self.floor_db.is_nan() || self.ceil_db <= self.floor_db {
            self.floor_db = -70.0;
            self.ceil_db = -10.0;
        }
        self.attack_secs = self.attack_secs.max(0.0);
        self.decay_per_sec = self.decay_per_sec.max(0.0);
        self.peak_hold_secs = self.peak_hold_secs.max(0.0);
        self.peak_gravity = self.peak_gravity.max(0.0);
        self.scope_len = self.scope_len.max(1);
        self.scope_window = self.scope_window.clamp(1, self.fft_size);
        self
    }
}

/// How one output band is derived from FFT bins.
#[derive(Debug, Clone, Copy, Default)]
struct Band {
    #[cfg(test)]
    lo_hz: f32,
    #[cfg(test)]
    hi_hz: f32,
    /// Integer bins `start..end` inside the band. Empty when the band is
    /// narrower than a bin; `center` is then interpolated instead.
    start: u32,
    end: u32,
    /// Fractional bin of the band's geometric centre.
    center: f32,
    /// Linear amplitude gain from the spectral tilt.
    gain: f32,
}

/// Read-only view of the spectrogram history ring, newest row first.
#[derive(Debug, Clone, Copy, Default)]
pub struct HistoryView<'a> {
    data: &'a [f32],
    bands: usize,
    head: usize,
    len: usize,
}

impl<'a> HistoryView<'a> {
    #[cfg(test)]
    /// Contiguous rows of `bands` values each, newest first.
    pub fn from_rows(rows: &'a [f32], bands: usize) -> Self {
        let len = rows.len().checked_div(bands).unwrap_or(0);
        Self {
            data: rows,
            bands,
            head: 0,
            len,
        }
    }

    #[cfg(test)]
    pub fn len(&self) -> usize {
        self.len
    }

    #[cfg(test)]
    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// Row `age` (0 = newest), or `None` past the recorded history.
    pub fn row(&self, age: usize) -> Option<&'a [f32]> {
        if age >= self.len {
            return None;
        }
        let cap = self.data.len() / self.bands;
        let r = (self.head + age) % cap;
        Some(&self.data[r * self.bands..(r + 1) * self.bands])
    }
}

pub struct Analyzer {
    cfg: AnalyzerConfig,
    fft: Fft,
    window: Box<[f32]>,
    samples: Box<[f32]>,
    re: Box<[f32]>,
    im: Box<[f32]>,
    /// Normalized amplitude per bin, `0..=fft_size/2` (1.0 = full-scale sine).
    mags: Box<[f32]>,
    map: Vec<Band>,
    sample_rate: u32,
    target: Vec<f32>,
    spectrum: Vec<f32>,
    peaks: Vec<f32>,
    /// Value the peak started falling from.
    peak_origin: Vec<f32>,
    /// Seconds since the hold expired (negative while holding).
    peak_time: Vec<f32>,
    scope: Vec<f32>,
    history: Vec<f32>,
    hist_acc: Vec<f32>,
    hist_head: usize,
    hist_len: usize,
    hist_timer: f32,
    frame: u64,
    idle: bool,
}

impl Analyzer {
    pub fn new(cfg: AnalyzerConfig) -> Self {
        let cfg = cfg.sanitized();
        let n = cfg.fft_size;
        let bands = cfg.bands;
        let window = (0..n)
            .map(|i| 0.5 - 0.5 * (TAU * i as f32 / n as f32).cos())
            .collect();
        let mut a = Self {
            cfg,
            fft: Fft::new(n),
            window,
            samples: vec![0.0; n].into_boxed_slice(),
            re: vec![0.0; n].into_boxed_slice(),
            im: vec![0.0; n].into_boxed_slice(),
            mags: vec![0.0; n / 2 + 1].into_boxed_slice(),
            map: vec![Band::default(); bands],
            sample_rate: 44_100,
            target: vec![0.0; bands],
            spectrum: vec![0.0; bands],
            peaks: vec![0.0; bands],
            peak_origin: vec![0.0; bands],
            peak_time: vec![0.0; bands],
            scope: vec![0.0; cfg.scope_len],
            history: vec![0.0; bands * HISTORY_ROWS],
            hist_acc: vec![0.0; bands],
            hist_head: 0,
            hist_len: 0,
            hist_timer: 0.0,
            frame: 0,
            idle: true,
        };
        a.rebuild_map();
        a
    }

    #[cfg(test)]
    pub fn bands(&self) -> usize {
        self.cfg.bands
    }

    #[cfg(test)]
    pub fn sample_rate(&self) -> u32 {
        self.sample_rate
    }

    /// Smoothed band levels, 0.0..=1.0, low → high frequency.
    pub fn spectrum(&self) -> &[f32] {
        &self.spectrum
    }

    /// Peak-hold markers per band, always `>= spectrum()[i]`.
    pub fn peaks(&self) -> &[f32] {
        &self.peaks
    }

    /// Decimated waveform, -1.0..=1.0, oldest first.
    pub fn scope(&self) -> &[f32] {
        &self.scope
    }

    pub fn history(&self) -> HistoryView<'_> {
        HistoryView {
            data: &self.history,
            bands: self.cfg.bands,
            head: self.hist_head,
            len: self.hist_len,
        }
    }

    /// Number of updates so far; drives deterministic animation (e.g. flicker).
    pub fn frame(&self) -> u64 {
        self.frame
    }

    /// True once the input is silent and every bar and peak reached zero:
    /// further updates change nothing until audio resumes.
    pub fn is_idle(&self) -> bool {
        self.idle
    }

    #[cfg(test)]
    /// Frequency range `(lo, hi)` in Hz covered by band `i`.
    pub fn band_range_hz(&self, i: usize) -> Option<(f32, f32)> {
        self.map.get(i).map(|b| (b.lo_hz, b.hi_hz))
    }

    /// Change the band count (allocates only when growing). Resets levels.
    pub fn set_bands(&mut self, bands: usize) {
        let bands = bands.max(1);
        if bands == self.cfg.bands {
            return;
        }
        self.cfg.bands = bands;
        for v in [
            &mut self.target,
            &mut self.spectrum,
            &mut self.peaks,
            &mut self.peak_origin,
            &mut self.peak_time,
            &mut self.hist_acc,
        ] {
            v.clear();
            v.resize(bands, 0.0);
        }
        self.history.clear();
        self.history.resize(bands * HISTORY_ROWS, 0.0);
        self.hist_head = 0;
        self.hist_len = 0;
        self.map.resize(bands, Band::default());
        self.rebuild_map();
    }

    /// Change the oscilloscope point count (allocates only when growing).
    pub fn set_scope_len(&mut self, len: usize) {
        let len = len.max(1);
        self.cfg.scope_len = len;
        self.scope.clear();
        self.scope.resize(len, 0.0);
    }

    /// Advance the analyzer by `dt`. While `playing`, the latest window is
    /// read from `tap`; otherwise the input counts as silence and the display
    /// decays. Returns whether anything visible changed (false once idle).
    pub fn update(&mut self, tap: &Tap, dt: Duration, playing: bool) -> bool {
        let sr = tap.sample_rate();
        if sr != self.sample_rate {
            self.sample_rate = sr;
            self.rebuild_map();
        }
        let silent = if playing {
            let n = self.samples.len();
            let got = tap.snapshot_mono(&mut self.samples);
            if got < n {
                // Keep the newest sample at the end; pad the past with silence.
                self.samples.copy_within(0..got, n - got);
                self.samples[..n - got].fill(0.0);
            }
            self.samples.iter().all(|s| s.abs() < SILENCE)
        } else {
            true
        };
        self.step(dt.as_secs_f32().min(MAX_DT), silent)
    }

    fn step(&mut self, dt: f32, silent: bool) -> bool {
        if silent {
            self.target.fill(0.0);
            self.scope.fill(0.0);
        } else {
            self.analyze();
            self.decimate_scope();
        }
        self.smooth(dt);
        self.push_history(dt);
        self.frame = self.frame.wrapping_add(1);

        let was_idle = self.idle;
        self.idle = silent
            && self.spectrum.iter().all(|&v| v == 0.0)
            && self.peaks.iter().all(|&v| v == 0.0);
        if self.idle && !was_idle {
            // The waterfall would freeze once ticking stops; blank it instead.
            self.hist_len = 0;
        }
        !(was_idle && self.idle)
    }

    fn rebuild_map(&mut self) {
        let n = self.cfg.fft_size;
        let nyq = (n / 2) as u32;
        let sr = self.sample_rate.max(1) as f32;
        let max_hz = self.cfg.max_hz.min(sr * 0.5);
        let min_hz = self.cfg.min_hz.min(max_hz * 0.5);
        let ratio = max_hz / min_hz;
        let bins_per_hz = n as f32 / sr;
        let count = self.map.len();
        let tilt = self.cfg.tilt_db_per_octave;
        for (i, b) in self.map.iter_mut().enumerate() {
            let lo_hz = min_hz * ratio.powf(i as f32 / count as f32);
            let hi_hz = if i + 1 == count {
                max_hz
            } else {
                min_hz * ratio.powf((i + 1) as f32 / count as f32)
            };
            let lo = lo_hz * bins_per_hz;
            let hi = hi_hz * bins_per_hz;
            let start = (lo.ceil() as u32).clamp(1, nyq);
            let mut end = (hi.ceil() as u32).min(nyq + 1);
            if i + 1 == count {
                // Close the last band so the top edge bin is included.
                end = end.max(hi.floor() as u32 + 1).min(nyq + 1);
            }
            let center_hz = (lo_hz * hi_hz).sqrt();
            *b = Band {
                #[cfg(test)]
                lo_hz,
                #[cfg(test)]
                hi_hz,
                start,
                end: end.max(start),
                center: (center_hz * bins_per_hz).clamp(1.0, nyq as f32),
                gain: 10f32.powf(tilt * (center_hz / 1000.0).log2() / 20.0),
            };
        }
    }

    fn analyze(&mut self) {
        for (((r, i), &s), &w) in self
            .re
            .iter_mut()
            .zip(self.im.iter_mut())
            .zip(self.samples.iter())
            .zip(self.window.iter())
        {
            *r = s * w;
            *i = 0.0;
        }
        self.fft.forward(&mut self.re, &mut self.im);

        // Hann coherent gain is 1/2, so a unit sine peaks at N/4.
        let scale = 4.0 / self.cfg.fft_size as f32;
        for ((m, &r), &i) in self.mags.iter_mut().zip(self.re.iter()).zip(self.im.iter()) {
            *m = (r * r + i * i).sqrt() * scale;
        }

        let floor = self.cfg.floor_db;
        let range = self.cfg.ceil_db - floor;
        for (t, b) in self.target.iter_mut().zip(&self.map) {
            let amp = band_amplitude(&self.mags, b) * b.gain;
            *t = if amp > 0.0 {
                ((20.0 * amp.log10() - floor) / range).clamp(0.0, 1.0)
            } else {
                0.0
            };
        }
    }

    fn decimate_scope(&mut self) {
        let w = self.cfg.scope_window;
        let src = &self.samples[self.samples.len() - w..];
        let len = self.scope.len();
        for (i, o) in self.scope.iter_mut().enumerate() {
            let a = i * w / len;
            let b = ((i + 1) * w / len).max(a + 1);
            // Keep the extreme sample of each bucket so transients survive.
            let best = src[a..b]
                .iter()
                .fold(0.0f32, |acc, &s| if s.abs() > acc.abs() { s } else { acc });
            *o = best.clamp(-1.0, 1.0);
        }
    }

    fn smooth(&mut self, dt: f32) {
        let attack = if self.cfg.attack_secs > 0.0 {
            1.0 - (-dt / self.cfg.attack_secs).exp()
        } else {
            1.0
        };
        let fall = self.cfg.decay_per_sec * dt;
        let hold = self.cfg.peak_hold_secs;
        let g = self.cfg.peak_gravity;
        for ((((v, &t), p), origin), pt) in self
            .spectrum
            .iter_mut()
            .zip(&self.target)
            .zip(self.peaks.iter_mut())
            .zip(self.peak_origin.iter_mut())
            .zip(self.peak_time.iter_mut())
        {
            if t > *v {
                *v += (t - *v) * attack;
            } else {
                *v = (*v - fall).max(t);
            }
            if *v > *p {
                *p = *v;
                *origin = *v;
                *pt = -hold;
            } else {
                *pt += dt;
                if *pt > 0.0 {
                    // Closed-form free fall: independent of frame rate.
                    *p = (*origin - 0.5 * g * *pt * *pt).max(*v);
                }
            }
        }
    }

    fn push_history(&mut self, dt: f32) {
        for (a, &v) in self.hist_acc.iter_mut().zip(&self.spectrum) {
            *a = a.max(v);
        }
        self.hist_timer += dt;
        let bands = self.cfg.bands;
        let mut pushed = 0;
        while self.hist_timer >= HISTORY_ROW_SECS && pushed < HISTORY_ROWS {
            self.hist_timer -= HISTORY_ROW_SECS;
            self.hist_head = (self.hist_head + HISTORY_ROWS - 1) % HISTORY_ROWS;
            let at = self.hist_head * bands;
            self.history[at..at + bands].copy_from_slice(&self.hist_acc);
            self.hist_len = (self.hist_len + 1).min(HISTORY_ROWS);
            pushed += 1;
        }
        if pushed == HISTORY_ROWS {
            self.hist_timer = 0.0;
        }
        if pushed > 0 {
            self.hist_acc.fill(0.0);
        }
    }
}

fn band_amplitude(mags: &[f32], b: &Band) -> f32 {
    if b.end > b.start {
        mags[b.start as usize..b.end as usize]
            .iter()
            .fold(0.0f32, |acc, &m| acc.max(m))
    } else {
        let k0 = b.center.floor() as usize;
        let k1 = (k0 + 1).min(mags.len() - 1);
        let f = b.center - k0 as f32;
        mags[k0] * (1.0 - f) + mags[k1] * f
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const FPS: Duration = Duration::from_nanos(1_000_000_000 / 30);

    fn sine_tap(sr: u32, hz: f32, amp: f32) -> Tap {
        let tap = Tap::new();
        tap.set_format(1, sr);
        for i in 0..8192 {
            let t = i as f64 / sr as f64;
            tap.push(amp * (std::f64::consts::TAU * hz as f64 * t).sin() as f32);
        }
        tap
    }

    fn argmax(v: &[f32]) -> usize {
        v.iter()
            .enumerate()
            .fold(
                (0, f32::MIN),
                |acc, (i, &x)| if x > acc.1 { (i, x) } else { acc },
            )
            .0
    }

    fn with_rate(cfg: AnalyzerConfig, sr: u32) -> Analyzer {
        let mut a = Analyzer::new(cfg);
        a.sample_rate = sr;
        a.rebuild_map();
        a
    }

    #[test]
    fn bands_cover_audible_range_monotonically_without_gaps() {
        for sr in [44_100, 48_000, 96_000] {
            for bands in [10, 19, 75, 200] {
                let cfg = AnalyzerConfig {
                    bands,
                    ..AnalyzerConfig::default()
                };
                let a = with_rate(cfg, sr);
                let nyq = (a.cfg.fft_size / 2) as f32;
                assert_eq!(a.map.len(), bands);
                assert!((a.map[0].lo_hz - 20.0).abs() < 1e-3);
                assert!((a.map[bands - 1].hi_hz - 20_000.0).abs() < 0.5);
                for (i, b) in a.map.iter().enumerate() {
                    assert!(b.hi_hz > b.lo_hz, "sr {sr} band {i} degenerate");
                    let has_bins = b.end > b.start;
                    let interpolated = b.center >= 1.0 && b.center < nyq;
                    assert!(has_bins || interpolated, "sr {sr} band {i} empty");
                    if i > 0 {
                        let prev = &a.map[i - 1];
                        assert!((b.lo_hz - prev.hi_hz).abs() < 1e-2, "gap before {i}");
                        assert!(b.lo_hz > prev.lo_hz, "edges not monotonic at {i}");
                        // Sub-bin bands below bin 1 share the clamped centre.
                        assert!(b.center >= prev.center, "centres not monotonic at {i}");
                        assert!(b.start >= prev.start);
                    }
                }
            }
        }
    }

    #[test]
    fn max_hz_clamps_to_nyquist() {
        let a = with_rate(AnalyzerConfig::default(), 22_050);
        let last = a.map.last().copied().unwrap_or_default();
        assert!((last.hi_hz - 11_025.0).abs() < 0.5);
        assert!(last.end as usize <= a.cfg.fft_size / 2 + 1);
    }

    #[test]
    fn sine_lights_its_own_band() {
        let cfg = AnalyzerConfig::winamp();
        let sr = 44_100;
        for band in [6, 10, 15] {
            let mut a = with_rate(cfg, sr);
            let (lo, hi) = a.band_range_hz(band).unwrap_or_default();
            let tap = sine_tap(sr, (lo * hi).sqrt(), 0.5);
            for _ in 0..10 {
                a.update(&tap, FPS, true);
            }
            assert_eq!(argmax(&a.target), band);
            assert_eq!(argmax(a.spectrum()), band);
            assert!(a.spectrum()[band] > 0.8);
            assert!(!a.is_idle());
        }
    }

    #[test]
    fn noise_reaches_every_band() {
        let tap = Tap::new();
        tap.set_format(1, 44_100);
        let mut seed: u32 = 0x1234_5678;
        for _ in 0..8192 {
            seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            tap.push((seed >> 8) as f32 / (1u32 << 23) as f32 - 1.0);
        }
        let mut a = with_rate(
            AnalyzerConfig {
                bands: 75,
                ..AnalyzerConfig::default()
            },
            44_100,
        );
        a.update(&tap, FPS, true);
        for (i, &t) in a.target.iter().enumerate() {
            assert!(t > 0.0, "band {i} stayed dark");
        }
    }

    #[test]
    fn silence_decays_to_exact_zero_then_idles() {
        let tap = sine_tap(44_100, 1_000.0, 0.8);
        let mut a = Analyzer::new(AnalyzerConfig::default());
        assert!(a.is_idle());
        for _ in 0..10 {
            assert!(a.update(&tap, FPS, true));
        }
        assert!(!a.is_idle());
        assert!(!a.history().is_empty());

        // Paused: the tap still holds audio but must be ignored.
        let mut frames = 0;
        while !a.is_idle() {
            assert!(a.update(&tap, FPS, false));
            frames += 1;
            assert!(frames < 90, "never went idle");
        }
        assert!(a.spectrum().iter().all(|&v| v == 0.0));
        assert!(a.peaks().iter().all(|&v| v == 0.0));
        assert!(a.scope().iter().all(|&v| v == 0.0));
        assert!(a.history().is_empty());
        // Once idle, further updates report no change.
        assert!(!a.update(&tap, FPS, false));

        // Playing, but the stream is silent: same outcome.
        for _ in 0..10 {
            a.update(&tap, FPS, true);
        }
        assert!(!a.is_idle());
        tap.clear();
        for _ in 0..90 {
            a.update(&tap, FPS, true);
        }
        assert!(a.is_idle());
        assert!(a.spectrum().iter().all(|&v| v == 0.0));
    }

    #[test]
    fn peaks_hold_then_fall() {
        let tap = sine_tap(44_100, 1_000.0, 0.8);
        let mut a = Analyzer::new(AnalyzerConfig::default());
        for _ in 0..10 {
            a.update(&tap, FPS, true);
        }
        let k = argmax(a.peaks());
        let top = a.peaks()[k];
        assert!(top > 0.5);

        let dt = Duration::from_millis(10);
        // Hold phase (0.3 s): peak stays while the bar already falls.
        for _ in 0..15 {
            a.update(&tap, dt, false);
            assert_eq!(a.peaks()[k], top);
        }
        assert!(a.spectrum()[k] < top);

        let mut last = top;
        let mut fell = false;
        for _ in 0..200 {
            a.update(&tap, dt, false);
            let p = a.peaks()[k];
            assert!(p <= last, "peak rose while silent");
            assert!(p >= a.spectrum()[k]);
            fell |= p < top;
            last = p;
        }
        assert!(fell);
        assert_eq!(last, 0.0);
    }

    #[test]
    fn smoothing_is_frame_rate_independent() {
        let tap = sine_tap(44_100, 2_000.0, 0.8);
        let mut fast = Analyzer::new(AnalyzerConfig::default());
        let mut slow = Analyzer::new(AnalyzerConfig::default());
        for _ in 0..10 {
            fast.update(&tap, FPS, true);
            slow.update(&tap, FPS, true);
        }
        // 0.6 s of decay at 120 fps vs 15 fps.
        for _ in 0..72 {
            fast.update(&tap, Duration::from_nanos(1_000_000_000 / 120), false);
        }
        for _ in 0..9 {
            slow.update(&tap, Duration::from_nanos(1_000_000_000 / 15), false);
        }
        for (f, s) in fast.spectrum().iter().zip(slow.spectrum()) {
            assert!((f - s).abs() < 1e-3, "{f} vs {s}");
        }
        for (f, s) in fast.peaks().iter().zip(slow.peaks()) {
            assert!((f - s).abs() < 1e-3, "{f} vs {s}");
        }
    }

    #[test]
    fn scope_is_decimated_to_requested_length() {
        let tap = sine_tap(44_100, 440.0, 0.5);
        let mut a = Analyzer::new(AnalyzerConfig::winamp());
        a.update(&tap, FPS, true);
        assert_eq!(a.scope().len(), 75);
        let max = a.scope().iter().fold(0.0f32, |m, &s| m.max(s.abs()));
        assert!((0.45..=0.5).contains(&max), "{max}");

        for len in [1, 76, 4096] {
            a.set_scope_len(len);
            a.update(&tap, FPS, true);
            assert_eq!(a.scope().len(), len);
            assert!(a.scope().iter().all(|s| (-1.0..=1.0).contains(s)));
            assert!(a.scope().iter().any(|&s| s != 0.0));
        }
    }

    #[test]
    fn short_tap_and_band_resize() {
        let tap = Tap::new();
        tap.set_format(2, 48_000);
        for i in 0..200 {
            let s = if i % 4 < 2 { 0.5 } else { -0.5 };
            tap.push(s);
        }
        let mut a = Analyzer::new(AnalyzerConfig::default());
        a.update(&tap, FPS, true);
        assert_eq!(a.sample_rate(), 48_000);
        // Newest samples land at the end of the window.
        assert!(a.samples[a.samples.len() - 1] != 0.0);
        assert_eq!(a.samples[0], 0.0);

        a.set_bands(7);
        assert_eq!(a.spectrum().len(), 7);
        assert_eq!(a.peaks().len(), 7);
        a.update(&tap, FPS, true);
        assert_eq!(a.history().row(0).map(<[f32]>::len), Some(7));
    }

    #[test]
    fn history_ring_is_newest_first() {
        let mut a = Analyzer::new(AnalyzerConfig {
            bands: 2,
            ..AnalyzerConfig::default()
        });
        for step in 0..(HISTORY_ROWS + 5) {
            a.spectrum = vec![step as f32, 0.0];
            a.push_history(HISTORY_ROW_SECS + 1e-4);
        }
        let h = a.history();
        assert_eq!(h.len(), HISTORY_ROWS);
        let newest = (HISTORY_ROWS + 4) as f32;
        assert_eq!(h.row(0).map(|r| r[0]), Some(newest));
        assert_eq!(h.row(1).map(|r| r[0]), Some(newest - 1.0));
        assert_eq!(h.row(HISTORY_ROWS), None);
    }
}
