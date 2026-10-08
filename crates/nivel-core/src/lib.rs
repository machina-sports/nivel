//! Audio engine for [Nivel](https://github.com/machina-sports/nivel).
//!
//! - [`engine`] captures a microphone, runs the [`nivel_dsp::Chain`] on a
//!   worker thread and plays the result into an output device (usually a
//!   virtual microphone).
//! - [`supervisor`] keeps an engine alive: it restarts it when a device
//!   disappears or stalls, falls back to the default mic and switches back
//!   when the preferred one returns.
//! - [`os_volume`] reads and sets the operating system's own input volume and
//!   mute, which the "ride" mode steers without any virtual device.
//! - [`virtual_mic`] finds (or on Linux, creates) the virtual microphone that
//!   other apps select as their input.
//! - [`offline`] runs the chain over an in-memory clip.

pub mod devices;
pub mod engine;
pub mod offline;
pub mod os_volume;
pub mod supervisor;
pub mod telemetry;
pub mod virtual_mic;
pub mod watchdog;

pub use engine::{Engine, EngineConfig, EngineInfo};
pub use supervisor::{Event, Phase, Status, Supervisor};
pub use telemetry::{Snapshot, Telemetry};
pub use watchdog::Alert;

pub use nivel_dsp::{DspConfig, FrameStats};

/// The audio backend crate, re-exported so callers use the same version.
pub use cpal;
