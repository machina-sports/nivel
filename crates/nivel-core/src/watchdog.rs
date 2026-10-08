//! Health checks over successive telemetry snapshots. Pure logic, no I/O.

use std::collections::VecDeque;
use std::fmt;

use crate::telemetry::Snapshot;

/// Something the user should know about the microphone right now.
#[derive(Debug, Clone, PartialEq)]
pub enum Alert {
    /// The mic delivers exact zeros: hardware mute switch, muted input, or no
    /// permission to record (macOS and Windows privacy settings).
    DigitalSilence { seconds: f32 },
    /// The input is hitting full scale; the hardware gain is too high.
    Clipping { percent: f32 },
    /// Speech is so quiet that the AGC is giving all the boost it may.
    TooQuiet { seconds: f32 },
    /// Audio was dropped or padded recently (CPU starvation, device hiccup).
    Glitches { count: u64 },
}

impl fmt::Display for Alert {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Alert::DigitalSilence { seconds } => write!(
                f,
                "mic has been completely silent for {seconds:.0}s: check the mute switch, the OS input volume and the app's microphone permission"
            ),
            Alert::Clipping { percent } => write!(
                f,
                "mic is clipping ({percent:.1}% of the last seconds): lower its hardware gain or run `nivel ride`"
            ),
            Alert::TooQuiet { seconds } => write!(
                f,
                "voice is very quiet even at maximum boost ({seconds:.0}s): move closer or raise the mic gain"
            ),
            Alert::Glitches { count } => write!(f, "{count} audio glitch(es) in the last seconds"),
        }
    }
}

#[derive(Debug, Clone)]
pub struct WatchdogConfig {
    /// Restart if a stream has not called back for this long.
    pub stall_ms: u64,
    /// Alert after this much digital silence.
    pub silence_alert_s: f32,
    /// Reopen the device after this much digital silence (it often fixes a
    /// device that went quiet after sleep or a USB hiccup)...
    pub silence_restart_s: f32,
    /// ...but not more often than this.
    pub silence_restart_every_s: f32,
    /// Window for clipping and glitch statistics.
    pub window_s: f32,
    /// Alert when more than this share of frames clipped in the window.
    pub clip_ratio: f32,
    /// Alert after this many seconds of speech pinned at maximum gain.
    pub quiet_alert_s: f32,
}

impl Default for WatchdogConfig {
    fn default() -> Self {
        Self {
            stall_ms: 2_000,
            silence_alert_s: 3.0,
            silence_restart_s: 6.0,
            silence_restart_every_s: 30.0,
            window_s: 5.0,
            clip_ratio: 0.01,
            quiet_alert_s: 8.0,
        }
    }
}

#[derive(Debug, Clone, Copy, Default)]
struct Delta {
    dt: f32,
    frames: u64,
    clipped: u64,
    glitches: u64,
}

#[derive(Debug, Default)]
pub struct Watchdog {
    cfg: WatchdogConfig,
    last: Option<Snapshot>,
    window: VecDeque<Delta>,
    silent_s: f32,
    quiet_s: f32,
    since_silence_restart: Option<f32>,
}

/// Outcome of one check.
#[derive(Debug, Default)]
pub struct Verdict {
    /// The engine should be rebuilt, for this reason.
    pub restart: Option<String>,
    pub alerts: Vec<Alert>,
}

impl Watchdog {
    pub fn new(cfg: WatchdogConfig) -> Self {
        Self {
            cfg,
            ..Self::default()
        }
    }

    /// Forget history, e.g. after the engine was rebuilt.
    pub fn reset(&mut self) {
        self.last = None;
        self.window.clear();
        self.silent_s = 0.0;
        self.quiet_s = 0.0;
    }

    /// Evaluate a fresh snapshot taken `dt` seconds after the previous one.
    pub fn check(&mut self, snap: &Snapshot, has_output: bool, dt: f32) -> Verdict {
        let mut verdict = Verdict::default();

        if snap.input_age_ms > self.cfg.stall_ms {
            verdict.restart = Some("the microphone stopped delivering audio".into());
        } else if has_output && snap.output_age_ms > self.cfg.stall_ms {
            verdict.restart = Some("the output device stopped playing".into());
        }

        let last = self
            .last
            .replace(snap.clone())
            .unwrap_or_else(|| snap.clone());
        let d = Delta {
            dt,
            frames: snap.frames.saturating_sub(last.frames),
            clipped: snap.clipped_frames.saturating_sub(last.clipped_frames),
            glitches: snap.glitches().saturating_sub(last.glitches()),
        };
        let silent = snap.silent_frames.saturating_sub(last.silent_frames);
        let voiced = snap.voiced_frames.saturating_sub(last.voiced_frames);
        let quiet = snap
            .quiet_voiced_frames
            .saturating_sub(last.quiet_voiced_frames);

        self.window.push_back(d);
        let mut span: f32 = self.window.iter().map(|d| d.dt).sum();
        while span > self.cfg.window_s && self.window.len() > 1 {
            span -= self.window.pop_front().map_or(0.0, |d| d.dt);
        }

        if d.frames > 0 && silent == d.frames {
            self.silent_s += dt;
        } else if d.frames > 0 {
            self.silent_s = 0.0;
        }
        if self.silent_s >= self.cfg.silence_alert_s {
            verdict.alerts.push(Alert::DigitalSilence {
                seconds: self.silent_s,
            });
        }
        if let Some(t) = &mut self.since_silence_restart {
            *t += dt;
        }
        let may_restart = self
            .since_silence_restart
            .is_none_or(|t| t >= self.cfg.silence_restart_every_s);
        if verdict.restart.is_none() && self.silent_s >= self.cfg.silence_restart_s && may_restart {
            self.since_silence_restart = Some(0.0);
            verdict.restart = Some("the microphone went completely silent".into());
        }

        if voiced > 0 {
            if quiet * 2 > voiced {
                self.quiet_s += dt;
            } else {
                self.quiet_s = 0.0;
            }
        }
        if self.quiet_s >= self.cfg.quiet_alert_s {
            verdict.alerts.push(Alert::TooQuiet {
                seconds: self.quiet_s,
            });
        }

        let frames: u64 = self.window.iter().map(|d| d.frames).sum();
        let clipped: u64 = self.window.iter().map(|d| d.clipped).sum();
        if frames > 0 && clipped as f32 / frames as f32 > self.cfg.clip_ratio {
            verdict.alerts.push(Alert::Clipping {
                percent: 100.0 * clipped as f32 / frames as f32,
            });
        }
        let glitches: u64 = self.window.iter().map(|d| d.glitches).sum();
        if glitches > 0 {
            verdict.alerts.push(Alert::Glitches { count: glitches });
        }

        verdict
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const TICK: f32 = 0.1;
    const FRAMES_PER_TICK: u64 = 10;

    fn advance(s: &mut Snapshot, silent: bool, clipped: bool, voiced_quiet: bool) {
        s.frames += FRAMES_PER_TICK;
        if silent {
            s.silent_frames += FRAMES_PER_TICK;
        }
        if clipped {
            s.clipped_frames += FRAMES_PER_TICK;
        }
        if voiced_quiet {
            s.voiced_frames += FRAMES_PER_TICK;
            s.quiet_voiced_frames += FRAMES_PER_TICK;
        }
    }

    #[test]
    fn healthy_stream_is_quiet() {
        let mut w = Watchdog::new(WatchdogConfig::default());
        let mut s = Snapshot::default();
        for _ in 0..100 {
            advance(&mut s, false, false, false);
            let v = w.check(&s, true, TICK);
            assert!(v.restart.is_none() && v.alerts.is_empty(), "{v:?}");
        }
    }

    #[test]
    fn stall_requests_restart() {
        let mut w = Watchdog::new(WatchdogConfig::default());
        let s = Snapshot {
            input_age_ms: 2_500,
            ..Snapshot::default()
        };
        assert!(w.check(&s, false, TICK).restart.is_some());
        let s = Snapshot {
            output_age_ms: 2_500,
            ..Snapshot::default()
        };
        assert!(
            w.check(&s, false, TICK).restart.is_none(),
            "output age is ignored without an output"
        );
        assert!(w.check(&s, true, TICK).restart.is_some());
    }

    #[test]
    fn digital_silence_alerts_then_restarts_once() {
        let mut w = Watchdog::new(WatchdogConfig::default());
        let mut s = Snapshot::default();
        let mut alerted_at = None;
        let mut restarts = 0;
        for i in 0..200 {
            advance(&mut s, true, false, false);
            let v = w.check(&s, false, TICK);
            if alerted_at.is_none()
                && v.alerts
                    .iter()
                    .any(|a| matches!(a, Alert::DigitalSilence { .. }))
            {
                alerted_at = Some(i);
            }
            restarts += v.restart.is_some() as u32;
        }
        let at = alerted_at.expect("silence should be reported") as f32 * TICK;
        assert!((2.9..=3.2).contains(&at), "alerted after {at}s");
        assert_eq!(
            restarts, 1,
            "20 s of silence: one reopen attempt, then wait 30 s"
        );
    }

    #[test]
    fn clipping_is_reported() {
        let mut w = Watchdog::new(WatchdogConfig::default());
        let mut s = Snapshot::default();
        let mut v = Verdict::default();
        for i in 0..20 {
            advance(&mut s, false, i % 5 == 0, false);
            v = w.check(&s, false, TICK);
        }
        assert!(v.alerts.iter().any(|a| matches!(a, Alert::Clipping { .. })));
    }

    #[test]
    fn sustained_max_gain_is_reported() {
        let mut w = Watchdog::new(WatchdogConfig::default());
        let mut s = Snapshot::default();
        let mut v = Verdict::default();
        for _ in 0..100 {
            advance(&mut s, false, false, true);
            v = w.check(&s, false, TICK);
        }
        assert!(v.alerts.iter().any(|a| matches!(a, Alert::TooQuiet { .. })));
    }
}
