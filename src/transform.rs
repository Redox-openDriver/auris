use alloc::vec;
use alloc::vec::Vec;
use core::f64::consts::PI;

#[derive(Clone, Copy, Default, Debug, PartialEq)]
pub struct Complex {
    pub re: f32,
    pub im: f32,
}

impl Complex {
    fn times(self, other: Complex) -> Complex {
        Complex { re: self.re * other.re - self.im * other.im, im: self.re * other.im + self.im * other.re }
    }

    fn turn(angle: f64) -> Complex {
        Complex { re: libm::cos(angle) as f32, im: libm::sin(angle) as f32 }
    }
}

struct Fft {
    size: usize,
    twiddles: Vec<Complex>,
    reversed: Vec<usize>,
}

impl Fft {
    fn new(size: usize) -> Fft {
        let bits = size.trailing_zeros();
        let reversed = (0..size).map(|index| if bits == 0 { 0 } else { index.reverse_bits() >> (usize::BITS - bits) }).collect();
        let twiddles = (0..size / 2).map(|index| Complex::turn(-2.0 * PI * index as f64 / size as f64)).collect();
        Fft { size, twiddles, reversed }
    }

    fn run(&self, data: &mut [Complex]) {
        for (index, &other) in self.reversed.iter().enumerate() {
            if other > index {
                data.swap(index, other);
            }
        }
        let mut length = 2;
        while length <= self.size {
            let step = self.size / length;
            for start in (0..self.size).step_by(length) {
                for offset in 0..length / 2 {
                    let twiddle = self.twiddles[offset * step];
                    let even = data[start + offset];
                    let odd = data[start + offset + length / 2].times(twiddle);
                    data[start + offset] = Complex { re: even.re + odd.re, im: even.im + odd.im };
                    data[start + offset + length / 2] = Complex { re: even.re - odd.re, im: even.im - odd.im };
                }
            }
            length *= 2;
        }
    }
}

pub struct Dct4 {
    size: usize,
    fft: Fft,
    pre: Vec<Complex>,
    post: Vec<Complex>,
}

impl Dct4 {
    pub fn new(size: usize) -> Dct4 {
        let half = size / 2;
        let m = size as f64;
        Dct4 {
            size,
            fft: Fft::new(half),
            pre: (0..half).map(|p| Complex::turn(-PI * p as f64 / m)).collect(),
            post: (0..half).map(|q| Complex::turn(-PI * (q as f64 + 0.25) / m)).collect(),
        }
    }

    pub fn run(&self, input: &[f32], output: &mut [f32]) {
        let (m, half) = (self.size, self.size / 2);
        let mut work: Vec<Complex> = (0..half).map(|p| Complex { re: input[2 * p], im: input[m - 1 - 2 * p] }.times(self.pre[p])).collect();
        self.fft.run(&mut work);
        for q in 0..half {
            let value = work[q].times(self.post[q]);
            output[2 * q] = value.re;
            output[m - 1 - 2 * q] = -value.im;
        }
    }
}

pub struct Mdct {
    half: usize,
    dct: Dct4,
}

impl Mdct {
    pub fn new(window: usize) -> Mdct {
        Mdct { half: window / 2, dct: Dct4::new(window / 2) }
    }

    pub fn forward(&self, windowed: &[f32], spectrum: &mut [f32]) {
        let m = self.half;
        let quarter = m / 2;
        let mut folded = vec![0f32; m];
        for i in 0..quarter {
            folded[i] = -windowed[3 * quarter - 1 - i] - windowed[3 * quarter + i];
            folded[quarter + i] = windowed[i] - windowed[m - 1 - i];
        }
        self.dct.run(&folded, spectrum);
        for value in spectrum.iter_mut().take(m) {
            *value *= 2.0;
        }
    }

    pub fn inverse(&self, spectrum: &[f32], output: &mut [f32]) {
        let m = self.half;
        let quarter = m / 2;
        let mut u = vec![0f32; m];
        self.dct.run(spectrum, &mut u);
        let scale = 1.0 / m as f32;
        for n in 0..2 * m {
            output[n] = scale
                * match n {
                    _ if n < quarter => u[n + quarter],
                    _ if n < 3 * quarter => -u[3 * quarter - 1 - n],
                    _ => -u[n - 3 * quarter],
                };
        }
    }
}

fn bessel_i0(x: f64) -> f64 {
    let mut sum = 1.0;
    let mut term = 1.0;
    for k in 1..50 {
        term *= x / 2.0 / k as f64;
        sum += term * term;
    }
    sum
}

pub fn sine_window(length: usize) -> Vec<f32> {
    (0..length / 2).map(|n| libm::sin(PI / length as f64 * (n as f64 + 0.5)) as f32).collect()
}

pub fn kbd_window(length: usize, alpha: f64) -> Vec<f32> {
    let half = length / 2;
    let kaiser: Vec<f64> = (0..=half)
        .map(|j| {
            let ratio = (j as f64 - half as f64 / 2.0) / (half as f64 / 2.0);
            bessel_i0(PI * alpha * libm::sqrt((1.0 - ratio * ratio).max(0.0)))
        })
        .collect();
    let total: f64 = kaiser.iter().sum();
    let mut running = 0.0;
    (0..half)
        .map(|n| {
            running += kaiser[n];
            libm::sqrt(running / total) as f32
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn direct_dct4(input: &[f32]) -> Vec<f32> {
        let m = input.len();
        (0..m).map(|k| (0..m).map(|n| f64::from(input[n]) * libm::cos(PI / m as f64 * (n as f64 + 0.5) * (k as f64 + 0.5))).sum::<f64>() as f32).collect()
    }

    #[test]
    fn the_fast_dct_iv_matches_its_definition() {
        for size in [8usize, 128, 1024] {
            let input: Vec<f32> = (0..size).map(|n| libm::sin(n as f64 * 0.37) as f32 + (n % 7) as f32 * 0.1).collect();
            let mut fast = vec![0f32; size];
            Dct4::new(size).run(&input, &mut fast);
            let slow = direct_dct4(&input);
            let worst = fast.iter().zip(&slow).map(|(a, b)| (a - b).abs()).fold(0f32, f32::max);
            assert!(worst < 2e-3 * size as f32 / 128.0, "{size}: {worst}");
        }
    }

    #[test]
    fn windowed_transforms_overlap_back_into_the_signal() {
        for (window, shape) in [(2048usize, sine_window(2048)), (256, sine_window(256)), (2048, kbd_window(2048, 4.0)), (256, kbd_window(256, 6.0))] {
            let half = window / 2;
            let full: Vec<f32> = shape.iter().copied().chain(shape.iter().rev().copied()).collect();
            assert!((0..half).all(|n| (full[n] * full[n] + full[n + half] * full[n + half] - 1.0).abs() < 1e-5));
            let mdct = Mdct::new(window);
            let signal: Vec<f32> = (0..half * 6).map(|n| 1000.0 * libm::sin(n as f64 * 0.05) as f32 + (n % 13) as f32).collect();
            let mut output = vec![0f32; signal.len()];
            let mut previous = vec![0f32; half];
            for (frame, start) in (0..signal.len() - window + 1).step_by(half).enumerate() {
                let windowed: Vec<f32> = (0..window).map(|n| signal[start + n] * full[n]).collect();
                let mut spectrum = vec![0f32; half];
                mdct.forward(&windowed, &mut spectrum);
                let mut back = vec![0f32; window];
                mdct.inverse(&spectrum, &mut back);
                for n in 0..half {
                    output[start + n] = previous[n] + back[n] * full[n];
                }
                previous = (half..window).map(|n| back[n] * full[n]).collect();
                if frame > 0 {
                    let worst = (start..start + half).map(|n| (output[n] - signal[n]).abs()).fold(0f32, f32::max);
                    assert!(worst < 0.05, "{window}: {worst}");
                }
            }
        }
    }
}
