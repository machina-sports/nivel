//! Lock-free meters and counters shared between the audio threads and the UI.

use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, AtomicUsize, Ordering::Relaxed};
use std::time::Instant;

use nivel_dsp::FrameStats;
use nivel_dsp::util::{SILENCE_DB, lin_to_db};
use rtrb::{Consumer, Producer, RingBuffer};

struct AtomicF32(AtomicU32);

impl AtomicF32 {
    fn new(v: f32) -> Self {
        Self(AtomicU32::new(v.to_bits()))
    }
    fn load(&self) -> f32 {
        f32::from_bits(self.0.load(Relaxed))
    }
    fn store(&self, v: f32) {
        self.0.store(v.to_bits(), Relaxed);
    }
}

/// Written by the audio callbacks and the DSP worker, read by whoever draws
/// meters or supervises the engine. All updates are wait-free.
pub struct Telemetry {
    epoch: Instant,
    in_db: AtomicF32,
    out_db: AtomicF32,
    // Peaks are non-negative floats, whose bit patterns sort like the values,
    // so `fetch_max` on the raw bits holds the loudest peak between reads.
    in_peak: AtomicU32,
    out_peak: AtomicU32,
    vad: AtomicF32,
    voiced: AtomicBool,
    agc_db: AtomicF32,
    agc_at_max: AtomicBool,
    gate_db: AtomicF32,
    limiter_db: AtomicF32,
    floor_db: AtomicF32,
    frames: AtomicU64,
    voiced_frames: AtomicU64,
    clipped_frames: AtomicU64,
    silent_frames: AtomicU64,
    quiet_voiced_frames: AtomicU64,
    input_overruns: AtomicU64,
    output_underruns: AtomicU64,
    output_dropped: AtomicU64,
    xruns: AtomicU64,
    last_input_ms: AtomicU64,
    last_output_ms: AtomicU64,
    output_callback_frames: AtomicUsize,
    error: Mutex<Option<String>>,
    frames_tx: Mutex<Option<Producer<FrameStats>>>,
}

/// A copy of the telemetry at one instant.
#[derive(Debug, Clone, Default)]
pub struct Snapshot {
    /// Smoothed input level, dBFS RMS.
    pub in_db: f32,
    /// Loudest input peak since the previous snapshot, dBFS.
    pub in_peak_db: f32,
    /// Smoothed output level, dBFS RMS.
    pub out_db: f32,
    /// Loudest output peak since the previous snapshot, dBFS.
    pub out_peak_db: f32,
    pub vad: f32,
    pub voiced: bool,
    pub agc_db: f32,
    pub agc_at_max: bool,
    pub gate_db: f32,
    pub limiter_db: f32,
    pub floor_db: f32,
    pub frames: u64,
    pub voiced_frames: u64,
    pub clipped_frames: u64,
    pub silent_frames: u64,
    /// Voiced frames during which the AGC was already at maximum boost.
    pub quiet_voiced_frames: u64,
    pub input_overruns: u64,
    pub output_underruns: u64,
    pub output_dropped: u64,
    pub xruns: u64,
    /// Milliseconds since the microphone last delivered audio.
    pub input_age_ms: u64,
    /// Milliseconds since the output device last asked for audio.
    pub output_age_ms: u64,
}

impl Snapshot {
    /// Audio glitches of any kind counted so far.
    pub fn glitches(&self) -> u64 {
        self.input_overruns + self.output_underruns + self.output_dropped + self.xruns
    }
}

impl Default for Telemetry {
    fn default() -> Self {
        Self {
            epoch: Instant::now(),
            in_db: AtomicF32::new(SILENCE_DB),
            out_db: AtomicF32::new(SILENCE_DB),
            in_peak: AtomicU32::new(0),
            out_peak: AtomicU32::new(0),
            vad: AtomicF32::new(0.0),
            voiced: AtomicBool::new(false),
            agc_db: AtomicF32::new(0.0),
            agc_at_max: AtomicBool::new(false),
            gate_db: AtomicF32::new(0.0),
            limiter_db: AtomicF32::new(0.0),
            floor_db: AtomicF32::new(SILENCE_DB),
            frames: AtomicU64::new(0),
            voiced_frames: AtomicU64::new(0),
            clipped_frames: AtomicU64::new(0),
            silent_frames: AtomicU64::new(0),
            quiet_voiced_frames: AtomicU64::new(0),
            input_overruns: AtomicU64::new(0),
            output_underruns: AtomicU64::new(0),
            output_dropped: AtomicU64::new(0),
            xruns: AtomicU64::new(0),
            last_input_ms: AtomicU64::new(0),
            last_output_ms: AtomicU64::new(0),
            output_callback_frames: AtomicUsize::new(0),
            error: Mutex::new(None),
            frames_tx: Mutex::new(None),
        }
    }
}

impl Telemetry {
    fn now_ms(&self) -> u64 {
        self.epoch.elapsed().as_millis() as u64
    }

    /// Called when a new engine starts, so stall detection measures from now.
    pub(crate) fn mark_start(&self) {
        let now = self.now_ms();
        self.last_input_ms.store(now, Relaxed);
        self.last_output_ms.store(now, Relaxed);
        self.output_callback_frames.store(0, Relaxed);
    }

    pub(crate) fn record_frame(&self, s: &FrameStats, in_db: f32, out_db: f32, agc_at_max: bool) {
        self.in_db.store(in_db);
        self.out_db.store(out_db);
        self.in_peak
            .fetch_max(nivel_dsp::util::db_to_lin(s.in_peak_db).to_bits(), Relaxed);
        self.out_peak
            .fetch_max(nivel_dsp::util::db_to_lin(s.out_peak_db).to_bits(), Relaxed);
        self.vad.store(s.vad);
        self.voiced.store(s.voiced, Relaxed);
        self.agc_db.store(s.agc_db);
        self.agc_at_max.store(agc_at_max, Relaxed);
        self.gate_db.store(s.gate_db);
        self.limiter_db.store(s.limiter_db);
        self.floor_db.store(s.floor_db);
        self.frames.fetch_add(1, Relaxed);
        if s.voiced {
            self.voiced_frames.fetch_add(1, Relaxed);
            if agc_at_max {
                self.quiet_voiced_frames.fetch_add(1, Relaxed);
            }
        }
        if s.clipped {
            self.clipped_frames.fetch_add(1, Relaxed);
        }
        if s.silent {
            self.silent_frames.fetch_add(1, Relaxed);
        }
    }

    /// Receive every frame's [`FrameStats`] (10 s of backlog). Replaces any
    /// previous subscriber.
    pub fn subscribe(&self) -> Consumer<FrameStats> {
        let (tx, rx) = RingBuffer::new(1_000);
        if let Ok(mut slot) = self.frames_tx.lock() {
            *slot = Some(tx);
        }
        rx
    }

    /// Called by the DSP worker (not an audio callback), so a short lock is fine.
    pub(crate) fn publish(&self, stats: &FrameStats) {
        if let Ok(mut slot) = self.frames_tx.try_lock()
            && let Some(tx) = slot.as_mut()
        {
            let _ = tx.push(*stats);
        }
    }

    pub(crate) fn input_callback(&self, overrun: bool) {
        self.last_input_ms.store(self.now_ms(), Relaxed);
        if overrun {
            self.input_overruns.fetch_add(1, Relaxed);
        }
    }

    pub(crate) fn output_callback(&self, frames: usize, underrun: bool) {
        self.last_output_ms.store(self.now_ms(), Relaxed);
        self.output_callback_frames.fetch_max(frames, Relaxed);
        if underrun {
            self.output_underruns.fetch_add(1, Relaxed);
        }
    }

    pub(crate) fn output_callback_frames(&self) -> usize {
        self.output_callback_frames.load(Relaxed)
    }

    pub(crate) fn output_dropped(&self, samples: usize) {
        self.output_dropped.fetch_add(samples as u64, Relaxed);
    }

    pub(crate) fn stream_error(&self, err: cpal::Error) {
        if err.kind() == cpal::ErrorKind::Xrun {
            self.xruns.fetch_add(1, Relaxed);
            return;
        }
        // The stream follows the new default device by itself.
        if err.kind() == cpal::ErrorKind::DeviceChanged {
            return;
        }
        if let Ok(mut slot) = self.error.lock() {
            slot.get_or_insert_with(|| err.to_string());
        }
    }

    /// The first fatal stream error since the last call, if any.
    pub fn take_error(&self) -> Option<String> {
        self.error.lock().ok().and_then(|mut e| e.take())
    }

    /// Read every meter and counter. Peak meters reset on each call.
    pub fn snapshot(&self) -> Snapshot {
        let now = self.now_ms();
        Snapshot {
            in_db: self.in_db.load(),
            in_peak_db: lin_to_db(f32::from_bits(self.in_peak.swap(0, Relaxed))),
            out_db: self.out_db.load(),
            out_peak_db: lin_to_db(f32::from_bits(self.out_peak.swap(0, Relaxed))),
            vad: self.vad.load(),
            voiced: self.voiced.load(Relaxed),
            agc_db: self.agc_db.load(),
            agc_at_max: self.agc_at_max.load(Relaxed),
            gate_db: self.gate_db.load(),
            limiter_db: self.limiter_db.load(),
            floor_db: self.floor_db.load(),
            frames: self.frames.load(Relaxed),
            voiced_frames: self.voiced_frames.load(Relaxed),
            clipped_frames: self.clipped_frames.load(Relaxed),
            silent_frames: self.silent_frames.load(Relaxed),
            quiet_voiced_frames: self.quiet_voiced_frames.load(Relaxed),
            input_overruns: self.input_overruns.load(Relaxed),
            output_underruns: self.output_underruns.load(Relaxed),
            output_dropped: self.output_dropped.load(Relaxed),
            xruns: self.xruns.load(Relaxed),
            input_age_ms: now.saturating_sub(self.last_input_ms.load(Relaxed)),
            output_age_ms: now.saturating_sub(self.last_output_ms.load(Relaxed)),
        }
    }
}
