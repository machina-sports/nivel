//! Look-ahead peak limiter: a hard ceiling for shouts, laughs, bumps and pops.
//!
//! The signal is delayed by the look-ahead window while the gain envelope
//! starts ducking before a peak arrives, so peaks are caught without the
//! distortion of plain clipping.

use std::collections::VecDeque;

use crate::util::{coeff, db_to_lin, lin_to_db};

#[derive(Debug, Clone)]
pub struct Limiter {
    ceiling: f32,
    lookahead: usize,
    delay: Vec<f32>,
    pos: usize,
    /// Monotonic queue of (sample index, required gain) for a sliding minimum.
    window: VecDeque<(u64, f32)>,
    n: u64,
    env: f32,
    attack: f32,
    release: f32,
    min_env_in_block: f32,
}

impl Limiter {
    pub fn new(sample_rate: u32, ceiling_db: f32, lookahead_ms: f32, release_ms: f32) -> Self {
        let sr = sample_rate as f32;
        let lookahead = ((lookahead_ms / 1000.0) * sr).round().max(1.0) as usize;
        Self {
            ceiling: db_to_lin(ceiling_db.min(0.0)),
            lookahead,
            delay: vec![0.0; lookahead],
            pos: 0,
            window: VecDeque::with_capacity(lookahead + 2),
            n: 0,
            env: 1.0,
            // Reach the target well within the look-ahead window.
            attack: coeff(lookahead as f32 / 6.0, 1.0),
            release: coeff(release_ms / 1000.0 * sr, 1.0),
            min_env_in_block: 1.0,
        }
    }

    /// Delay introduced by the look-ahead, in samples.
    pub fn latency(&self) -> usize {
        self.lookahead
    }

    #[inline]
    pub fn process(&mut self, x: f32) -> f32 {
        let a = x.abs();
        let required = if a > self.ceiling {
            self.ceiling / a
        } else {
            1.0
        };

        while let Some(&(_, v)) = self.window.back() {
            if v >= required {
                self.window.pop_back();
            } else {
                break;
            }
        }
        self.window.push_back((self.n, required));
        while let Some(&(i, _)) = self.window.front() {
            if i + (self.lookahead as u64) < self.n {
                self.window.pop_front();
            } else {
                break;
            }
        }
        let target = self.window.front().map_or(1.0, |&(_, v)| v);

        let c = if target < self.env {
            self.attack
        } else {
            self.release
        };
        self.env = target + (self.env - target) * c;
        self.min_env_in_block = self.min_env_in_block.min(self.env);

        let delayed = self.delay[self.pos];
        self.delay[self.pos] = x;
        self.pos = (self.pos + 1) % self.lookahead;
        self.n += 1;

        // The envelope gets within 0.25% of target in time; the clamp is a
        // safety net for that last fraction.
        (delayed * self.env).clamp(-self.ceiling, self.ceiling)
    }

    /// Process a block in place; returns the deepest gain reduction in dB
    /// (0 or negative) applied during the block.
    pub fn process_block(&mut self, block: &mut [f32]) -> f32 {
        self.min_env_in_block = self.env;
        for s in block.iter_mut() {
            *s = self.process(*s);
        }
        lin_to_db(self.min_env_in_block).min(0.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sine(amp: f32, n: usize) -> Vec<f32> {
        (0..n)
            .map(|i| amp * (i as f32 * 2.0 * std::f32::consts::PI * 440.0 / 48_000.0).sin())
            .collect()
    }

    #[test]
    fn never_exceeds_ceiling() {
        let mut l = Limiter::new(48_000, -1.0, 5.0, 80.0);
        let mut x = sine(4.0, 48_000); // +12 dBFS
        l.process_block(&mut x);
        let ceiling = db_to_lin(-1.0);
        assert!(x.iter().all(|s| s.abs() <= ceiling + 1e-6));
    }

    #[test]
    fn leaves_quiet_audio_alone() {
        let mut l = Limiter::new(48_000, -1.0, 5.0, 80.0);
        let input = sine(0.25, 4_800);
        let mut x = input.clone();
        let gr = l.process_block(&mut x);
        assert!(gr > -0.01);
        let lat = l.latency();
        for i in lat..x.len() {
            assert!((x[i] - input[i - lat]).abs() < 1e-6);
        }
    }

    #[test]
    fn catches_a_single_spike_cleanly() {
        let mut l = Limiter::new(48_000, -1.0, 5.0, 80.0);
        let mut x = vec![0.1f32; 4_800];
        x[2_000] = 3.0;
        l.process_block(&mut x);
        let ceiling = db_to_lin(-1.0);
        let lat = l.latency();
        let out = x[2_000 + lat];
        assert!(
            out <= ceiling + 1e-6 && out > ceiling * 0.95,
            "spike {out} should sit at the ceiling"
        );
    }
}
