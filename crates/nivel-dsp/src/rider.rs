//! Controller that "rides" the operating system's microphone volume.
//!
//! Instead of processing audio, it watches the raw input and nudges the OS
//! input slider in small steps until speech sits at the target level. It is
//! closed-loop, so it works whatever curve the OS uses between slider
//! position and gain. Clipping is handled immediately; everything else waits
//! for enough speech to be heard.

use crate::chain::FrameStats;
use crate::level::SpeechLevel;
use crate::util::{FRAME_SECONDS, db_to_lin};

#[derive(Debug, Clone)]
pub struct RiderConfig {
    /// Desired raw speech level, dBFS RMS. Leaves headroom for apps that run
    /// their own processing afterwards.
    pub target_db: f32,
    /// No adjustment while the speech level is within this many dB of target.
    pub tolerance_db: f32,
    /// Never turn the OS volume below this (0.0 to 1.0).
    pub min_volume: f32,
    /// Never turn the OS volume above this (0.0 to 1.0).
    pub max_volume: f32,
    /// Minimum seconds between regular adjustments.
    pub interval_s: f32,
    /// Seconds of speech to hear before judging the level.
    pub min_speech_s: f32,
    /// Largest regular step, in slider units.
    pub max_step: f32,
    /// Step down when the input clips, in slider units.
    pub clip_step: f32,
}

impl Default for RiderConfig {
    fn default() -> Self {
        Self {
            target_db: -24.0,
            tolerance_db: 3.0,
            min_volume: 0.05,
            max_volume: 1.0,
            interval_s: 1.5,
            min_speech_s: 0.8,
            max_step: 0.08,
            clip_step: 0.1,
        }
    }
}

/// Slider units per dB of error for regular adjustments.
const STEP_PER_DB: f32 = 0.008;

#[derive(Debug, Clone)]
pub struct Rider {
    cfg: RiderConfig,
    volume: f32,
    level: SpeechLevel,
    since_adjust: f32,
    clipped_frames: u32,
}

impl Rider {
    pub fn new(cfg: RiderConfig, current_volume: f32) -> Self {
        Self {
            volume: current_volume,
            level: SpeechLevel::new(1.0),
            since_adjust: 0.0,
            clipped_frames: 0,
            cfg,
        }
    }

    pub fn config(&self) -> &RiderConfig {
        &self.cfg
    }

    /// The volume the rider believes the OS is set to.
    pub fn volume(&self) -> f32 {
        self.volume
    }

    /// Raw speech level measured since the last adjustment.
    pub fn speech_db(&self) -> Option<f32> {
        self.level.db()
    }

    /// Whether the slider is maxed out and speech is still too quiet.
    pub fn starved(&self) -> bool {
        self.volume >= self.cfg.max_volume - 1e-3
            && self
                .level
                .db()
                .is_some_and(|l| l < self.cfg.target_db - self.cfg.tolerance_db)
    }

    /// Tell the rider the OS volume was changed by someone else.
    pub fn sync_volume(&mut self, volume: f32) {
        if (volume - self.volume).abs() > 1e-3 {
            self.volume = volume;
            self.restart_measurement();
        }
    }

    /// Feed one frame of measurements. Returns a new OS volume to apply, if
    /// one is due.
    pub fn update(&mut self, stats: &FrameStats) -> Option<f32> {
        self.since_adjust += FRAME_SECONDS;
        if stats.clipped || stats.in_peak_db > -1.0 {
            self.clipped_frames += 1;
        }
        self.level
            .update(db_to_lin(stats.in_db).powi(2), stats.voiced);

        if self.clipped_frames >= 2 && self.since_adjust >= 0.25 {
            return self.adjust(self.volume - self.cfg.clip_step);
        }
        if self.since_adjust < self.cfg.interval_s
            || self.level.voiced_seconds() < self.cfg.min_speech_s
        {
            return None;
        }
        let level = self.level.db()?;
        let error = self.cfg.target_db - level;
        if error.abs() <= self.cfg.tolerance_db {
            return None;
        }
        let step = (error * STEP_PER_DB).clamp(-self.cfg.max_step, self.cfg.max_step);
        self.adjust(self.volume + step)
    }

    fn adjust(&mut self, wanted: f32) -> Option<f32> {
        let new = wanted.clamp(self.cfg.min_volume, self.cfg.max_volume);
        if (new - self.volume).abs() < 0.005 {
            // Pinned at a limit: keep measuring, don't spam the OS.
            self.restart_measurement();
            return None;
        }
        self.volume = new;
        self.restart_measurement();
        Some(new)
    }

    fn restart_measurement(&mut self) {
        self.level.reset();
        self.since_adjust = 0.0;
        self.clipped_frames = 0;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::util::lin_to_db;

    /// Simulate a talker into an OS whose slider maps to gain via `curve`.
    fn simulate(
        voice_at_full_db: f32,
        curve: impl Fn(f32) -> f32,
        start_volume: f32,
        seconds: f32,
    ) -> (Rider, Vec<f32>) {
        let mut rider = Rider::new(RiderConfig::default(), start_volume);
        let mut history = Vec::new();
        let frames = (seconds / FRAME_SECONDS) as usize;
        for i in 0..frames {
            let t = i as f32 * FRAME_SECONDS;
            let talking = (t % 3.0) < 2.0;
            let level = voice_at_full_db + curve(rider.volume());
            let stats = FrameStats {
                in_db: if talking { level } else { -70.0 },
                in_peak_db: if talking { level + 12.0 } else { -60.0 },
                clipped: talking && level + 12.0 >= 0.0,
                voiced: talking,
                ..FrameStats::default()
            };
            if let Some(v) = rider.update(&stats) {
                history.push(v);
            }
        }
        (rider, history)
    }

    fn settled_level(voice_at_full_db: f32, curve: impl Fn(f32) -> f32, rider: &Rider) -> f32 {
        voice_at_full_db + curve(rider.volume())
    }

    #[test]
    fn raises_a_quiet_mic_and_settles() {
        // Square-law slider (like many mixers): gain = 40·log10(v).
        let curve = |v: f32| 40.0 * v.max(1e-4).log10();
        let (rider, history) = simulate(-14.0, curve, 0.3, 60.0);
        let level = settled_level(-14.0, curve, &rider);
        assert!((level + 24.0).abs() <= 3.5, "settled at {level} dB");
        let tail = &history[history.len().saturating_sub(3)..];
        assert!(
            history.len() < 25,
            "should not keep hunting: {} moves",
            history.len()
        );
        assert!(tail.windows(2).all(|w| (w[0] - w[1]).abs() < 0.1));
    }

    #[test]
    fn lowers_a_clipping_mic_fast() {
        // dB-linear slider spanning 50 dB.
        let curve = |v: f32| (v - 1.0) * 50.0;
        let (rider, history) = simulate(-2.0, curve, 1.0, 30.0);
        let level = settled_level(-2.0, curve, &rider);
        assert!(!history.is_empty() && history[0] < 1.0);
        assert!((level + 24.0).abs() <= 3.5, "settled at {level} dB");
    }

    #[test]
    fn reports_starvation_when_maxed_out() {
        let curve = |v: f32| lin_to_db(v);
        let (rider, _) = simulate(-50.0, curve, 0.5, 60.0);
        assert!(rider.volume() >= 0.999);
        // Keep feeding at max volume so a measurement is available.
        let mut rider = rider;
        for _ in 0..200 {
            let stats = FrameStats {
                in_db: -50.0,
                in_peak_db: -40.0,
                voiced: true,
                ..FrameStats::default()
            };
            rider.update(&stats);
        }
        assert!(rider.starved());
    }
}
