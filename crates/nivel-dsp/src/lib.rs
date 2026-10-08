//! Real-time voice stabilization DSP used by [Nivel](https://github.com/machina-sports/nivel).
//!
//! Everything here is pure computation: no audio I/O, no allocation once a
//! processor is built, and no platform code. The processing chain works on
//! 10 ms mono frames at 48 kHz ([`FRAME`] samples), which is what the RNNoise
//! model expects.
//!
//! ```text
//! mic ─► high-pass ─► noise suppression ─► voice-gated AGC ─► soft gate ─► limiter ─► out
//!                         │ (voice prob.)        ▲                ▲
//!                         └──────────────────────┴────────────────┘
//! ```
//!
//! The [`Rider`] is a separate controller that steers the operating system's
//! own input volume instead of processing samples, for setups where no virtual
//! microphone is installed.

mod agc;
mod biquad;
mod chain;
mod denoise;
mod gate;
mod level;
mod limiter;
mod rider;
pub mod util;

pub use agc::{Agc, AgcConfig};
pub use biquad::Biquad;
pub use chain::{Chain, DspConfig, FrameStats};
pub use denoise::Denoiser;
pub use gate::Gate;
pub use level::{NoiseFloor, SpeechLevel};
pub use limiter::Limiter;
pub use rider::{Rider, RiderConfig};
pub use util::{FRAME, FRAME_SECONDS, SAMPLE_RATE};
