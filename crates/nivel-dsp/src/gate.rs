//! Soft, voice-driven downward expander.
//!
//! When nobody is talking the signal is turned down by `range_db` rather than
//! muted, so the line never sounds dead and word onsets are never chopped.

use crate::util::{FRAME_SECONDS, apply_ramp, coeff, db_to_lin};

#[derive(Debug, Clone)]
pub struct Gate {
    range_db: f32,
    hold_s: f32,
    open_tau: f32,
    close_tau: f32,
    hold_left: f32,
    gain_db: f32,
}

impl Gate {
    /// `range_db` is how far to turn down silence; 0 disables the gate.
    pub fn new(range_db: f32) -> Self {
        Self {
            range_db: range_db.max(0.0),
            hold_s: 0.3,
            open_tau: 0.002,
            close_tau: 0.15,
            hold_left: 0.0,
            gain_db: 0.0,
        }
    }

    /// Current attenuation in dB (0 when open, `-range_db` when closed).
    pub fn gain_db(&self) -> f32 {
        self.gain_db
    }

    pub fn process(&mut self, frame: &mut [f32], voiced: bool) -> f32 {
        if self.range_db <= 0.0 {
            return 0.0;
        }
        let target = if voiced {
            self.hold_left = self.hold_s;
            0.0
        } else if self.hold_left > 0.0 {
            self.hold_left -= FRAME_SECONDS;
            0.0
        } else {
            -self.range_db
        };
        let tau = if target > self.gain_db {
            self.open_tau
        } else {
            self.close_tau
        };
        let new_gain = target + (self.gain_db - target) * coeff(tau, FRAME_SECONDS);
        apply_ramp(frame, db_to_lin(self.gain_db), db_to_lin(new_gain));
        self.gain_db = new_gain;
        new_gain
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::util::FRAME;

    #[test]
    fn closes_after_hold_and_reopens_instantly() {
        let mut g = Gate::new(12.0);
        let mut f = [0.1; FRAME];
        for _ in 0..200 {
            g.process(&mut f, false);
        }
        assert!((g.gain_db() + 12.0).abs() < 0.1);
        g.process(&mut f, true);
        assert!(
            g.gain_db() > -0.5,
            "voice must reopen the gate within one frame"
        );
    }

    #[test]
    fn holds_open_through_short_pauses() {
        let mut g = Gate::new(12.0);
        let mut f = [0.1; FRAME];
        g.process(&mut f, true);
        for _ in 0..20 {
            g.process(&mut f, false); // 200 ms pause, shorter than the hold
        }
        assert!(g.gain_db() > -0.1);
    }

    #[test]
    fn zero_range_is_bypass() {
        let mut g = Gate::new(0.0);
        let mut f = [0.5; FRAME];
        for _ in 0..100 {
            g.process(&mut f, false);
        }
        assert!(f.iter().all(|&s| s == 0.5));
    }
}
