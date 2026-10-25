//! Format conversion run on decoder threads (never on the audio callback):
//! channel remapping and band-limited sample-rate conversion.

use std::f64::consts::PI;

/// Kernel half-width in input samples (64 taps total).
const HALF_TAPS: usize = 32;
/// Kernel table resolution per input sample (linear interpolation between).
const PHASES: usize = 512;

/// Windowed-sinc (Blackman) resampler for interleaved f32 frames.
///
/// The cutoff sits half a transition band below the lower of the two Nyquist
/// frequencies, so the stopband starts at Nyquist: ~-74 dB images/aliases with
/// a flat passband to ~18 kHz for 44.1 ↔ 48 kHz.
pub struct Resampler {
    channels: usize,
    /// Step of `in/out` input frames per output frame as an exact fraction
    /// (`step_int + step_num / den`), so output never depends on chunking.
    step_int: usize,
    step_num: u64,
    den: u64,
    /// Next output frame position in `hist`: frame `base` + `num / den`.
    base: usize,
    num: u64,
    hist: Vec<f32>,
    /// Kernel sampled at x = i / PHASES for i in 0..=HALF_TAPS * PHASES.
    table: Vec<f32>,
}

fn gcd(mut a: u64, mut b: u64) -> u64 {
    while b != 0 {
        (a, b) = (b, a % b);
    }
    a
}

impl Resampler {
    pub fn new(in_rate: u32, out_rate: u32, channels: usize) -> Self {
        let channels = channels.max(1);
        let (in_r, out_r) = (u64::from(in_rate.max(1)), u64::from(out_rate.max(1)));
        let g = gcd(in_r, out_r);
        let (step, den) = (in_r / g, out_r / g);
        let in_rate = in_r as f64;
        let out_rate = out_r as f64;
        let ratio = (out_rate / in_rate).min(1.0);
        // Blackman transition width ≈ 5.5 / N cycles per sample for N taps.
        let transition = 5.5 / (2 * HALF_TAPS) as f64;
        let fc = (0.5 * ratio - transition / 2.0).max(0.05);
        let table = (0..=HALF_TAPS * PHASES)
            .map(|i| {
                let x = i as f64 / PHASES as f64;
                let t = 2.0 * fc * x;
                let sinc = if t == 0.0 {
                    1.0
                } else {
                    (PI * t).sin() / (PI * t)
                };
                let w = x / HALF_TAPS as f64; // 0..=1
                let window = 0.42 + 0.5 * (PI * w).cos() + 0.08 * (2.0 * PI * w).cos();
                (2.0 * fc * sinc * window) as f32
            })
            .collect();
        let mut r = Self {
            channels,
            step_int: (step / den) as usize,
            step_num: step % den,
            den,
            base: 0,
            num: 0,
            hist: Vec::new(),
            table,
        };
        r.reset();
        r
    }

    /// Forget buffered input (after a seek).
    pub fn reset(&mut self) {
        self.hist.clear();
        // Prime with zeros so the first output frame lines up with input frame 0.
        self.hist.resize((HALF_TAPS - 1) * self.channels, 0.0);
        self.base = HALF_TAPS - 1;
        self.num = 0;
    }

    #[inline]
    fn kernel(&self, x: f64) -> f32 {
        let x = x.abs() * PHASES as f64;
        let i = x as usize;
        if i >= HALF_TAPS * PHASES {
            return 0.0;
        }
        let frac = (x - i as f64) as f32;
        let a = self.table[i];
        let b = self.table[i + 1];
        a + (b - a) * frac
    }

    /// Convert `input` (whole frames) and append the produced frames to `out`.
    pub fn process(&mut self, input: &[f32], out: &mut Vec<f32>) {
        let ch = self.channels;
        self.hist.extend_from_slice(input);
        let frames = self.hist.len() / ch;
        loop {
            let b = self.base;
            if b + HALF_TAPS >= frames {
                break;
            }
            let frac = self.num as f64 / self.den as f64;
            // Taps k = b - HALF_TAPS + 1 ..= b + HALF_TAPS; distance pos - k.
            let mut weights = [0.0f32; 2 * HALF_TAPS];
            let mut wsum = 0.0f32;
            for (j, w) in weights.iter_mut().enumerate() {
                *w = self.kernel(frac + (HALF_TAPS as f64 - 1.0 - j as f64));
                wsum += *w;
            }
            let norm = if wsum.abs() > 1e-6 { 1.0 / wsum } else { 1.0 };
            let first = (b + 1 - HALF_TAPS) * ch;
            for c in 0..ch {
                let mut acc = 0.0f32;
                for (j, w) in weights.iter().enumerate() {
                    acc += w * self.hist[first + j * ch + c];
                }
                out.push(acc * norm);
            }
            self.base += self.step_int;
            self.num += self.step_num;
            if self.num >= self.den {
                self.num -= self.den;
                self.base += 1;
            }
        }
        // Drop input frames no output will need again.
        let drop = (self.base + 1).saturating_sub(HALF_TAPS).min(frames);
        if drop > 0 {
            self.hist.drain(..drop * ch);
            self.base -= drop;
        }
    }

    /// Flush the tail at end of stream.
    pub fn finish(&mut self, out: &mut Vec<f32>) {
        let zeros = vec![0.0f32; HALF_TAPS * self.channels];
        self.process(&zeros, out);
    }

    #[cfg(test)]
    fn kernel_at(&self, x: f64) -> f32 {
        self.kernel(x)
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
        let input = sine(3_000.0, 48_000.0, 20_000, 2);
        let a = run(&mut Resampler::new(48_000, 44_100, 2), &input, 20_000 * 2);
        let b = run(&mut Resampler::new(48_000, 44_100, 2), &input, 2 * 37);
        assert_eq!(a.len(), b.len());
        assert!(a.iter().zip(&b).all(|(x, y)| (x - y).abs() < 1e-6));
    }

    #[test]
    fn kernel_is_symmetric_and_bounded() {
        let r = Resampler::new(44_100, 48_000, 1);
        assert!(r.kernel_at(0.0) > 0.8);
        assert_eq!(r.kernel_at(HALF_TAPS as f64), 0.0);
        assert_eq!(r.kernel_at(1.3), r.kernel_at(-1.3));
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
}
