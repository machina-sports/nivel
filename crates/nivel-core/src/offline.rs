//! Run the chain over a whole clip: useful for recordings, demos and tests.

use anyhow::{Context, Result};
use audioadapter_buffers::direct::InterleavedSlice;
use nivel_dsp::util::{db_to_lin, power_to_db};
use nivel_dsp::{Chain, DspConfig, FRAME, FrameStats, SAMPLE_RATE};
use rubato::{Fft, FixedSync, Resampler};

/// Before/after numbers for a processed clip.
#[derive(Debug, Clone)]
pub struct OfflineReport {
    pub seconds: f32,
    /// Speech level before processing, dBFS RMS.
    pub speech_in_db: Option<f32>,
    /// Speech level after processing, dBFS RMS.
    pub speech_out_db: Option<f32>,
    /// Typical background noise when nobody talks, before processing, dBFS.
    pub noise_in_db: Option<f32>,
    /// Typical background noise when nobody talks, after processing, dBFS.
    pub noise_out_db: Option<f32>,
    /// Share of the clip that contained speech.
    pub voiced_ratio: f32,
    /// Frames where the input clipped.
    pub clipped_frames: usize,
    /// Frames where the limiter had to step in.
    pub limited_frames: usize,
}

/// Process mono `samples` at `sample_rate`. The result is 48 kHz mono,
/// time-aligned with the input.
pub fn process(
    samples: &[f32],
    sample_rate: u32,
    cfg: &DspConfig,
) -> Result<(Vec<f32>, OfflineReport)> {
    let input = if sample_rate == SAMPLE_RATE {
        samples.to_vec()
    } else {
        resample(samples, sample_rate, SAMPLE_RATE)?
    };

    let mut chain = Chain::new(cfg.clone());
    let latency = chain.latency();
    let len = input.len();
    let mut padded = input;
    padded.resize((len + latency).div_ceil(FRAME) * FRAME, 0.0);
    let mut out = vec![0.0; padded.len()];

    let mut stats: Vec<FrameStats> = Vec::with_capacity(padded.len() / FRAME);
    for (i, o) in padded
        .as_chunks::<FRAME>()
        .0
        .iter()
        .zip(out.as_chunks_mut::<FRAME>().0)
    {
        stats.push(chain.process(i, o));
    }
    out.drain(..latency);
    out.truncate(len);

    // Only frames that held real input count towards the report.
    let real = &stats[..len.div_ceil(FRAME).min(stats.len())];
    let mean_db = |sel: &dyn Fn(&FrameStats) -> bool, val: &dyn Fn(&FrameStats) -> f32| {
        let picked: Vec<f32> = real
            .iter()
            .filter(|s| sel(s))
            .map(|s| db_to_lin(val(s)).powi(2))
            .collect();
        (!picked.is_empty()).then(|| power_to_db(picked.iter().sum::<f32>() / picked.len() as f32))
    };
    let report = OfflineReport {
        seconds: len as f32 / SAMPLE_RATE as f32,
        speech_in_db: mean_db(&|s| s.voiced, &|s| s.in_db),
        speech_out_db: mean_db(&|s| s.voiced, &|s| s.out_db),
        noise_in_db: background(real, |s| s.in_db),
        noise_out_db: background(real, |s| s.out_db),
        voiced_ratio: real.iter().filter(|s| s.voiced).count() as f32 / real.len().max(1) as f32,
        clipped_frames: real.iter().filter(|s| s.clipped).count(),
        limited_frames: real.iter().filter(|s| s.limiter_db < -0.5).count(),
    };
    Ok((out, report))
}

/// Median level of the non-speech frames. A median rather than a mean, so
/// word tails, breaths and the odd bump don't pass for background noise.
fn background(frames: &[FrameStats], level: impl Fn(&FrameStats) -> f32) -> Option<f32> {
    let mut v: Vec<f32> = frames
        .iter()
        .filter(|s| !s.voiced && !s.silent)
        .map(level)
        .collect();
    if v.is_empty() {
        return None;
    }
    v.sort_by(f32::total_cmp);
    Some(v[v.len() / 2])
}

fn resample(x: &[f32], from: u32, to: u32) -> Result<Vec<f32>> {
    let mut rs = Fft::<f32>::new(from as usize, to as usize, 1024, 1, FixedSync::Both)
        .context("creating the resampler")?;
    let input = InterleavedSlice::new(x, 1, x.len()).context("wrapping the input")?;
    Ok(rs
        .process_all(&input, x.len(), None)
        .context("resampling")?
        .take_data())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keeps_length_and_alignment() {
        let x: Vec<f32> = (0..44_100)
            .map(|i| 0.05 * (i as f32 * 0.07).sin())
            .collect();
        let (out, report) = process(&x, 44_100, &DspConfig::default()).unwrap();
        assert!(
            (out.len() as i64 - 48_000).abs() <= 2,
            "got {} samples",
            out.len()
        );
        assert!((report.seconds - 1.0).abs() < 0.01);
    }
}
