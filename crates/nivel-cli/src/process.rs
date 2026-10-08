use std::path::Path;

use anyhow::{Context, Result, bail};
use hound::{SampleFormat, WavReader, WavSpec, WavWriter};
use nivel_core::{DspConfig, offline};
use nivel_dsp::SAMPLE_RATE;

fn read_mono(path: &Path) -> Result<(Vec<f32>, u32)> {
    let mut reader =
        WavReader::open(path).with_context(|| format!("opening {}", path.display()))?;
    let spec = reader.spec();
    let interleaved: Vec<f32> = match spec.sample_format {
        SampleFormat::Float => reader.samples::<f32>().collect::<Result<_, _>>()?,
        SampleFormat::Int => {
            let scale = (1i64 << (spec.bits_per_sample - 1)) as f32;
            reader
                .samples::<i32>()
                .map(|s| s.map(|v| v as f32 / scale))
                .collect::<Result<_, _>>()?
        }
    };
    let channels = spec.channels.max(1) as usize;
    let mono = interleaved
        .chunks_exact(channels)
        .map(|f| f.iter().sum::<f32>() / channels as f32)
        .collect();
    Ok((mono, spec.sample_rate))
}

fn fmt_db(v: Option<f32>) -> String {
    match v {
        None => "—".into(),
        Some(d) if d < -90.0 => "silent".into(),
        Some(d) => format!("{d:.1} dB"),
    }
}

pub fn run(input: &Path, output: &Path, float: bool, dsp: &DspConfig) -> Result<()> {
    let (samples, rate) = read_mono(input)?;
    if samples.is_empty() {
        bail!("{} has no audio", input.display());
    }
    let (out, report) = offline::process(&samples, rate, dsp)?;

    let spec = WavSpec {
        channels: 1,
        sample_rate: SAMPLE_RATE,
        bits_per_sample: if float { 32 } else { 16 },
        sample_format: if float {
            SampleFormat::Float
        } else {
            SampleFormat::Int
        },
    };
    let mut writer = WavWriter::create(output, spec)
        .with_context(|| format!("creating {}", output.display()))?;
    for s in &out {
        if float {
            writer.write_sample(*s)?;
        } else {
            writer.write_sample((s.clamp(-1.0, 1.0) * i16::MAX as f32).round() as i16)?;
        }
    }
    writer.finalize()?;

    println!(
        "Processed {:.1} s → {} (48 kHz mono)\n",
        report.seconds,
        output.display()
    );
    println!("                 before       after");
    println!(
        "  Voice          {:<12} {}",
        fmt_db(report.speech_in_db),
        fmt_db(report.speech_out_db)
    );
    println!(
        "  Background     {:<12} {}",
        fmt_db(report.noise_in_db),
        fmt_db(report.noise_out_db)
    );
    println!(
        "  Speech         {:.0}% of the clip",
        report.voiced_ratio * 100.0
    );
    if report.clipped_frames > 0 {
        println!(
            "  ! the source clipped in {} frame(s); clipping can't be undone, only kept from getting worse",
            report.clipped_frames
        );
    }
    if report.limited_frames > 0 {
        println!(
            "  · the limiter caught peaks in {} frame(s)",
            report.limited_frames
        );
    }
    Ok(())
}
