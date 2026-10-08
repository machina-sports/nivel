//! Real-time pipeline: microphone → DSP worker → output device.
//!
//! The audio callbacks only move samples in and out of lock-free ring
//! buffers. A worker thread does everything else: resampling to 48 kHz,
//! running the [`Chain`], and resampling to the output device's rate with a
//! slowly adjusted ratio so the two device clocks never drift apart.
//!
//! ```text
//! mic callback ──ring──► worker: resample → Chain → resample (drift) ──ring──► output callback
//! ```

use std::collections::VecDeque;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering::Relaxed};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use anyhow::{Context, Result, bail};
use audioadapter_buffers::direct::InterleavedSlice;
use cpal::traits::{DeviceTrait, StreamTrait};
use cpal::{
    FromSample, Sample, SampleFormat, SizedSample, StreamConfig, SupportedBufferSize,
    SupportedStreamConfig,
};
use nivel_dsp::util::{FRAME_SECONDS, coeff, power, power_to_db};
use nivel_dsp::{Chain, DspConfig, FRAME, SAMPLE_RATE};
use rtrb::{Consumer, Producer, RingBuffer};
use rubato::{Adjustable, Async, FixedAsync, PolynomialDegree, Resampler};

use crate::devices;
use crate::telemetry::Telemetry;

/// Instantiate a generic stream builder for the device's sample format.
macro_rules! with_sample_type {
    ($format:expr, $func:ident ( $($arg:expr),* $(,)? )) => {
        match $format {
            SampleFormat::F32 => $func::<f32>($($arg),*),
            SampleFormat::I16 => $func::<i16>($($arg),*),
            SampleFormat::I32 => $func::<i32>($($arg),*),
            SampleFormat::I24 => $func::<cpal::I24>($($arg),*),
            SampleFormat::U16 => $func::<u16>($($arg),*),
            SampleFormat::U8 => $func::<u8>($($arg),*),
            SampleFormat::I8 => $func::<i8>($($arg),*),
            SampleFormat::F64 => $func::<f64>($($arg),*),
            other => bail!("unsupported sample format {other}"),
        }
    };
}

/// What to capture, how to process it and where to send it.
#[derive(Debug, Clone, Default)]
pub struct EngineConfig {
    /// Microphone name or fragment; `None` for the system default.
    pub input: Option<String>,
    /// Output device name or fragment; `None` to only analyze.
    pub output: Option<String>,
    /// Capture a single input channel (0-based) instead of auto-mixing.
    pub channel: Option<usize>,
    pub dsp: DspConfig,
}

/// What the engine actually opened.
#[derive(Debug, Clone)]
pub struct EngineInfo {
    pub input: String,
    pub input_rate: u32,
    pub input_channels: u16,
    pub output: Option<String>,
    pub output_rate: Option<u32>,
    pub notes: Vec<String>,
}

pub struct Engine {
    stop: Arc<AtomicBool>,
    worker: Option<JoinHandle<()>>,
    // Dropped after the worker has been joined (see `Drop`).
    _input: cpal::Stream,
    _output: Option<cpal::Stream>,
    info: EngineInfo,
}

impl Engine {
    pub fn start(host: &cpal::Host, cfg: &EngineConfig, telemetry: Arc<Telemetry>) -> Result<Self> {
        let selected = devices::select_input(host, cfg.input.as_deref())?;
        let mut notes: Vec<String> = selected.note.into_iter().collect();

        let in_supported = choose_config(&selected.device, true)
            .with_context(|| format!("reading the formats of \"{}\"", selected.name))?;
        let in_rate = in_supported.sample_rate();
        let in_channels = in_supported.channels();
        if let Some(c) = cfg.channel
            && c >= in_channels as usize
        {
            bail!(
                "channel {} is out of range: \"{}\" has {in_channels} channel(s)",
                c + 1,
                selected.name
            );
        }

        // Output first, so a bad output name fails before the mic is opened.
        let mut output = None;
        if let Some(query) = &cfg.output {
            let device = devices::select_output(host, query)?;
            let name = devices::name_of(&device);
            if name == selected.name {
                bail!(
                    "input and output are the same device (\"{name}\"); that would feed back into itself"
                );
            }
            let supported = choose_config(&device, false)
                .with_context(|| format!("reading the formats of \"{name}\""))?;
            output = Some((device, name, supported));
        }

        let (in_prod, in_cons) = RingBuffer::<f32>::new(in_rate as usize);
        let input_stream = with_sample_type!(
            in_supported.sample_format(),
            open_input(
                &selected.device,
                &in_supported,
                cfg.channel,
                in_prod,
                telemetry.clone()
            )
        )
        .with_context(|| format!("opening the microphone \"{}\"", selected.name))?;

        let mut output_stream = None;
        let mut out_stage = None;
        let mut out_info = (None, None);
        if let Some((device, name, supported)) = output {
            let rate = supported.sample_rate();
            let (prod, cons) = RingBuffer::<f32>::new(rate as usize);
            let prime = Arc::new(AtomicUsize::new(usize::MAX));
            out_stage = Some(OutStage::new(rate, prod, prime.clone())?);
            output_stream = Some(
                with_sample_type!(
                    supported.sample_format(),
                    open_output(&device, &supported, cons, prime, telemetry.clone())
                )
                .with_context(|| format!("opening the output \"{name}\""))?,
            );
            out_info = (Some(name), Some(rate));
        }

        let in_stage = if in_rate == SAMPLE_RATE {
            None
        } else {
            notes.push(format!(
                "resampling the microphone from {in_rate} Hz to {SAMPLE_RATE} Hz"
            ));
            Some(InStage::new(in_rate)?)
        };

        telemetry.mark_start();
        let stop = Arc::new(AtomicBool::new(false));
        let worker = Worker {
            stop: stop.clone(),
            telemetry,
            input: in_cons,
            in_stage,
            pending: VecDeque::with_capacity(SAMPLE_RATE as usize),
            frame_in: vec![0.0; FRAME],
            frame_out: vec![0.0; FRAME],
            chain: Chain::new(cfg.dsp.clone()),
            out_stage,
            in_power: 0.0,
            out_power: 0.0,
        };
        let handle = thread::Builder::new()
            .name("nivel-dsp".into())
            .spawn(move || worker.run())
            .context("starting the DSP thread")?;

        input_stream
            .play()
            .context("starting the microphone stream")?;
        if let Some(s) = &output_stream {
            s.play().context("starting the output stream")?;
        }

        Ok(Self {
            stop,
            worker: Some(handle),
            _input: input_stream,
            _output: output_stream,
            info: EngineInfo {
                input: selected.name,
                input_rate: in_rate,
                input_channels: in_channels,
                output: out_info.0,
                output_rate: out_info.1,
                notes,
            },
        })
    }

    pub fn info(&self) -> &EngineInfo {
        &self.info
    }
}

impl Drop for Engine {
    fn drop(&mut self) {
        self.stop.store(true, Relaxed);
        if let Some(h) = self.worker.take() {
            let _ = h.join();
        }
    }
}

/// Prefer the device's current rate in f32 (no system-wide rate switch),
/// then 48 kHz in f32, then whatever the device suggests.
fn choose_config(device: &cpal::Device, input: bool) -> Result<SupportedStreamConfig> {
    let default = if input {
        device.default_input_config()?
    } else {
        device.default_output_config()?
    };
    let ranges: Vec<_> = if input {
        device.supported_input_configs()?.collect()
    } else {
        device.supported_output_configs()?.collect()
    };
    for rate in [default.sample_rate(), SAMPLE_RATE] {
        if let Some(r) = ranges.iter().find(|r| {
            r.sample_format() == SampleFormat::F32
                && r.channels() == default.channels()
                && r.contains_rate(rate)
        }) {
            return Ok(r.with_sample_rate(rate));
        }
    }
    Ok(default)
}

/// PulseAudio picks multi-second buffers by default, so on Linux ask for
/// 10 ms periods when the device allows it. Elsewhere the backend default is
/// already low-latency.
fn stream_config(supported: &SupportedStreamConfig) -> StreamConfig {
    let mut config = supported.config();
    if cfg!(target_os = "linux") {
        let period = supported.sample_rate() / 100;
        if let SupportedBufferSize::Range { min, max } = *supported.buffer_size()
            && (min..=max).contains(&period)
        {
            config.buffer_size = cpal::BufferSize::Fixed(period);
        }
    }
    config
}

fn open_input<T>(
    device: &cpal::Device,
    supported: &SupportedStreamConfig,
    channel: Option<usize>,
    mut producer: Producer<f32>,
    telemetry: Arc<Telemetry>,
) -> Result<cpal::Stream>
where
    T: SizedSample,
    f32: FromSample<T>,
{
    let channels = supported.channels() as usize;
    let mut downmix = Downmix::new(channels, channel);
    let on_error = telemetry.clone();
    let stream = device.build_input_stream::<T, _, _>(
        stream_config(supported),
        move |data: &[T], _| {
            downmix.observe(data);
            let mut overrun = false;
            for frame in data.chunks_exact(channels) {
                if producer.push(downmix.mix(frame)).is_err() {
                    overrun = true;
                }
            }
            telemetry.input_callback(overrun);
        },
        move |e| on_error.stream_error(e),
        Some(Duration::from_secs(5)),
    )?;
    Ok(stream)
}

fn open_output<T>(
    device: &cpal::Device,
    supported: &SupportedStreamConfig,
    mut consumer: Consumer<f32>,
    prime: Arc<AtomicUsize>,
    telemetry: Arc<Telemetry>,
) -> Result<cpal::Stream>
where
    T: SizedSample + FromSample<f32>,
{
    let channels = supported.channels() as usize;
    let on_error = telemetry.clone();
    // Hold back until a cushion has built up, and rebuild it after any
    // underrun: one short gap instead of a burst of crackles.
    let mut primed = false;
    let stream = device.build_output_stream::<T, _, _>(
        stream_config(supported),
        move |data: &mut [T], _| {
            let frames = data.len() / channels;
            if !primed && consumer.slots() >= prime.load(Relaxed).max(frames) {
                primed = true;
            }
            let mut underrun = false;
            for frame in data.chunks_exact_mut(channels) {
                let s = if primed {
                    consumer.pop().unwrap_or_else(|_| {
                        underrun = true;
                        0.0
                    })
                } else {
                    0.0
                };
                frame.fill(T::from_sample(s));
            }
            if underrun {
                primed = false;
            }
            telemetry.output_callback(frames, underrun);
        },
        move |e| on_error.stream_error(e),
        Some(Duration::from_secs(5)),
    )?;
    Ok(stream)
}

/// Turns interleaved multi-channel input into mono. Averages channels, except
/// when one channel carries all the signal (an interface with the mic on
/// input 1 only), in which case that channel is used alone so the level and
/// clip detection aren't diluted by an empty channel.
struct Downmix {
    channels: usize,
    fixed: Option<usize>,
    energy: Vec<f32>,
    active: Option<usize>,
    blocks: u32,
}

impl Downmix {
    fn new(channels: usize, fixed: Option<usize>) -> Self {
        Self {
            channels,
            fixed,
            energy: vec![0.0; channels],
            active: fixed,
            blocks: 0,
        }
    }

    fn observe<T>(&mut self, data: &[T])
    where
        T: Sample,
        f32: FromSample<T>,
    {
        if self.fixed.is_some() || self.channels == 1 {
            return;
        }
        for ch in 0..self.channels {
            let e: f32 = data
                .iter()
                .skip(ch)
                .step_by(self.channels)
                .map(|&s| {
                    let f = f32::from_sample(s);
                    f * f
                })
                .sum();
            self.energy[ch] = self.energy[ch] * 0.9 + e * 0.1;
        }
        self.blocks = self.blocks.wrapping_add(1);
        if self.blocks.is_multiple_of(32) {
            let (loud, max) =
                self.energy
                    .iter()
                    .copied()
                    .enumerate()
                    .fold(
                        (0, 0.0f32),
                        |best, (i, e)| if e > best.1 { (i, e) } else { best },
                    );
            let others_silent = self
                .energy
                .iter()
                .enumerate()
                .all(|(i, &e)| i == loud || e < max * 0.01);
            self.active = (max > 1e-9 && others_silent).then_some(loud);
        }
    }

    #[inline]
    fn mix<T>(&self, frame: &[T]) -> f32
    where
        T: Sample,
        f32: FromSample<T>,
    {
        match self.active {
            Some(c) => f32::from_sample(frame[c]),
            None => frame.iter().map(|&s| f32::from_sample(s)).sum::<f32>() / self.channels as f32,
        }
    }
}

/// Microphone rate → 48 kHz.
struct InStage {
    rs: Async<f32>,
    buf_in: Vec<f32>,
    buf_out: Vec<f32>,
}

impl InStage {
    fn new(in_rate: u32) -> Result<Self> {
        let chunk = (in_rate / 100).max(16) as usize;
        let rs = Async::<f32>::new_poly(
            SAMPLE_RATE as f64 / in_rate as f64,
            1.0,
            PolynomialDegree::Septic,
            chunk,
            1,
            FixedAsync::Input,
        )
        .context("creating the input resampler")?;
        Ok(Self {
            buf_in: vec![0.0; rs.input_frames_max()],
            buf_out: vec![0.0; rs.output_frames_max()],
            rs,
        })
    }
}

/// 48 kHz → output rate, with clock-drift compensation.
struct OutStage {
    rs: Async<f32>,
    buf: Vec<f32>,
    producer: Producer<f32>,
    capacity: usize,
    rate: u32,
    target: usize,
    fill: f32,
    prime: Arc<AtomicUsize>,
}

/// Largest ratio correction used to absorb clock drift (0.2 %, far beyond
/// real-world drift and inaudible as pitch).
const MAX_DRIFT_CORRECTION: f64 = 0.002;

impl OutStage {
    fn new(rate: u32, producer: Producer<f32>, prime: Arc<AtomicUsize>) -> Result<Self> {
        let rs = Async::<f32>::new_poly(
            rate as f64 / SAMPLE_RATE as f64,
            1.01,
            PolynomialDegree::Septic,
            FRAME,
            1,
            FixedAsync::Input,
        )
        .context("creating the output resampler")?;
        let target = Self::target_for(rate, 0);
        prime.store(target, Relaxed);
        Ok(Self {
            buf: vec![0.0; rs.output_frames_max()],
            capacity: producer.buffer().capacity(),
            producer,
            rs,
            rate,
            target,
            fill: target as f32,
            prime,
        })
    }

    /// Queue depth to aim for: 20 ms, or two device periods if larger.
    fn target_for(rate: u32, callback_frames: usize) -> usize {
        (rate as usize / 50).max(callback_frames * 2)
    }

    fn push(&mut self, frame: &[f32], telemetry: &Telemetry) {
        let target = Self::target_for(self.rate, telemetry.output_callback_frames());
        if target != self.target {
            self.target = target;
            self.prime.store(target, Relaxed);
        }

        let queued = self.capacity - self.producer.slots();
        self.fill += (queued as f32 - self.fill) * 0.05;
        if queued > self.target * 4 + 4 * FRAME {
            // Far behind (e.g. the output stalled for a moment): skip ahead.
            telemetry.output_dropped(FRAME);
            return;
        }
        let error = ((self.fill - self.target as f32) / self.target as f32).clamp(-1.0, 1.0) as f64;
        let _ = self
            .rs
            .set_resample_ratio_relative(1.0 - MAX_DRIFT_CORRECTION * error, true);

        let Ok(input) = InterleavedSlice::new(frame, 1, FRAME) else {
            return;
        };
        let capacity = self.buf.len();
        let Ok(mut output) = InterleavedSlice::new_mut(&mut self.buf[..], 1, capacity) else {
            return;
        };
        if let Ok((_, produced)) = self.rs.process_into_buffer(&input, &mut output, None) {
            let dropped = self.buf[..produced]
                .iter()
                .filter(|&&s| self.producer.push(s).is_err())
                .count();
            if dropped > 0 {
                telemetry.output_dropped(dropped);
            }
        }
    }
}

struct Worker {
    stop: Arc<AtomicBool>,
    telemetry: Arc<Telemetry>,
    input: Consumer<f32>,
    in_stage: Option<InStage>,
    /// 48 kHz samples waiting to fill a frame.
    pending: VecDeque<f32>,
    frame_in: Vec<f32>,
    frame_out: Vec<f32>,
    chain: Chain,
    out_stage: Option<OutStage>,
    in_power: f32,
    out_power: f32,
}

/// Meter ballistics.
const METER_TAU: f32 = 0.3;

impl Worker {
    fn run(mut self) {
        while !self.stop.load(Relaxed) {
            let got = self.pull();
            while self.pending.len() >= FRAME {
                self.process_frame();
            }
            if !got {
                thread::sleep(Duration::from_millis(2));
            }
        }
    }

    /// Move captured audio into `pending`, resampling to 48 kHz if needed.
    fn pull(&mut self) -> bool {
        match &mut self.in_stage {
            None => {
                let n = self.input.slots();
                if n == 0 {
                    return false;
                }
                if let Ok(chunk) = self.input.read_chunk(n) {
                    let (a, b) = chunk.as_slices();
                    self.pending.extend(a);
                    self.pending.extend(b);
                    chunk.commit_all();
                }
                true
            }
            Some(stage) => {
                let mut any = false;
                loop {
                    let need = stage.rs.input_frames_next();
                    if self.input.slots() < need {
                        break;
                    }
                    let Ok(chunk) = self.input.read_chunk(need) else {
                        break;
                    };
                    let (a, b) = chunk.as_slices();
                    stage.buf_in[..a.len()].copy_from_slice(a);
                    stage.buf_in[a.len()..need].copy_from_slice(b);
                    chunk.commit_all();
                    any = true;

                    let Ok(input) = InterleavedSlice::new(&stage.buf_in[..need], 1, need) else {
                        break;
                    };
                    let capacity = stage.buf_out.len();
                    let Ok(mut output) =
                        InterleavedSlice::new_mut(&mut stage.buf_out[..], 1, capacity)
                    else {
                        break;
                    };
                    if let Ok((_, produced)) =
                        stage.rs.process_into_buffer(&input, &mut output, None)
                    {
                        self.pending.extend(&stage.buf_out[..produced]);
                    }
                }
                any
            }
        }
    }

    fn process_frame(&mut self) {
        for (dst, src) in self.frame_in.iter_mut().zip(self.pending.drain(..FRAME)) {
            *dst = src;
        }
        let stats = self.chain.process(&self.frame_in, &mut self.frame_out);

        let c = 1.0 - coeff(METER_TAU, FRAME_SECONDS);
        self.in_power += (power(&self.frame_in) - self.in_power) * c;
        self.out_power += (power(&self.frame_out) - self.out_power) * c;
        self.telemetry.record_frame(
            &stats,
            power_to_db(self.in_power),
            power_to_db(self.out_power),
            self.chain.agc_at_max(),
        );
        self.telemetry.publish(&stats);

        if let Some(out) = &mut self.out_stage {
            out.push(&self.frame_out, &self.telemetry);
        }
    }
}
