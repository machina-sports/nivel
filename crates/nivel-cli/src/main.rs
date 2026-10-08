//! `nivel` — keep your microphone on the level.

mod devices;
mod doctor;
mod process;
mod ride;
mod run;
mod ui;

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use anyhow::Result;
use clap::{Args, Parser, Subcommand};
use nivel_core::DspConfig;

#[derive(Parser)]
#[command(
    name = "nivel",
    version,
    about = "Keep your microphone on the level: no dropouts, no blasts, no whispers.",
    long_about = "Nivel stabilizes your microphone for every app on Windows, macOS and Linux.\n\n\
                  Start with `nivel doctor` to see what your mic needs, then use `nivel ride` \
                  (auto-level the system mic volume) or `nivel run` (noise suppression and leveling \
                  into a virtual microphone).",
    after_help = "Docs and issues: https://github.com/machina-sports/nivel"
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// List microphones, outputs and the virtual microphone status
    Devices,
    /// Listen to your mic for a few seconds and explain what's wrong
    Doctor {
        /// Microphone to check (name or part of it); default: system default
        #[arg(short, long)]
        input: Option<String>,
        /// How long to listen
        #[arg(long, default_value_t = 10)]
        seconds: u32,
    },
    /// Auto-level the system microphone volume, for every app, without any driver
    Ride {
        /// Microphone to control (name or part of it); default: system default
        #[arg(short, long)]
        input: Option<String>,
        /// Speech level to aim for, in dBFS
        #[arg(long, default_value_t = -24.0, allow_negative_numbers = true)]
        target: f32,
        /// Never raise the system volume above this, in percent
        #[arg(long, default_value_t = 100)]
        max_volume: u8,
        /// Put the original volume back on exit
        #[arg(long)]
        restore: bool,
    },
    /// Clean up and level your voice into a virtual microphone
    Run {
        /// Microphone to capture (name or part of it); default: system default
        #[arg(short, long)]
        input: Option<String>,
        /// Where to play the processed voice; default: the virtual microphone
        #[arg(short, long)]
        output: Option<String>,
        /// Capture only this input channel (1-based), for multi-input interfaces
        #[arg(long)]
        channel: Option<usize>,
        /// Wait for the chosen mic instead of using the default one while it's unplugged
        #[arg(long)]
        no_fallback: bool,
        #[command(flatten)]
        dsp: DspArgs,
    },
    /// Process a WAV file with the same chain (output: 48 kHz mono WAV)
    Process {
        input: PathBuf,
        output: PathBuf,
        /// Write 32-bit float instead of 16-bit PCM
        #[arg(long)]
        float: bool,
        #[command(flatten)]
        dsp: DspArgs,
    },
}

#[derive(Args, Clone)]
struct DspArgs {
    /// Speech level to aim for, in dBFS
    #[arg(long, default_value_t = -20.0, allow_negative_numbers = true, help_heading = "Processing")]
    target: f32,
    /// Most boost a quiet voice may get, in dB
    #[arg(long, default_value_t = 30.0, help_heading = "Processing")]
    max_gain: f32,
    /// Noise suppression strength, 0–100 %
    #[arg(long, default_value_t = 100, value_parser = clap::value_parser!(u8).range(0..=100), help_heading = "Processing")]
    strength: u8,
    /// Turn off noise suppression
    #[arg(long, help_heading = "Processing")]
    no_denoise: bool,
    /// Turn off automatic gain control
    #[arg(long, help_heading = "Processing")]
    no_agc: bool,
    /// How far to turn down the gaps between words, in dB (0 = off)
    #[arg(long, default_value_t = 10.0, help_heading = "Processing")]
    gate: f32,
    /// Hard output ceiling, in dBFS
    #[arg(long, default_value_t = -1.0, allow_negative_numbers = true, help_heading = "Processing")]
    ceiling: f32,
    /// Rumble filter cutoff in Hz (0 = off)
    #[arg(long, default_value_t = 80.0, help_heading = "Processing")]
    highpass: f32,
}

impl From<DspArgs> for DspConfig {
    fn from(a: DspArgs) -> Self {
        DspConfig {
            highpass_hz: a.highpass.max(0.0),
            denoise: !a.no_denoise,
            denoise_mix: f32::from(a.strength) / 100.0,
            agc: !a.no_agc,
            target_db: a.target.min(-3.0),
            max_gain_db: a.max_gain.max(0.0),
            gate_range_db: a.gate.max(0.0),
            ceiling_db: a.ceiling.min(0.0),
        }
    }
}

/// Set to false by Ctrl+C.
fn running_flag() -> Arc<AtomicBool> {
    let running = Arc::new(AtomicBool::new(true));
    let r = running.clone();
    let _ = ctrlc::set_handler(move || r.store(false, Ordering::SeqCst));
    running
}

fn main() {
    let cli = Cli::parse();
    let result: Result<()> = match cli.command {
        Command::Devices => devices::run(),
        Command::Doctor { input, seconds } => doctor::run(input, seconds.clamp(3, 120)),
        Command::Ride {
            input,
            target,
            max_volume,
            restore,
        } => ride::run(input, target, max_volume, restore),
        Command::Run {
            input,
            output,
            channel,
            no_fallback,
            dsp,
        } => run::run(input, output, channel, !no_fallback, dsp.into()),
        Command::Process {
            input,
            output,
            float,
            dsp,
        } => process::run(&input, &output, float, &dsp.into()),
    };
    if let Err(e) = result {
        ui::clear_line();
        eprintln!("error: {e:#}");
        std::process::exit(1);
    }
}
