//! Second-order IIR filter (RBJ cookbook), transposed direct form II.

#[derive(Debug, Clone)]
pub struct Biquad {
    b0: f32,
    b1: f32,
    b2: f32,
    a1: f32,
    a2: f32,
    z1: f32,
    z2: f32,
}

impl Biquad {
    /// High-pass filter. A `q` of `FRAC_1_SQRT_2` gives a Butterworth response.
    pub fn highpass(sample_rate: f32, cutoff_hz: f32, q: f32) -> Self {
        let w0 = 2.0 * std::f32::consts::PI * cutoff_hz / sample_rate;
        let (sin, cos) = w0.sin_cos();
        let alpha = sin / (2.0 * q);
        let a0 = 1.0 + alpha;
        Self {
            b0: (1.0 + cos) / 2.0 / a0,
            b1: -(1.0 + cos) / a0,
            b2: (1.0 + cos) / 2.0 / a0,
            a1: -2.0 * cos / a0,
            a2: (1.0 - alpha) / a0,
            z1: 0.0,
            z2: 0.0,
        }
    }

    #[inline]
    pub fn process(&mut self, x: f32) -> f32 {
        let y = self.b0 * x + self.z1;
        self.z1 = self.b1 * x - self.a1 * y + self.z2;
        self.z2 = self.b2 * x - self.a2 * y;
        y
    }

    pub fn process_block(&mut self, x: &mut [f32]) {
        for s in x {
            *s = self.process(*s);
        }
    }

    pub fn reset(&mut self) {
        self.z1 = 0.0;
        self.z2 = 0.0;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::util::{power, power_to_db};

    fn gain_at(freq: f32) -> f32 {
        let mut f = Biquad::highpass(48_000.0, 80.0, std::f32::consts::FRAC_1_SQRT_2);
        let mut x: Vec<f32> = (0..48_000)
            .map(|i| (i as f32 * 2.0 * std::f32::consts::PI * freq / 48_000.0).sin())
            .collect();
        let before = power_to_db(power(&x[24_000..]));
        f.process_block(&mut x);
        power_to_db(power(&x[24_000..])) - before
    }

    #[test]
    fn removes_rumble_keeps_voice() {
        assert!(gain_at(20.0) < -20.0, "20 Hz should be cut");
        assert!(gain_at(1_000.0).abs() < 0.1, "1 kHz should pass untouched");
        assert!(gain_at(200.0).abs() < 0.5, "voice fundamentals should pass");
    }
}
