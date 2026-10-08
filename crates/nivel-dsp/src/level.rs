//! Level trackers: speech loudness and background noise floor.

use crate::util::{FRAME_SECONDS, SILENCE_DB, coeff, power_to_db};

/// Running estimate of how loud the speaker is, updated only on frames that
/// contain voice. Averages in the power domain, so it behaves like the RMS of
/// the speech itself rather than of the pauses between words.
#[derive(Debug, Clone)]
pub struct SpeechLevel {
    tau: f32,
    power: Option<f32>,
    voiced_seconds: f32,
}

impl SpeechLevel {
    /// `tau` is the averaging time constant in seconds of voiced audio.
    pub fn new(tau: f32) -> Self {
        Self {
            tau,
            power: None,
            voiced_seconds: 0.0,
        }
    }

    pub fn update(&mut self, frame_power: f32, voiced: bool) {
        if !voiced {
            return;
        }
        self.voiced_seconds += FRAME_SECONDS;
        // Behave like a plain running mean until `tau` seconds of speech have
        // been heard, so the first estimate converges quickly.
        let tau = self.tau.min(self.voiced_seconds);
        let c = coeff(tau, FRAME_SECONDS);
        self.power = Some(match self.power {
            None => frame_power,
            Some(p) => p + (frame_power - p) * (1.0 - c),
        });
    }

    /// Speech level in dBFS, once any speech has been heard.
    pub fn db(&self) -> Option<f32> {
        self.power.map(power_to_db)
    }

    /// Seconds of speech that went into the estimate.
    pub fn voiced_seconds(&self) -> f32 {
        self.voiced_seconds
    }

    pub fn reset(&mut self) {
        self.power = None;
        self.voiced_seconds = 0.0;
    }
}

/// Minimum-tracking noise floor estimate: drops quickly to quieter frames and
/// creeps up slowly, so speech barely moves it but a fan turning on does.
#[derive(Debug, Clone)]
pub struct NoiseFloor {
    db: Option<f32>,
    rise_db_per_s: f32,
}

impl Default for NoiseFloor {
    fn default() -> Self {
        Self {
            db: None,
            rise_db_per_s: 2.0,
        }
    }
}

impl NoiseFloor {
    pub fn update(&mut self, frame_db: f32) {
        // Digital silence says nothing about the room.
        if frame_db <= SILENCE_DB {
            return;
        }
        self.db = Some(match self.db {
            None => frame_db,
            Some(f) if frame_db < f => f + (frame_db - f) * (1.0 - coeff(0.05, FRAME_SECONDS)),
            Some(f) => (f + self.rise_db_per_s * FRAME_SECONDS).min(frame_db),
        });
    }

    pub fn db(&self) -> f32 {
        self.db.unwrap_or(SILENCE_DB)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::util::db_to_lin;

    fn p(db: f32) -> f32 {
        db_to_lin(db).powi(2)
    }

    #[test]
    fn speech_level_ignores_unvoiced_frames() {
        let mut s = SpeechLevel::new(1.0);
        for _ in 0..100 {
            s.update(p(-30.0), true);
            s.update(p(-80.0), false);
        }
        assert!((s.db().unwrap() + 30.0).abs() < 0.1);
    }

    #[test]
    fn speech_level_follows_changes() {
        let mut s = SpeechLevel::new(0.5);
        for _ in 0..200 {
            s.update(p(-40.0), true);
        }
        for _ in 0..300 {
            s.update(p(-20.0), true);
        }
        assert!((s.db().unwrap() + 20.0).abs() < 0.5);
    }

    #[test]
    fn noise_floor_tracks_minimum() {
        let mut f = NoiseFloor::default();
        // Room noise at -60 with speech bursts at -25.
        for i in 0..1000 {
            f.update(if i % 10 < 6 { -25.0 } else { -60.0 });
        }
        assert!(
            f.db() < -55.0,
            "floor {} should stay near the room noise",
            f.db()
        );
    }
}
