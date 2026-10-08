//! RNNoise-based noise suppression (via the pure-Rust `nnnoiseless` port).

use nnnoiseless::DenoiseState;

use crate::util::FRAME;

/// RNNoise works on 16-bit-scaled floats.
const I16_SCALE: f32 = 32_768.0;

/// Wraps an RNNoise state with float I/O in `[-1, 1]` and an optional
/// wet/dry mix for gentler suppression.
pub struct Denoiser {
    state: Box<DenoiseState<'static>>,
    mix: f32,
    scaled_in: [f32; FRAME],
    scaled_out: [f32; FRAME],
    /// Dry signal delayed to line up with RNNoise's output.
    dry_delay: [f32; FRAME],
}

impl Denoiser {
    /// `mix` is the share of the denoised signal: 1.0 is full suppression,
    /// 0.0 bypasses it (the voice probability is still computed).
    pub fn new(mix: f32) -> Self {
        Self {
            state: DenoiseState::new(),
            mix: mix.clamp(0.0, 1.0),
            scaled_in: [0.0; FRAME],
            scaled_out: [0.0; FRAME],
            dry_delay: [0.0; FRAME],
        }
    }

    /// Denoise one frame in place and return the voice-activity probability
    /// (0.0 to 1.0) RNNoise estimated for it.
    pub fn process(&mut self, frame: &mut [f32]) -> f32 {
        debug_assert_eq!(frame.len(), FRAME);
        for (dst, src) in self.scaled_in.iter_mut().zip(frame.iter()) {
            *dst = src * I16_SCALE;
        }
        let vad = self
            .state
            .process_frame(&mut self.scaled_out, &self.scaled_in);
        if self.mix >= 1.0 {
            for (dst, src) in frame.iter_mut().zip(self.scaled_out.iter()) {
                *dst = src / I16_SCALE;
            }
        } else {
            // RNNoise's output lags its input by one frame; delay the dry
            // path the same amount so the mix doesn't comb-filter.
            for ((s, delayed), wet) in frame
                .iter_mut()
                .zip(self.dry_delay.iter_mut())
                .zip(self.scaled_out.iter())
            {
                let dry = std::mem::replace(delayed, *s);
                *s = wet / I16_SCALE * self.mix + dry * (1.0 - self.mix);
            }
        }
        vad
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The dry path delay above assumes RNNoise lags by exactly one frame.
    #[test]
    fn output_lags_input_by_one_frame() {
        let sr = 48_000.0;
        let x: Vec<f32> = (0..FRAME * 200)
            .map(|i| {
                let t = i as f32 / sr;
                let env = (0.5 + 0.5 * (2.0 * std::f32::consts::PI * 3.0 * t).sin()).powi(2);
                let voice: f32 = (1..=6)
                    .map(|h| (2.0 * std::f32::consts::PI * 170.0 * h as f32 * t).sin() / h as f32)
                    .sum();
                0.1 * env * voice
            })
            .collect();
        let mut d = Denoiser::new(1.0);
        let mut y = x.clone();
        for f in y.as_chunks_mut::<FRAME>().0 {
            d.process(f);
        }
        let start = FRAME * 50;
        let len = FRAME * 100;
        let best = (0..2 * FRAME)
            .max_by(|&a, &b| {
                let c =
                    |lag: usize| -> f32 { (start..start + len).map(|i| x[i] * y[i + lag]).sum() };
                c(a).total_cmp(&c(b))
            })
            .unwrap();
        // RNNoise's internal high-pass adds a little phase shift at voice
        // fundamentals, so allow a few samples either side.
        assert!(
            best.abs_diff(FRAME) <= 4,
            "RNNoise latency is {best} samples: the dry/wet mix delay must be updated"
        );
    }
}
