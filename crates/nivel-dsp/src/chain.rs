//! The full voice chain: high-pass → noise suppression → AGC → gate → limiter.

use crate::agc::{Agc, AgcConfig};
use crate::biquad::Biquad;
use crate::denoise::Denoiser;
use crate::gate::Gate;
use crate::level::NoiseFloor;
use crate::limiter::Limiter;
use crate::util::{FRAME, SAMPLE_RATE, lin_to_db, peak, power, power_to_db};

/// User-facing knobs for the chain. The defaults suit speech into a video
/// call, stream or recording.
#[derive(Debug, Clone)]
pub struct DspConfig {
    /// Rumble filter cutoff in Hz; 0 disables it.
    pub highpass_hz: f32,
    /// RNNoise noise suppression.
    pub denoise: bool,
    /// Share of the denoised signal, 0.0 to 1.0. Lower sounds more natural
    /// in quiet rooms but lets more noise through.
    pub denoise_mix: f32,
    /// Automatic gain control.
    pub agc: bool,
    /// Speech level the AGC aims for, dBFS RMS.
    pub target_db: f32,
    /// Most the AGC may boost a quiet voice, dB.
    pub max_gain_db: f32,
    /// How far to turn down the gaps between words, dB; 0 disables the gate.
    pub gate_range_db: f32,
    /// Absolute output ceiling, dBFS.
    pub ceiling_db: f32,
}

impl Default for DspConfig {
    fn default() -> Self {
        Self {
            highpass_hz: 80.0,
            denoise: true,
            denoise_mix: 1.0,
            agc: true,
            target_db: -20.0,
            max_gain_db: 30.0,
            gate_range_db: 10.0,
            ceiling_db: -1.0,
        }
    }
}

/// Measurements for one processed frame.
#[derive(Debug, Clone, Copy, Default)]
pub struct FrameStats {
    /// Raw input peak, dBFS.
    pub in_peak_db: f32,
    /// Raw input RMS, dBFS.
    pub in_db: f32,
    /// Output peak, dBFS.
    pub out_peak_db: f32,
    /// Output RMS, dBFS.
    pub out_db: f32,
    /// RNNoise voice probability (0 when noise suppression is off).
    pub vad: f32,
    /// Whether this frame was judged to contain speech.
    pub voiced: bool,
    /// Gain applied by the AGC, dB.
    pub agc_db: f32,
    /// Attenuation applied by the gate, dB (0 or negative).
    pub gate_db: f32,
    /// Deepest gain reduction from the limiter, dB (0 or negative).
    pub limiter_db: f32,
    /// Estimated background noise floor of the input, dBFS.
    pub floor_db: f32,
    /// The input hit full scale (the mic or interface is clipping).
    pub clipped: bool,
    /// The input was pure digital silence (muted, unplugged or no permission).
    pub silent: bool,
}

/// Samples at or above this are treated as clipped.
const CLIP_LEVEL: f32 = 0.99;
/// Below half an LSB of 16-bit audio: nothing is coming from the hardware.
const DIGITAL_SILENCE: f32 = 1.0 / 65_536.0;

pub struct Chain {
    cfg: DspConfig,
    hpf: Option<Biquad>,
    denoiser: Option<Denoiser>,
    floor: NoiseFloor,
    agc: Option<Agc>,
    gate: Gate,
    limiter: Limiter,
}

impl Chain {
    pub fn new(cfg: DspConfig) -> Self {
        let hpf = (cfg.highpass_hz > 0.0).then(|| {
            Biquad::highpass(
                SAMPLE_RATE as f32,
                cfg.highpass_hz,
                std::f32::consts::FRAC_1_SQRT_2,
            )
        });
        let denoiser = cfg.denoise.then(|| Denoiser::new(cfg.denoise_mix));
        let agc = cfg.agc.then(|| {
            Agc::new(AgcConfig {
                target_db: cfg.target_db,
                max_gain_db: cfg.max_gain_db,
                ..AgcConfig::default()
            })
        });
        Self {
            hpf,
            denoiser,
            floor: NoiseFloor::default(),
            agc,
            gate: Gate::new(cfg.gate_range_db),
            limiter: Limiter::new(SAMPLE_RATE, cfg.ceiling_db, 5.0, 80.0),
            cfg,
        }
    }

    pub fn config(&self) -> &DspConfig {
        &self.cfg
    }

    /// Delay from input to output, in samples at 48 kHz.
    pub fn latency(&self) -> usize {
        self.limiter.latency() + if self.denoiser.is_some() { FRAME } else { 0 }
    }

    /// Whether the AGC is already giving all the boost it is allowed to.
    pub fn agc_at_max(&self) -> bool {
        self.agc.as_ref().is_some_and(Agc::at_max_gain)
    }

    /// Process one [`FRAME`] of mono 48 kHz audio from `input` into `output`.
    pub fn process(&mut self, input: &[f32], output: &mut [f32]) -> FrameStats {
        assert_eq!(input.len(), FRAME, "Chain::process takes exactly one frame");
        assert_eq!(
            output.len(),
            FRAME,
            "Chain::process takes exactly one frame"
        );

        let in_peak = peak(input);
        let mut stats = FrameStats {
            in_peak_db: lin_to_db(in_peak),
            in_db: power_to_db(power(input)),
            clipped: input.iter().filter(|s| s.abs() >= CLIP_LEVEL).count() >= 3,
            silent: in_peak < DIGITAL_SILENCE,
            ..FrameStats::default()
        };

        output.copy_from_slice(input);
        if let Some(hpf) = &mut self.hpf {
            hpf.process_block(output);
        }

        let pre_db = power_to_db(power(output));
        self.floor.update(pre_db);
        let floor = self.floor.db();
        stats.floor_db = floor;

        stats.voiced = match &mut self.denoiser {
            Some(d) => {
                stats.vad = d.process(output);
                stats.vad >= 0.5 && pre_db > floor + 3.0 && pre_db > -70.0
            }
            None => pre_db > floor + 9.0 && pre_db > -60.0,
        };

        if let Some(agc) = &mut self.agc {
            let p = power(output);
            stats.agc_db = agc.process(output, p, stats.voiced);
        }
        stats.gate_db = self.gate.process(output, stats.voiced);
        stats.limiter_db = self.limiter.process_block(output);

        stats.out_peak_db = lin_to_db(peak(output));
        stats.out_db = power_to_db(power(output));
        stats
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::util::db_to_lin;

    /// A crude voice stand-in: harmonic tone at ~150 Hz with syllable-rate
    /// amplitude modulation and pauses, plus steady background noise.
    fn fake_speech(seconds: f32, voice_db: f32, noise_db: f32) -> (Vec<f32>, Vec<bool>) {
        let n = (seconds * SAMPLE_RATE as f32) as usize;
        let mut seed = 0x1234_5678u32;
        let mut noise = || {
            seed ^= seed << 13;
            seed ^= seed >> 17;
            seed ^= seed << 5;
            (seed as f32 / u32::MAX as f32) * 2.0 - 1.0
        };
        let v_amp = db_to_lin(voice_db);
        let n_amp = db_to_lin(noise_db) * 3f32.sqrt(); // uniform noise RMS = amp/sqrt(3)
        let mut talking = Vec::with_capacity(n);
        let x = (0..n)
            .map(|i| {
                let t = i as f32 / SAMPLE_RATE as f32;
                // 2 s talk, 1 s pause.
                let on = (t % 3.0) < 2.0;
                talking.push(on);
                let syllable = (0.5 + 0.5 * (2.0 * std::f32::consts::PI * 4.0 * t).sin()).powi(2);
                let f0 = 150.0;
                let voice: f32 = (1..=8)
                    .map(|h| (2.0 * std::f32::consts::PI * f0 * h as f32 * t).sin() / h as f32)
                    .sum::<f32>()
                    * 0.6;
                let v = if on {
                    v_amp * syllable * voice * 2.0
                } else {
                    0.0
                };
                v + n_amp * noise()
            })
            .collect();
        (x, talking)
    }

    fn run(cfg: DspConfig, x: &[f32]) -> (Vec<f32>, Vec<FrameStats>) {
        let mut chain = Chain::new(cfg);
        let mut out = vec![0.0; x.len() / FRAME * FRAME];
        let mut stats = Vec::new();
        for (i, o) in x
            .as_chunks::<FRAME>()
            .0
            .iter()
            .zip(out.as_chunks_mut::<FRAME>().0)
        {
            stats.push(chain.process(i, o));
        }
        (out, stats)
    }

    #[test]
    fn output_never_exceeds_ceiling() {
        let (x, _) = fake_speech(6.0, -3.0, -50.0);
        let x: Vec<f32> = x.iter().map(|s| s * 4.0).collect(); // badly clipping input
        let (out, stats) = run(DspConfig::default(), &x);
        let ceiling = db_to_lin(-1.0);
        assert!(out.iter().all(|s| s.abs() <= ceiling + 1e-6));
        assert!(
            stats.iter().any(|s| s.clipped),
            "input clipping should be reported"
        );
    }

    #[test]
    fn quiet_noisy_speaker_comes_out_level_and_clean() {
        let (x, _) = fake_speech(12.0, -42.0, -58.0);
        for denoise in [true, false] {
            let cfg = DspConfig {
                denoise,
                ..DspConfig::default()
            };
            let (_, stats) = run(cfg, &x);
            let late: Vec<_> = stats.iter().skip(stats.len() / 2).collect();
            let voiced: Vec<_> = late.iter().filter(|s| s.voiced).collect();
            assert!(
                !voiced.is_empty(),
                "speech should be detected (denoise={denoise})"
            );
            let speech_out = power_to_db(
                voiced
                    .iter()
                    .map(|s| db_to_lin(s.out_db).powi(2))
                    .sum::<f32>()
                    / voiced.len() as f32,
            );
            assert!(
                (speech_out + 20.0).abs() < 4.0,
                "speech should land near the -20 dB target, got {speech_out} (denoise={denoise})"
            );
            let quiet: Vec<_> = late.iter().filter(|s| !s.voiced).collect();
            let gap_out = quiet.iter().map(|s| s.out_db).sum::<f32>() / quiet.len().max(1) as f32;
            assert!(
                gap_out < speech_out - 15.0,
                "pauses ({gap_out} dB) should stay well below speech ({speech_out} dB) (denoise={denoise})"
            );
        }
    }

    #[test]
    fn digital_silence_is_flagged() {
        let x = vec![0.0f32; FRAME * 10];
        let (_, stats) = run(DspConfig::default(), &x);
        assert!(stats.iter().all(|s| s.silent && !s.voiced));
    }
}
