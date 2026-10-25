//! Format conversion run on decoder threads (never on the audio callback):
//! channel remapping and band-limited sample-rate conversion.

use std::f64::consts::PI;

/// Stopband attenuation the filter is designed for, in dB.
const ATTEN_DB: f64 = 90.0;
/// Passband edge as a fraction of the lower Nyquist: 20 kHz at 44.1 kHz. The
/// stopband starts at that Nyquist, so nothing aliases.
const PASS: f64 = 20_000.0 / 22_050.0;
/// Taps per phase are a multiple of this so the dot product splits into
/// independent SIMD accumulators.
const LANES: usize = 16;
/// Cap on taps per phase; extreme downsampling ratios get a wider transition.
const MAX_TAPS: usize = 1024;
/// Ratios with at most this many output phases get one exact coefficient row
/// per phase (44.1 -> 48 kHz has 160, 8 -> 44.1 kHz 441).
const MAX_EXACT_PHASES: u64 = 1024;
/// Phase rows for any other ratio; each output blends two adjacent rows.
const FINE_PHASES: usize = 256;

/// Polyphase windowed-sinc (Kaiser) resampler for interleaved f32 frames.
///
/// The kernel is precomputed per output phase, so an output sample costs one
/// contiguous dot product per channel over planar history. Flat (+-0.001 dB)
/// to 20 kHz-equivalent with the stopband (>= 90 dB) starting at the lower
/// Nyquist, for any ratio up to `MAX_TAPS`.
pub struct Resampler {
    channels: usize,
    /// Taps per phase row (multiple of `LANES`), centred on `half`.
    taps: usize,
    half: usize,
    /// Step of `in/out` input frames per output frame as an exact fraction
    /// (`step_int + step_num / den`), so output never depends on chunking.
    step_int: usize,
    step_num: u64,
    den: u64,
    /// Next output frame position in `hist`: frame `base` + `num / den`.
    base: usize,
    num: u64,
    /// Planar history, one buffer per channel.
    hist: Vec<Vec<f32>>,
    /// Rows of `taps` coefficients for fractional positions `p / den`
    /// (exact) or `p / FINE_PHASES` (fine), each normalised to unity DC gain.
    table: Vec<f32>,
    fine: bool,
}

fn gcd(mut a: u64, mut b: u64) -> u64 {
    while b != 0 {
        (a, b) = (b, a % b);
    }
    a
}

/// Modified Bessel function of the first kind, order 0 (Kaiser window).
fn bessel_i0(x: f64) -> f64 {
    let q = x * x / 4.0;
    let (mut sum, mut term, mut k) = (1.0, 1.0, 1.0);
    while term > sum * 1e-17 {
        term *= q / (k * k);
        sum += term;
        k += 1.0;
    }
    sum
}

/// `a . b` over whole `LANES` chunks with one accumulator per lane, which
/// the compiler turns into SIMD multiply-adds without reassociating floats.
#[inline(always)]
fn dot(a: &[f32], b: &[f32]) -> f32 {
    let mut acc = [0.0f32; LANES];
    for (x, y) in a
        .as_chunks::<LANES>()
        .0
        .iter()
        .zip(b.as_chunks::<LANES>().0)
    {
        for ((s, x), y) in acc.iter_mut().zip(x).zip(y) {
            *s += x * y;
        }
    }
    acc.iter().sum()
}

impl Resampler {
    pub fn new(in_rate: u32, out_rate: u32, channels: usize) -> Self {
        let channels = channels.max(1);
        let (in_r, out_r) = (u64::from(in_rate.max(1)), u64::from(out_rate.max(1)));
        let g = gcd(in_r, out_r);
        let (step, den) = (in_r / g, out_r / g);
        // Everything below is in cycles per input sample.
        let stop = in_r.min(out_r) as f64 / 2.0 / in_r as f64;
        let want = (1.0 - PASS) * stop;
        // Kaiser's estimate of the length for ATTEN_DB over that transition.
        let need = ((ATTEN_DB - 7.95) / (14.36 * want)).ceil() as usize + 1;
        let taps = need.next_multiple_of(LANES).clamp(LANES, MAX_TAPS);
        // Rounding up buys extra attenuation; the cap widens the transition.
        let width = want.max((ATTEN_DB - 7.95) / (14.36 * (taps - 1) as f64));
        let atten = 14.36 * width * (taps - 1) as f64 + 7.95;
        let beta = 0.1102 * (atten - 8.7);
        let fc = stop - width / 2.0;
        let half = taps / 2;
        let fine = den > MAX_EXACT_PHASES;
        let (rows, per) = if fine {
            (FINE_PHASES + 1, FINE_PHASES as f64)
        } else {
            (den as usize, den as f64)
        };
        let i0_beta = bessel_i0(beta);
        let mut table = Vec::with_capacity(rows * taps);
        let mut row = vec![0.0f64; taps];
        for p in 0..rows {
            let frac = p as f64 / per;
            for (j, w) in row.iter_mut().enumerate() {
                // Distance from the output position to tap j, in input frames.
                let d = frac + (half - 1) as f64 - j as f64;
                let r = d / half as f64;
                *w = if r.abs() >= 1.0 {
                    0.0
                } else {
                    let t = 2.0 * fc * d;
                    let sinc = if t == 0.0 {
                        1.0
                    } else {
                        (PI * t).sin() / (PI * t)
                    };
                    sinc * bessel_i0(beta * (1.0 - r * r).sqrt()) / i0_beta
                };
            }
            let sum: f64 = row.iter().sum();
            table.extend(row.iter().map(|w| (w / sum) as f32));
        }
        let mut r = Self {
            channels,
            taps,
            half,
            step_int: (step / den) as usize,
            step_num: step % den,
            den,
            base: 0,
            num: 0,
            hist: vec![Vec::new(); channels],
            table,
            fine,
        };
        r.reset();
        r
    }

    /// Forget buffered input (after a seek).
    pub fn reset(&mut self) {
        // Prime with zeros so the first output frame lines up with input frame 0.
        for h in &mut self.hist {
            h.clear();
            h.resize(self.half - 1, 0.0);
        }
        self.base = self.half - 1;
        self.num = 0;
    }

    /// Convert `input` (whole frames) and append the produced frames to `out`.
    pub fn process(&mut self, input: &[f32], out: &mut Vec<f32>) {
        let ch = self.channels;
        for (c, h) in self.hist.iter_mut().enumerate() {
            h.extend(input.chunks_exact(ch).map(|f| f[c]));
        }
        self.render(out);
    }

    /// Flush the tail at end of stream.
    pub fn finish(&mut self, out: &mut Vec<f32>) {
        for h in &mut self.hist {
            h.resize(h.len() + self.half, 0.0);
        }
        self.render(out);
    }

    /// Emit every output frame whose taps are all in `hist`.
    fn render(&mut self, out: &mut Vec<f32>) {
        let (taps, half, den) = (self.taps, self.half, self.den);
        let frames = self.hist[0].len();
        let ready = frames.saturating_sub(self.base + half) as u64;
        let step = self.step_int as u64 * den + self.step_num;
        out.reserve(((ready * den).div_ceil(step) as usize + 1) * self.channels);
        while self.base + half < frames {
            let first = self.base + 1 - half;
            if self.fine {
                let x = self.num * FINE_PHASES as u64;
                let p = (x / den) as usize;
                let t = (x % den) as f32 / den as f32;
                let a = &self.table[p * taps..(p + 1) * taps];
                let b = &self.table[(p + 1) * taps..(p + 2) * taps];
                for h in &self.hist {
                    let w = &h[first..first + taps];
                    let (ya, yb) = (dot(a, w), dot(b, w));
                    out.push(ya + (yb - ya) * t);
                }
            } else {
                let p = self.num as usize;
                let row = &self.table[p * taps..(p + 1) * taps];
                for h in &self.hist {
                    out.push(dot(row, &h[first..first + taps]));
                }
            }
            self.base += self.step_int;
            self.num += self.step_num;
            if self.num >= den {
                self.num -= den;
                self.base += 1;
            }
        }
        // Drop input frames no output will need again.
        let drop = (self.base + 1).saturating_sub(half).min(frames);
        if drop > 0 {
            for h in &mut self.hist {
                h.drain(..drop);
            }
            self.base -= drop;
        }
    }

    #[cfg(test)]
    fn row(&self, p: usize) -> &[f32] {
        &self.table[p * self.taps..(p + 1) * self.taps]
    }
}

/// Remap interleaved `input` from `in_ch` to `out_ch` channels, appending to `out`.
/// Mono is duplicated to both front channels; 5.1+ is downmixed to stereo with
/// ITU coefficients; otherwise channels are matched by position and extras dropped.
pub fn remix(input: &[f32], in_ch: usize, out_ch: usize, out: &mut Vec<f32>) {
    let in_ch = in_ch.max(1);
    let out_ch = out_ch.max(1);
    if in_ch == out_ch {
        out.extend_from_slice(input);
        return;
    }
    out.reserve(input.len() / in_ch * out_ch);
    for frame in input.chunks_exact(in_ch) {
        let (l, r) = match in_ch {
            1 => (frame[0], frame[0]),
            2..=5 => (frame[0], frame[1]),
            // FL FR FC LFE RL RR [SL SR]
            _ => {
                const C: f32 = std::f32::consts::FRAC_1_SQRT_2;
                const NORM: f32 = 1.0 / (1.0 + 2.0 * C);
                (
                    (frame[0] + C * frame[2] + C * frame[4]) * NORM,
                    (frame[1] + C * frame[2] + C * frame[5]) * NORM,
                )
            }
        };
        match out_ch {
            1 => out.push(if in_ch == 1 { l } else { 0.5 * (l + r) }),
            _ => {
                out.push(l);
                out.push(r);
                for _ in 2..out_ch {
                    out.push(0.0);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sine(freq: f64, rate: f64, frames: usize, channels: usize) -> Vec<f32> {
        let mut v = Vec::new();
        for i in 0..frames {
            let s = (0.5 * (2.0 * PI * freq * i as f64 / rate).sin()) as f32;
            for _ in 0..channels {
                v.push(s);
            }
        }
        v
    }

    fn run(r: &mut Resampler, input: &[f32], chunk: usize) -> Vec<f32> {
        let mut out = Vec::new();
        for c in input.chunks(chunk) {
            r.process(c, &mut out);
        }
        r.finish(&mut out);
        out
    }

    #[test]
    fn output_length_tracks_rate_ratio() {
        for (inr, outr) in [
            (44_100, 48_000),
            (48_000, 44_100),
            (22_050, 48_000),
            (96_000, 44_100),
        ] {
            let frames = inr as usize; // one second
            let input = sine(440.0, inr as f64, frames, 2);
            let mut r = Resampler::new(inr, outr, 2);
            let out = run(&mut r, &input, 1000);
            let got = out.len() / 2;
            let want = outr as usize;
            assert!(got.abs_diff(want) <= 2, "{inr}->{outr}: {got} vs {want}");
        }
    }

    #[test]
    fn dc_passes_at_unity_gain() {
        let input = vec![0.25f32; 2 * 10_000];
        let mut r = Resampler::new(44_100, 48_000, 2);
        let mut out = Vec::new();
        r.process(&input, &mut out);
        // Skip the zero-primed start.
        for s in &out[2 * 100..] {
            assert!((s - 0.25).abs() < 1e-4, "{s}");
        }
    }

    #[test]
    fn sine_keeps_amplitude_and_frequency() {
        let (inr, outr) = (44_100u32, 48_000u32);
        let input = sine(1_000.0, inr as f64, inr as usize, 1);
        let mut r = Resampler::new(inr, outr, 1);
        let out = run(&mut r, &input, 4096);
        let body = &out[4_800..43_200];
        let peak = body.iter().fold(0.0f32, |m, s| m.max(s.abs()));
        assert!((peak - 0.5).abs() < 0.005, "peak {peak}");
        // Compare against the ideal resampled sine (no time shift expected).
        let max_err = body
            .iter()
            .enumerate()
            .map(|(i, s)| {
                let t = (i + 4_800) as f64 / outr as f64;
                (s - (0.5 * (2.0 * PI * 1_000.0 * t).sin()) as f32).abs()
            })
            .fold(0.0f32, f32::max);
        assert!(max_err < 0.003, "max err {max_err}");
    }

    #[test]
    fn downsampling_rejects_content_above_new_nyquist() {
        // 23 kHz at 96 kHz → 44.1 kHz must be strongly attenuated, not aliased.
        let input = sine(23_000.0, 96_000.0, 96_000, 1);
        let mut r = Resampler::new(96_000, 44_100, 1);
        let out = run(&mut r, &input, 4096);
        let body = &out[4_410..39_690];
        let peak = body.iter().fold(0.0f32, |m, s| m.max(s.abs()));
        assert!(peak < 0.5 * 0.01, "alias peak {peak}");
    }

    #[test]
    fn chunking_does_not_change_output() {
        // 48 -> 44.1 kHz uses exact phases, 44.1 -> 47.999 kHz the fine table.
        for (inr, outr) in [(48_000, 44_100), (44_100, 47_999)] {
            let input = sine(3_000.0, f64::from(inr), 20_000, 2);
            let a = run(&mut Resampler::new(inr, outr, 2), &input, 20_000 * 2);
            let b = run(&mut Resampler::new(inr, outr, 2), &input, 2 * 37);
            assert_eq!(a.len(), b.len());
            assert!(a.iter().zip(&b).all(|(x, y)| (x - y).abs() < 1e-6));
        }
    }

    #[test]
    fn phase_rows_are_normalised_and_symmetric() {
        let r = Resampler::new(44_100, 48_000, 1);
        assert!(!r.fine);
        assert_eq!(r.taps % LANES, 0);
        let den = r.den as usize;
        for p in 0..den {
            let row = r.row(p);
            let sum: f32 = row.iter().sum();
            assert!((sum - 1.0).abs() < 1e-5, "row {p} sums to {sum}");
            // Distance d at position p/den is -d at (den - p)/den with the
            // taps reversed; position 0 mirrors itself one tap later.
            for (j, w) in row.iter().enumerate() {
                let k = r.taps - 1 - j;
                let m = match (p, k.checked_sub(1)) {
                    (0, Some(k)) => row[k],
                    (0, None) => continue,
                    _ => r.row(den - p)[k],
                };
                assert!((w - m).abs() < 1e-6, "row {p} tap {j}: {w} vs {m}");
            }
        }
        // Position 0 is (nearly) the input frame itself.
        assert!(r.row(0)[r.half - 1] > 0.9);
        assert!(Resampler::new(44_100, 47_999, 1).fine);
    }

    #[test]
    fn remix_layouts() {
        let mut out = Vec::new();
        remix(&[0.5, -0.5], 1, 2, &mut out);
        assert_eq!(out, [0.5, 0.5, -0.5, -0.5]);

        out.clear();
        remix(&[1.0, 0.0, 0.5, 0.5], 2, 1, &mut out);
        assert_eq!(out, [0.5, 0.5]);

        out.clear();
        remix(&[0.1, 0.2], 2, 4, &mut out);
        assert_eq!(out, [0.1, 0.2, 0.0, 0.0]);

        out.clear();
        // 5.1 with only centre: equal on both sides, normalised.
        remix(&[0.0, 0.0, 1.0, 0.0, 0.0, 0.0], 6, 2, &mut out);
        assert!((out[0] - out[1]).abs() < 1e-7);
        assert!(out[0] > 0.2 && out[0] < 0.5);

        out.clear();
        remix(&[0.3, 0.4], 2, 2, &mut out);
        assert_eq!(out, [0.3, 0.4]);
    }

    /// Least-squares fit of a sinusoid at `freq` Hz: (amplitude, residual rms).
    fn fit(y: &[f32], freq: f64, rate: f64) -> (f64, f64) {
        let w = 2.0 * PI * freq / rate;
        let (mut ss, mut cc, mut sc, mut ys, mut yc) = (0.0, 0.0, 0.0, 0.0, 0.0);
        for (i, &v) in y.iter().enumerate() {
            let (s, c) = (w * i as f64).sin_cos();
            let v = f64::from(v);
            ss += s * s;
            cc += c * c;
            sc += s * c;
            ys += v * s;
            yc += v * c;
        }
        let det = ss * cc - sc * sc;
        let a = (ys * cc - yc * sc) / det;
        let b = (yc * ss - ys * sc) / det;
        let res: f64 = y
            .iter()
            .enumerate()
            .map(|(i, &v)| {
                let (s, c) = (w * i as f64).sin_cos();
                (f64::from(v) - a * s - b * c).powi(2)
            })
            .sum();
        ((a * a + b * b).sqrt(), (res / y.len() as f64).sqrt())
    }

    fn db(x: f64) -> f64 {
        20.0 * x.max(1e-12).log10()
    }

    /// Resample a 0.5-amplitude tone; returns (gain dB, spurious dB) where
    /// spurious is everything in the output that is not the tone (images,
    /// aliases, noise) relative to the input tone. When `freq` is above the
    /// output Nyquist the whole output counts as spurious.
    fn tone_response(inr: u32, outr: u32, freq: f64) -> (f64, f64) {
        let input = sine(freq, f64::from(inr), inr as usize / 2, 1);
        let mut r = Resampler::new(inr, outr, 1);
        let out = run(&mut r, &input, 4096);
        let margin = outr as usize / 20;
        let body = &out[margin..out.len() - margin];
        let input_rms = 0.5 / 2f64.sqrt();
        if freq >= f64::from(outr) / 2.0 {
            let rms = (body.iter().map(|&s| f64::from(s).powi(2)).sum::<f64>() / body.len() as f64)
                .sqrt();
            return (db(0.0), db(rms / input_rms));
        }
        let (amp, res) = fit(body, freq, f64::from(outr));
        (db(amp / 0.5), db(res / input_rms))
    }

    /// Quality over a tone sweep of `steps` tones up to the input Nyquist:
    /// (passband min dB and max dB up to 20 kHz-equivalent, gain at that
    /// edge, worst stopband/spurious dB anywhere).
    fn quality(inr: u32, outr: u32, steps: u32) -> (f64, f64, f64, f64) {
        let nyq_lo = f64::from(inr.min(outr)) / 2.0;
        let nyq_in = f64::from(inr) / 2.0;
        let edge = nyq_lo * 20_000.0 / 22_050.0;
        let (mut lo, mut hi, mut worst) = (f64::MAX, f64::MIN, f64::MIN);
        let mut f = 20.0;
        while f < nyq_in * 0.995 {
            let (gain, spur) = tone_response(inr, outr, f);
            if f <= edge {
                lo = lo.min(gain);
                hi = hi.max(gain);
            }
            worst = worst.max(spur);
            f += nyq_in / f64::from(steps);
        }
        let (edge_gain, _) = tone_response(inr, outr, edge);
        (lo, hi, edge_gain, worst)
    }

    #[test]
    fn flat_passband_and_deep_stopband() {
        for (inr, outr) in [(44_100, 48_000), (48_000, 44_100), (44_100, 47_999)] {
            let (lo, hi, edge, worst) = quality(inr, outr, 29);
            let msg =
                format!("{inr}->{outr}: passband {lo}..{hi} dB, edge {edge} dB, stop {worst} dB");
            // Flat within 0.002 dB to 20 kHz-equivalent; >= 90 dB stopband.
            assert!(lo > -0.002 && hi < 0.002, "{msg}");
            assert!(edge > -0.002, "{msg}");
            assert!(worst < -90.0, "{msg}");
        }
    }

    #[test]
    #[ignore = "report: cargo test --release -- --ignored --nocapture bench_"]
    fn bench_resampler_quality() {
        println!("bench resampler quality (tone sweep up to the input Nyquist)");
        for (inr, outr) in [
            (44_100, 48_000),
            (48_000, 44_100),
            (96_000, 44_100),
            (22_050, 48_000),
            (44_100, 47_999),
        ] {
            let (lo, hi, edge, worst) = quality(inr, outr, 97);
            println!(
                "  {inr:>6} -> {outr:>6}: passband {lo:+.4}..{hi:+.4} dB, \
                 gain at 20 kHz-equiv {edge:+.4} dB, stopband/spurious {worst:.1} dB"
            );
        }
    }

    #[inline(always)]
    fn dot_n<const N: usize>(a: &[f32], b: &[f32]) -> f32 {
        let mut acc = [0.0f32; N];
        for (x, y) in a.as_chunks::<N>().0.iter().zip(b.as_chunks::<N>().0) {
            for ((s, x), y) in acc.iter_mut().zip(x).zip(y) {
                *s += x * y;
            }
        }
        acc.iter().sum()
    }

    #[inline(always)]
    fn dot_fma<const N: usize>(a: &[f32], b: &[f32]) -> f32 {
        let mut acc = [0.0f32; N];
        for (x, y) in a.as_chunks::<N>().0.iter().zip(b.as_chunks::<N>().0) {
            for ((s, x), y) in acc.iter_mut().zip(x).zip(y) {
                *s = x.mul_add(*y, *s);
            }
        }
        acc.iter().sum()
    }

    fn time_dot(name: &str, f: impl Fn(&[f32], &[f32]) -> f32) {
        let r = Resampler::new(44_100, 48_000, 2);
        let l: Vec<f32> = (0..8192).map(|i| (i as f32 * 0.01).sin()).collect();
        let rr: Vec<f32> = (0..8192).map(|i| (i as f32 * 0.013).cos()).collect();
        let n = 48_000 * 60;
        let taps = r.taps;
        let mut best = f64::MAX;
        let mut s = 0.0f32;
        for _ in 0..5 {
            let t = std::time::Instant::now();
            for i in 0..n {
                let row = r.row(i % 160);
                let p = std::hint::black_box(i % 8000);
                s += f(row, &l[p..p + taps]) + f(row, &rr[p..p + taps]);
            }
            best = best.min(t.elapsed().as_secs_f64());
        }
        println!(
            "bench dot {name}: {:.1} ns/frame ({s})",
            best * 1e9 / n as f64
        );
    }

    #[test]
    #[ignore = "experiment"]
    fn bench_dot_variants() {
        time_dot("plain 16", dot);
        time_dot("plain 8", dot_n::<8>);
        time_dot("plain 4", dot_n::<4>);
        if cfg!(target_arch = "aarch64") {
            time_dot("fma 16", dot_fma::<16>);
            time_dot("fma 8", dot_fma::<8>);
            time_dot("fma 4", dot_fma::<4>);
        }
        time_dot("plain 16 again", dot);
        time_dot("plain 8 again", dot_n::<8>);
    }
}
