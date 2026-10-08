//! Voice-gated automatic gain control.
//!
//! The gain only adapts while someone is talking, so pauses don't pump the
//! background noise up to speech level. Gain drops faster than it rises:
//! getting too loud is worse than being slightly quiet for a moment.

use crate::level::SpeechLevel;
use crate::util::{FRAME_SECONDS, apply_ramp, db_to_lin};

#[derive(Debug, Clone)]
pub struct AgcConfig {
    /// Desired speech level, dBFS RMS.
    pub target_db: f32,
    /// Maximum boost for quiet speakers.
    pub max_gain_db: f32,
    /// Maximum cut for loud speakers (negative).
    pub min_gain_db: f32,
    /// How fast the gain may rise, dB per second.
    pub rise_db_per_s: f32,
    /// How fast the gain may fall, dB per second.
    pub fall_db_per_s: f32,
    /// Averaging window for the speech level, seconds.
    pub level_tau: f32,
}

impl Default for AgcConfig {
    fn default() -> Self {
        Self {
            target_db: -20.0,
            max_gain_db: 30.0,
            min_gain_db: -20.0,
            rise_db_per_s: 6.0,
            fall_db_per_s: 20.0,
            level_tau: 1.0,
        }
    }
}

/// Seconds of speech during which the AGC converges faster.
const WARMUP_SECONDS: f32 = 1.5;

#[derive(Debug, Clone)]
pub struct Agc {
    cfg: AgcConfig,
    level: SpeechLevel,
    gain_db: f32,
}

impl Agc {
    pub fn new(cfg: AgcConfig) -> Self {
        let level = SpeechLevel::new(cfg.level_tau);
        Self {
            cfg,
            level,
            gain_db: 0.0,
        }
    }

    pub fn gain_db(&self) -> f32 {
        self.gain_db
    }

    /// Speech level before gain, if speech has been heard.
    pub fn speech_db(&self) -> Option<f32> {
        self.level.db()
    }

    /// Whether the gain is pinned at its maximum (the speaker is very quiet).
    pub fn at_max_gain(&self) -> bool {
        self.gain_db >= self.cfg.max_gain_db - 0.5
    }

    /// Apply gain to a frame. `frame_power` is the frame's mean power before
    /// gain and `voiced` whether it contains speech. Returns the gain in dB.
    pub fn process(&mut self, frame: &mut [f32], frame_power: f32, voiced: bool) -> f32 {
        self.level.update(frame_power, voiced);
        let desired = match self.level.db() {
            Some(level) => {
                (self.cfg.target_db - level).clamp(self.cfg.min_gain_db, self.cfg.max_gain_db)
            }
            None => 0.0,
        };

        let warming_up = self.level.voiced_seconds() < WARMUP_SECONDS;
        let (rise, fall) = if warming_up {
            (
                self.cfg.rise_db_per_s.max(30.0),
                self.cfg.fall_db_per_s.max(60.0),
            )
        } else {
            (self.cfg.rise_db_per_s, self.cfg.fall_db_per_s)
        };
        let step = (desired - self.gain_db).clamp(-fall * FRAME_SECONDS, rise * FRAME_SECONDS);

        // Hold the gain through pauses; only ever reduce it while silent.
        let new_gain = if voiced || step < 0.0 {
            self.gain_db + step
        } else {
            self.gain_db
        };
        apply_ramp(frame, db_to_lin(self.gain_db), db_to_lin(new_gain));
        self.gain_db = new_gain;
        new_gain
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::util::{FRAME, power, power_to_db};

    fn tone(level_db: f32) -> Vec<f32> {
        let amp = db_to_lin(level_db) * std::f32::consts::SQRT_2;
        (0..FRAME)
            .map(|i| amp * (i as f32 * 2.0 * std::f32::consts::PI * 300.0 / 48_000.0).sin())
            .collect()
    }

    fn run(agc: &mut Agc, level_db: f32, voiced: bool, seconds: f32) -> f32 {
        let mut out_db = 0.0;
        for _ in 0..(seconds / FRAME_SECONDS) as usize {
            let mut f = tone(level_db);
            let p = power(&f);
            agc.process(&mut f, p, voiced);
            out_db = power_to_db(power(&f));
        }
        out_db
    }

    #[test]
    fn lifts_quiet_speaker_to_target() {
        let mut agc = Agc::new(AgcConfig::default());
        let out = run(&mut agc, -42.0, true, 6.0);
        assert!(
            (out + 20.0).abs() < 1.0,
            "output {out} dB should be near -20"
        );
    }

    #[test]
    fn tames_loud_speaker() {
        let mut agc = Agc::new(AgcConfig::default());
        let out = run(&mut agc, -6.0, true, 4.0);
        assert!(
            (out + 20.0).abs() < 1.0,
            "output {out} dB should be near -20"
        );
    }

    #[test]
    fn does_not_pump_noise_during_pauses() {
        let mut agc = Agc::new(AgcConfig::default());
        run(&mut agc, -20.0, true, 3.0);
        let before = agc.gain_db();
        run(&mut agc, -70.0, false, 5.0);
        assert!(
            (agc.gain_db() - before).abs() < 0.01,
            "gain must hold while nobody talks"
        );
    }

    #[test]
    fn respects_max_gain() {
        let mut agc = Agc::new(AgcConfig::default());
        run(&mut agc, -80.0, true, 10.0);
        assert!(agc.gain_db() <= 30.0 + 1e-3);
        assert!(agc.at_max_gain());
    }
}
