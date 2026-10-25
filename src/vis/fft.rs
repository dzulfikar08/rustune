//! In-place iterative radix-2 FFT with precomputed twiddles and bit-reversal
//! table. Construction allocates; `forward` never does.

use std::f32::consts::TAU;

pub struct Fft {
    n: usize,
    /// Bit-reversed index for every position.
    rev: Box<[u32]>,
    /// `cos(2πk/n)` for `k` in `0..n/2`.
    cos: Box<[f32]>,
    /// `sin(2πk/n)` for `k` in `0..n/2`.
    sin: Box<[f32]>,
}

impl Fft {
    /// `n` must be a power of two, at least 2.
    pub fn new(n: usize) -> Self {
        assert!(
            n >= 2 && n.is_power_of_two(),
            "FFT size must be a power of two >= 2"
        );
        let bits = n.trailing_zeros();
        let rev = (0..n as u32)
            .map(|i| i.reverse_bits() >> (32 - bits))
            .collect();
        let half = n / 2;
        let cos = (0..half)
            .map(|k| (TAU * k as f32 / n as f32).cos())
            .collect();
        let sin = (0..half)
            .map(|k| (TAU * k as f32 / n as f32).sin())
            .collect();
        Self { n, rev, cos, sin }
    }

    pub fn size(&self) -> usize {
        self.n
    }

    /// Forward transform (`e^{-2πi kn/N}` kernel, unnormalized) in place.
    /// `re` and `im` must both have length `size()`.
    pub fn forward(&self, re: &mut [f32], im: &mut [f32]) {
        let n = self.n;
        assert!(re.len() == n && im.len() == n, "FFT buffer length mismatch");

        for (i, &j) in self.rev.iter().enumerate() {
            let j = j as usize;
            if j > i {
                re.swap(i, j);
                im.swap(i, j);
            }
        }

        let mut len = 2;
        while len <= n {
            let half = len / 2;
            let step = n / len;
            let mut start = 0;
            while start < n {
                for k in 0..half {
                    let wr = self.cos[k * step];
                    let wi = -self.sin[k * step];
                    let a = start + k;
                    let b = a + half;
                    let xr = re[b] * wr - im[b] * wi;
                    let xi = re[b] * wi + im[b] * wr;
                    re[b] = re[a] - xr;
                    im[b] = im[a] - xi;
                    re[a] += xr;
                    im[a] += xi;
                }
                start += len;
            }
            len <<= 1;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn magnitudes(n: usize, signal: impl Fn(usize) -> f32) -> Vec<f32> {
        let fft = Fft::new(n);
        let mut re: Vec<f32> = (0..n).map(signal).collect();
        let mut im = vec![0.0; n];
        fft.forward(&mut re, &mut im);
        re.iter()
            .zip(&im)
            .map(|(r, i)| (r * r + i * i).sqrt())
            .collect()
    }

    #[test]
    fn pure_sine_peaks_at_its_bin() {
        let n = 1024;
        let bin = 37;
        let mags = magnitudes(n, |i| (TAU * ((bin * i) % n) as f32 / n as f32).sin());
        let (peak, _) = mags[..n / 2]
            .iter()
            .enumerate()
            .fold(
                (0, 0.0f32),
                |acc, (i, &m)| if m > acc.1 { (i, m) } else { acc },
            );
        assert_eq!(peak, bin);
        // Unit sine, rectangular window: |X[k]| = N/2 at the bin, ~0 elsewhere.
        assert!((mags[bin] - n as f32 / 2.0).abs() < 0.5, "{}", mags[bin]);
        assert!((mags[n - bin] - n as f32 / 2.0).abs() < 0.5);
        for (i, &m) in mags.iter().enumerate() {
            if i != bin && i != n - bin {
                assert!(m < 0.05, "leak at bin {i}: {m}");
            }
        }
    }

    #[test]
    fn dc_and_impulse() {
        let n = 8;
        let dc = magnitudes(n, |_| 1.0);
        assert!((dc[0] - 8.0).abs() < 1e-5);
        assert!(dc[1..].iter().all(|&m| m < 1e-5));

        let imp = magnitudes(n, |i| if i == 0 { 1.0 } else { 0.0 });
        assert!(imp.iter().all(|&m| (m - 1.0).abs() < 1e-5));
    }

    #[test]
    fn matches_naive_dft() {
        let n = 64;
        let x = |i: usize| ((i * 7919) % 101) as f32 / 50.0 - 1.0;
        let fft = Fft::new(n);
        let mut re: Vec<f32> = (0..n).map(x).collect();
        let mut im = vec![0.0; n];
        fft.forward(&mut re, &mut im);
        for (k, (&r, &i)) in re.iter().zip(&im).enumerate() {
            let (mut sr, mut si) = (0.0f64, 0.0f64);
            for t in 0..n {
                let ang = -std::f64::consts::TAU * (k * t) as f64 / n as f64;
                sr += x(t) as f64 * ang.cos();
                si += x(t) as f64 * ang.sin();
            }
            assert!((r as f64 - sr).abs() < 1e-3, "re[{k}]");
            assert!((i as f64 - si).abs() < 1e-3, "im[{k}]");
        }
    }
}
