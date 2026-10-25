//! Sample-path DSP: 10-band peaking EQ with preamp, balance and volume.
//!
//! Parameters are written by any thread into [`DspParams`] (atomics plus an EQ
//! generation counter) and picked up by the audio thread once per buffer, so the
//! per-sample path never locks or allocates.

use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, AtomicU8, Ordering};

use crate::tap::Tap;
use crate::view::{EqSettings, EQ_BANDS, EQ_FREQS, EQ_MAX_DB};

use super::ReplayGainMode;

/// Channels with independent filter state; extra channels pass through the EQ.
pub const MAX_CHANNELS: usize = 8;
/// Band quality: about one octave, matching the spacing of Winamp's bands.
const BAND_Q: f32 = 1.41;
/// Gains closer to 0 dB than this are treated as flat (band skipped).
const FLAT_DB: f32 = 0.01;

#[inline]
pub fn db_to_gain(db: f32) -> f32 {
    10f32.powf(db / 20.0)
}

/// Perceptual volume curve: slider 0..=1 → linear gain (cubic, ≈ -18 dB at 0.5).
#[inline]
pub fn volume_gain(v: f32) -> f32 {
    let v = v.clamp(0.0, 1.0);
    v * v * v
}

/// Balance -1 (left) ..= 1 (right) → (left gain, right gain). The centre is
/// unity on both sides; moving towards one side attenuates the other linearly.
#[inline]
pub fn balance_gains(b: f32) -> (f32, f32) {
    let b = b.clamp(-1.0, 1.0);
    ((1.0 - b).min(1.0), (1.0 + b).min(1.0))
}

/// Normalised biquad coefficients (a0 = 1).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Biquad {
    b0: f32,
    b1: f32,
    b2: f32,
    a1: f32,
    a2: f32,
}

impl Biquad {
    pub const IDENTITY: Self = Self {
        b0: 1.0,
        b1: 0.0,
        b2: 0.0,
        a1: 0.0,
        a2: 0.0,
    };

    /// RBJ Audio EQ Cookbook peaking filter. Computed in f64 for accuracy at low
    /// frequencies, stored as f32.
    pub fn peaking(sample_rate: f32, freq: f32, q: f32, gain_db: f32) -> Self {
        let fs = f64::from(sample_rate);
        // Keep the centre safely below Nyquist (e.g. 16 kHz band at 32 kHz).
        let f0 = f64::from(freq).min(fs * 0.45);
        let a = 10f64.powf(f64::from(gain_db) / 40.0);
        let w0 = std::f64::consts::TAU * f0 / fs;
        let (sin, cos) = w0.sin_cos();
        let alpha = sin / (2.0 * f64::from(q));
        let a0 = 1.0 + alpha / a;
        Self {
            b0: ((1.0 + alpha * a) / a0) as f32,
            b1: ((-2.0 * cos) / a0) as f32,
            b2: ((1.0 - alpha * a) / a0) as f32,
            a1: ((-2.0 * cos) / a0) as f32,
            a2: ((1.0 - alpha / a) / a0) as f32,
        }
    }
}

/// Transposed direct form II state.
#[derive(Debug, Clone, Copy, Default)]
struct State {
    z1: f32,
    z2: f32,
}

impl State {
    #[inline(always)]
    fn tick(&mut self, c: &Biquad, x: f32) -> f32 {
        let y = c.b0 * x + self.z1;
        self.z1 = c.b1 * x - c.a1 * y + self.z2;
        self.z2 = c.b2 * x - c.a2 * y;
        y
    }

    /// Flush denormal-range state once per buffer (cheap guard for silent tails).
    #[inline]
    fn flush_tiny(&mut self) {
        if self.z1.abs() < 1e-15 {
            self.z1 = 0.0;
        }
        if self.z2.abs() < 1e-15 {
            self.z2 = 0.0;
        }
    }
}

/// 10-band peaking equaliser with preamp over interleaved frames.
pub struct Equalizer {
    sample_rate: f32,
    channels: usize,
    coeffs: [Biquad; EQ_BANDS],
    /// Indices of non-flat bands, in order.
    active: [u8; EQ_BANDS],
    n_active: usize,
    state: [[State; EQ_BANDS]; MAX_CHANNELS],
    preamp: f32,
    enabled: bool,
}

impl Equalizer {
    pub fn new(sample_rate: u32, channels: usize) -> Self {
        Self {
            sample_rate: sample_rate.max(1) as f32,
            channels: channels.max(1),
            coeffs: [Biquad::IDENTITY; EQ_BANDS],
            active: [0; EQ_BANDS],
            n_active: 0,
            state: [[State::default(); EQ_BANDS]; MAX_CHANNELS],
            preamp: 1.0,
            enabled: false,
        }
    }

    /// Update settings; recomputes coefficients (no allocation).
    pub fn set(&mut self, eq: &EqSettings) {
        let was_active = self.active_mask();
        self.enabled = eq.enabled;
        let preamp_db = finite_or_zero(eq.preamp_db).clamp(-EQ_MAX_DB, EQ_MAX_DB);
        self.preamp = if preamp_db.abs() < FLAT_DB {
            1.0
        } else {
            db_to_gain(preamp_db)
        };
        self.n_active = 0;
        for (i, (&freq, &db)) in EQ_FREQS.iter().zip(eq.bands_db.iter()).enumerate() {
            let db = finite_or_zero(db).clamp(-EQ_MAX_DB, EQ_MAX_DB);
            if db.abs() < FLAT_DB {
                self.coeffs[i] = Biquad::IDENTITY;
                continue;
            }
            self.coeffs[i] = Biquad::peaking(self.sample_rate, freq, BAND_Q, db);
            self.active[self.n_active] = i as u8;
            self.n_active += 1;
        }
        // Bands that just became active start from silence, not stale state.
        let newly = self.active_mask() & !was_active;
        if newly != 0 || !self.enabled {
            for ch in self.state.iter_mut() {
                for (b, s) in ch.iter_mut().enumerate() {
                    if newly & (1 << b) != 0 || !self.enabled {
                        *s = State::default();
                    }
                }
            }
        }
    }

    fn active_mask(&self) -> u32 {
        if !self.enabled {
            return 0;
        }
        self.active[..self.n_active]
            .iter()
            .fold(0, |m, &b| m | (1 << b))
    }

    /// True when processing would leave samples untouched.
    pub fn is_bypassed(&self) -> bool {
        !self.enabled || (self.n_active == 0 && self.preamp == 1.0)
    }

    pub fn process(&mut self, buf: &mut [f32]) {
        if self.is_bypassed() {
            return;
        }
        let ch = self.channels;
        let filtered = ch.min(MAX_CHANNELS);
        let active = &self.active[..self.n_active];
        for frame in buf.chunks_exact_mut(ch) {
            for (c, x) in frame.iter_mut().enumerate() {
                let mut y = *x * self.preamp;
                if c < filtered {
                    let st = &mut self.state[c];
                    for &b in active {
                        let b = b as usize;
                        y = st[b].tick(&self.coeffs[b], y);
                    }
                }
                *x = y;
            }
        }
        for st in self.state[..filtered].iter_mut() {
            for &b in active {
                st[b as usize].flush_tiny();
            }
        }
    }
}

fn finite_or_zero(x: f32) -> f32 {
    if x.is_finite() {
        x
    } else {
        0.0
    }
}

/// Lock-free parameter block shared between the control side and the audio thread.
pub struct DspParams {
    volume: AtomicU32,
    balance: AtomicU32,
    eq_gen: AtomicU64,
    eq_enabled: AtomicBool,
    eq_auto: AtomicBool,
    eq_preamp: AtomicU32,
    eq_bands: [AtomicU32; EQ_BANDS],
    rg_mode: AtomicU8,
}

impl Default for DspParams {
    fn default() -> Self {
        Self {
            volume: AtomicU32::new(1f32.to_bits()),
            balance: AtomicU32::new(0f32.to_bits()),
            eq_gen: AtomicU64::new(0),
            eq_enabled: AtomicBool::new(false),
            eq_auto: AtomicBool::new(false),
            eq_preamp: AtomicU32::new(0f32.to_bits()),
            eq_bands: std::array::from_fn(|_| AtomicU32::new(0f32.to_bits())),
            rg_mode: AtomicU8::new(0),
        }
    }
}

impl DspParams {
    pub fn set_volume(&self, v: f32) {
        if v.is_finite() {
            self.volume
                .store(v.clamp(0.0, 1.0).to_bits(), Ordering::Relaxed);
        }
    }

    pub fn volume(&self) -> f32 {
        f32::from_bits(self.volume.load(Ordering::Relaxed))
    }

    pub fn set_balance(&self, b: f32) {
        if b.is_finite() {
            self.balance
                .store(b.clamp(-1.0, 1.0).to_bits(), Ordering::Relaxed);
        }
    }

    pub fn balance(&self) -> f32 {
        f32::from_bits(self.balance.load(Ordering::Relaxed))
    }

    pub fn set_eq(&self, eq: &EqSettings) {
        self.eq_enabled.store(eq.enabled, Ordering::Relaxed);
        self.eq_auto.store(eq.auto, Ordering::Relaxed);
        self.eq_preamp
            .store(eq.preamp_db.to_bits(), Ordering::Relaxed);
        for (slot, db) in self.eq_bands.iter().zip(eq.bands_db) {
            slot.store(db.to_bits(), Ordering::Relaxed);
        }
        self.eq_gen.fetch_add(1, Ordering::Release);
    }

    pub fn eq_generation(&self) -> u64 {
        self.eq_gen.load(Ordering::Acquire)
    }

    pub fn eq(&self) -> EqSettings {
        EqSettings {
            enabled: self.eq_enabled.load(Ordering::Relaxed),
            auto: self.eq_auto.load(Ordering::Relaxed),
            preamp_db: f32::from_bits(self.eq_preamp.load(Ordering::Relaxed)),
            bands_db: std::array::from_fn(|i| {
                f32::from_bits(self.eq_bands[i].load(Ordering::Relaxed))
            }),
        }
    }

    pub fn set_replaygain(&self, m: ReplayGainMode) {
        let v = match m {
            ReplayGainMode::Off => 0,
            ReplayGainMode::Track => 1,
            ReplayGainMode::Album => 2,
        };
        self.rg_mode.store(v, Ordering::Relaxed);
    }

    pub fn replaygain(&self) -> ReplayGainMode {
        match self.rg_mode.load(Ordering::Relaxed) {
            1 => ReplayGainMode::Track,
            2 => ReplayGainMode::Album,
            _ => ReplayGainMode::Off,
        }
    }
}

/// The per-output DSP chain run on the audio thread:
/// pre-gain (ReplayGain) → preamp + EQ → tap → balance → volume → clamp.
pub struct Chain {
    channels: usize,
    eq: Equalizer,
    eq_gen: Option<u64>,
    /// Gains applied at the end of the previous buffer (per output channel role).
    cur: [f32; 3],
}

impl Chain {
    pub fn new(sample_rate: u32, channels: usize) -> Self {
        Self {
            channels: channels.max(1),
            eq: Equalizer::new(sample_rate, channels),
            eq_gen: None,
            cur: [f32::NAN; 3],
        }
    }

    /// Pick up parameter changes; call once per buffer.
    pub fn sync(&mut self, p: &DspParams) {
        let g = p.eq_generation();
        if self.eq_gen != Some(g) {
            self.eq_gen = Some(g);
            self.eq.set(&p.eq());
        }
    }

    /// Pre-volume stage for one contiguous segment of a single track.
    pub fn process(&mut self, buf: &mut [f32], pre_gain: f32, tap: &Tap) {
        if pre_gain != 1.0 {
            for s in buf.iter_mut() {
                *s *= pre_gain;
            }
        }
        self.eq.process(buf);
        for &s in buf.iter() {
            tap.push(s);
        }
    }

    /// Balance + volume over the whole output buffer, ramped from the previous
    /// buffer's gains to avoid zipper noise, then clamped to [-1, 1].
    pub fn output(&mut self, buf: &mut [f32], p: &DspParams) {
        let vol = volume_gain(p.volume());
        let (bl, br) = balance_gains(p.balance());
        let target = [vol * bl, vol * br, vol];
        if self.cur[0].is_nan() {
            self.cur = target;
        }
        let ch = self.channels;
        let frames = buf.len() / ch;
        if self.cur == target {
            if target == [1.0; 3] {
                for s in buf.iter_mut() {
                    *s = s.clamp(-1.0, 1.0);
                }
                return;
            }
            for frame in buf.chunks_exact_mut(ch) {
                for (c, s) in frame.iter_mut().enumerate() {
                    *s = (*s * target[role(c, ch)]).clamp(-1.0, 1.0);
                }
            }
            return;
        }
        let start = self.cur;
        let inv = 1.0 / frames.max(1) as f32;
        for (i, frame) in buf.chunks_exact_mut(ch).enumerate() {
            let t = (i + 1) as f32 * inv;
            for (c, s) in frame.iter_mut().enumerate() {
                let r = role(c, ch);
                let g = start[r] + (target[r] - start[r]) * t;
                *s = (*s * g).clamp(-1.0, 1.0);
            }
        }
        self.cur = target;
    }
}

/// Gain slot for channel `c`: 0 = left, 1 = right, 2 = other (volume only).
/// Mono output uses the plain volume.
#[inline(always)]
fn role(c: usize, channels: usize) -> usize {
    match (channels, c) {
        (1, _) => 2,
        (_, 0) => 0,
        (_, 1) => 1,
        _ => 2,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sine(freq: f32, rate: f32, frames: usize, channels: usize, amp: f32) -> Vec<f32> {
        let mut v = Vec::with_capacity(frames * channels);
        for i in 0..frames {
            let s = amp * (std::f32::consts::TAU * freq * i as f32 / rate).sin();
            for _ in 0..channels {
                v.push(s);
            }
        }
        v
    }

    fn rms(x: &[f32]) -> f32 {
        (x.iter().map(|s| s * s).sum::<f32>() / x.len() as f32).sqrt()
    }

    fn eq_with(band: usize, db: f32) -> EqSettings {
        let mut eq = EqSettings {
            enabled: true,
            ..Default::default()
        };
        eq.bands_db[band] = db;
        eq
    }

    #[test]
    fn flat_eq_is_bit_identical_passthrough() {
        let input = sine(440.0, 48_000.0, 4096, 2, 0.8);
        for settings in [
            EqSettings::default(),
            EqSettings {
                enabled: true,
                ..Default::default()
            },
            // Disabled EQ ignores non-flat bands and preamp.
            EqSettings {
                enabled: false,
                preamp_db: 6.0,
                bands_db: [12.0; EQ_BANDS],
                ..Default::default()
            },
        ] {
            let mut eq = Equalizer::new(48_000, 2);
            eq.set(&settings);
            assert!(eq.is_bypassed());
            let mut buf = input.clone();
            eq.process(&mut buf);
            assert!(buf
                .iter()
                .zip(&input)
                .all(|(a, b)| a.to_bits() == b.to_bits()));
        }
    }

    #[test]
    fn chain_at_unity_is_bit_identical() {
        let input = sine(1000.0, 44_100.0, 2048, 2, 0.9);
        let params = DspParams::default();
        params.set_eq(&EqSettings {
            enabled: true,
            ..Default::default()
        });
        let tap = Tap::new();
        let mut chain = Chain::new(44_100, 2);
        chain.sync(&params);
        let mut buf = input.clone();
        chain.process(&mut buf, 1.0, &tap);
        chain.output(&mut buf, &params);
        assert!(buf
            .iter()
            .zip(&input)
            .all(|(a, b)| a.to_bits() == b.to_bits()));
        let mut snap = [0.0f32; 4];
        assert_eq!(tap.snapshot_mono(&mut snap), 4);
        assert_eq!(snap[3], input[input.len() - 1]);
    }

    #[test]
    fn plus_six_db_band_boosts_its_centre_by_six_db() {
        let rate = 48_000.0;
        for band in [0, 4, 7] {
            let f = EQ_FREQS[band];
            let input = sine(f, rate, 48_000, 1, 0.25);
            let mut eq = Equalizer::new(48_000, 1);
            eq.set(&eq_with(band, 6.0));
            let mut buf = input.clone();
            eq.process(&mut buf);
            // Skip the transient; compare steady-state RMS.
            let gain_db = 20.0 * (rms(&buf[24_000..]) / rms(&input[24_000..])).log10();
            assert!((gain_db - 6.0).abs() < 0.1, "band {band}: {gain_db} dB");
        }
    }

    #[test]
    fn boost_far_from_centre_is_small() {
        let input = sine(10_000.0, 48_000.0, 24_000, 1, 0.25);
        let mut eq = Equalizer::new(48_000, 1);
        eq.set(&eq_with(0, 12.0)); // 60 Hz band
        let mut buf = input.clone();
        eq.process(&mut buf);
        let gain_db = 20.0 * (rms(&buf[12_000..]) / rms(&input[12_000..])).log10();
        assert!(gain_db.abs() < 0.1, "{gain_db} dB");
    }

    #[test]
    fn preamp_scales_signal() {
        let input = sine(500.0, 44_100.0, 4096, 2, 0.1);
        let mut eq = Equalizer::new(44_100, 2);
        eq.set(&EqSettings {
            enabled: true,
            preamp_db: -6.0,
            ..Default::default()
        });
        let mut buf = input.clone();
        eq.process(&mut buf);
        let ratio = rms(&buf) / rms(&input);
        assert!((ratio - db_to_gain(-6.0)).abs() < 1e-4);
    }

    #[test]
    fn extreme_gains_stay_stable() {
        for rate in [8_000u32, 22_050, 44_100, 48_000, 96_000, 192_000] {
            let mut eq = Equalizer::new(rate, 2);
            for db in [EQ_MAX_DB, -EQ_MAX_DB, 1e6, f32::NAN] {
                eq.set(&EqSettings {
                    enabled: true,
                    auto: false,
                    preamp_db: db,
                    bands_db: [db; EQ_BANDS],
                });
                // Impulse followed by a long silence; output must decay, never blow up.
                let mut buf = vec![0.0f32; 2 * rate as usize];
                buf[0] = 1.0;
                buf[1] = 1.0;
                eq.process(&mut buf);
                assert!(buf.iter().all(|s| s.is_finite()), "rate {rate} db {db}");
                let tail = &buf[buf.len() - 2000..];
                assert!(tail.iter().all(|s| s.abs() < 1e-3), "rate {rate} db {db}");
            }
        }
    }

    #[test]
    fn coefficient_poles_inside_unit_circle() {
        for rate in [22_050.0f32, 44_100.0, 48_000.0] {
            for &f in &EQ_FREQS {
                for db in [-12.0, -3.0, 3.0, 12.0] {
                    let c = Biquad::peaking(rate, f, BAND_Q, db);
                    // Stability triangle for a2 z^-2 + a1 z^-1 + 1.
                    assert!(
                        c.a2.abs() < 1.0 && c.a1.abs() < 1.0 + c.a2,
                        "{rate} {f} {db}"
                    );
                }
            }
        }
    }

    #[test]
    fn volume_curve() {
        assert_eq!(volume_gain(0.0), 0.0);
        assert_eq!(volume_gain(1.0), 1.0);
        assert_eq!(volume_gain(0.5), 0.125);
        assert_eq!(volume_gain(2.0), 1.0);
        assert_eq!(volume_gain(-1.0), 0.0);
        let mut prev = -1.0;
        for i in 0..=100 {
            let g = volume_gain(i as f32 / 100.0);
            assert!(g > prev);
            prev = g;
        }
    }

    #[test]
    fn balance_math() {
        assert_eq!(balance_gains(0.0), (1.0, 1.0));
        assert_eq!(balance_gains(1.0), (0.0, 1.0));
        assert_eq!(balance_gains(-1.0), (1.0, 0.0));
        assert_eq!(balance_gains(-0.5), (1.0, 0.5));
        assert_eq!(balance_gains(0.25), (0.75, 1.0));
        assert_eq!(balance_gains(5.0), (0.0, 1.0));
    }

    #[test]
    fn output_applies_balance_volume_with_ramp_and_clamp() {
        let params = DspParams::default();
        let mut chain = Chain::new(48_000, 2);
        params.set_volume(0.5);
        params.set_balance(0.5);
        let mut buf = vec![1.0f32; 8];
        chain.output(&mut buf, &params);
        // First buffer jumps straight to target.
        assert_eq!(
            buf,
            [0.0625, 0.125, 0.0625, 0.125, 0.0625, 0.125, 0.0625, 0.125]
        );

        // Change volume: ramps linearly over the next buffer, reaching target.
        params.set_volume(1.0);
        params.set_balance(0.0);
        let mut buf = vec![1.0f32; 8];
        chain.output(&mut buf, &params);
        assert!(buf[0] > 0.0625 && buf[0] < 1.0);
        assert!(buf[2] > buf[0]);
        assert_eq!(&buf[6..], &[1.0, 1.0]);

        // Clamp to [-1, 1].
        let mut buf = vec![3.0f32, -3.0];
        chain.output(&mut buf, &params);
        assert_eq!(buf, [1.0, -1.0]);
    }

    #[test]
    fn params_reject_non_finite() {
        let p = DspParams::default();
        p.set_volume(f32::NAN);
        p.set_balance(f32::INFINITY);
        assert_eq!(p.volume(), 1.0);
        assert_eq!(p.balance(), 0.0);
        p.set_volume(3.0);
        assert_eq!(p.volume(), 1.0);
    }
}
