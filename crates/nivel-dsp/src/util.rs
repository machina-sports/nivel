//! Small numeric helpers shared by the processors.

/// Processing sample rate. RNNoise only works at 48 kHz.
pub const SAMPLE_RATE: u32 = 48_000;

/// Samples per processing frame (10 ms at 48 kHz).
pub const FRAME: usize = 480;

/// Duration of one frame in seconds.
pub const FRAME_SECONDS: f32 = FRAME as f32 / SAMPLE_RATE as f32;

/// Floor used for "no signal" when converting to decibels.
pub const SILENCE_DB: f32 = -120.0;

/// Decibels to linear gain.
#[inline]
pub fn db_to_lin(db: f32) -> f32 {
    10f32.powf(db / 20.0)
}

/// Linear amplitude to dBFS, clamped at [`SILENCE_DB`].
#[inline]
pub fn lin_to_db(x: f32) -> f32 {
    if x <= 1e-6 {
        SILENCE_DB
    } else {
        (20.0 * x.log10()).max(SILENCE_DB)
    }
}

/// Mean power to dBFS, clamped at [`SILENCE_DB`].
#[inline]
pub fn power_to_db(p: f32) -> f32 {
    if p <= 1e-12 {
        SILENCE_DB
    } else {
        (10.0 * p.log10()).max(SILENCE_DB)
    }
}

/// Mean power (mean of squares) of a block.
#[inline]
pub fn power(x: &[f32]) -> f32 {
    if x.is_empty() {
        return 0.0;
    }
    x.iter().map(|s| s * s).sum::<f32>() / x.len() as f32
}

/// Absolute peak of a block.
#[inline]
pub fn peak(x: &[f32]) -> f32 {
    x.iter().fold(0.0f32, |m, s| m.max(s.abs()))
}

/// One-pole smoothing coefficient for a time constant `tau` (seconds) when
/// stepping by `dt` seconds. `y += (x - y) * (1 - coeff)`.
#[inline]
pub fn coeff(tau: f32, dt: f32) -> f32 {
    if tau <= 0.0 { 0.0 } else { (-dt / tau).exp() }
}

/// Multiply a block by a gain that moves linearly from `g0` to `g1`, which
/// avoids zipper noise when the gain changes between frames.
#[inline]
pub fn apply_ramp(x: &mut [f32], g0: f32, g1: f32) {
    let n = x.len() as f32;
    if (g1 - g0).abs() < 1e-7 {
        x.iter_mut().for_each(|s| *s *= g1);
        return;
    }
    for (i, s) in x.iter_mut().enumerate() {
        let t = (i + 1) as f32 / n;
        *s *= g0 + (g1 - g0) * t;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn db_roundtrip() {
        for db in [-60.0, -20.0, -6.0, 0.0, 6.0] {
            assert!((lin_to_db(db_to_lin(db)) - db).abs() < 1e-3);
        }
        assert_eq!(lin_to_db(0.0), SILENCE_DB);
        assert_eq!(power_to_db(0.0), SILENCE_DB);
    }

    #[test]
    fn full_scale_sine_is_minus_3_dbfs() {
        let x: Vec<f32> = (0..4800)
            .map(|i| (i as f32 * 2.0 * std::f32::consts::PI * 1000.0 / 48_000.0).sin())
            .collect();
        assert!((power_to_db(power(&x)) + 3.01).abs() < 0.05);
        assert!((peak(&x) - 1.0).abs() < 1e-3);
    }
}
