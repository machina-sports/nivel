//! The virtual microphone other apps select as their input.
//!
//! Nivel plays processed audio into a virtual *output*; the paired virtual
//! *input* is what Zoom, Meet, Discord, OBS and friends record from.
//!
//! - Linux: created on the fly with PulseAudio/PipeWire modules and removed
//!   on exit.
//! - macOS: uses BlackHole (free, open source) if installed.
//! - Windows: uses VB-CABLE (free) if installed.

use anyhow::Result;

/// A virtual microphone ready to be played into.
pub struct VirtualMic {
    /// Output device Nivel plays into.
    pub output: String,
    /// Name apps show for the microphone.
    pub mic_name: String,
    /// PulseAudio modules loaded by us, unloaded on drop.
    #[cfg_attr(not(target_os = "linux"), allow(dead_code))]
    modules: Vec<String>,
}

/// How to get a virtual microphone on this platform.
pub fn install_hint() -> &'static str {
    if cfg!(target_os = "macos") {
        "Install the free BlackHole driver, then run Nivel again:\n    brew install blackhole-2ch\n  (or download it from https://existential.audio/blackhole/)"
    } else if cfg!(windows) {
        "Install the free VB-CABLE driver from https://vb-audio.com/Cable/ (run the installer as administrator and reboot), then run Nivel again."
    } else {
        "Nivel creates its virtual microphone through PulseAudio or PipeWire (pipewire-pulse) using `pactl`; make sure one of them is running."
    }
}

impl VirtualMic {
    /// Find or create the virtual microphone.
    pub fn prepare(host: &cpal::Host) -> Result<Self> {
        #[cfg(target_os = "linux")]
        {
            let _ = host;
            linux::prepare()
        }
        #[cfg(not(target_os = "linux"))]
        {
            find_installed(host)
        }
    }
}

/// Look for a known virtual audio driver among the output devices.
#[cfg(not(target_os = "linux"))]
fn find_installed(host: &cpal::Host) -> Result<VirtualMic> {
    // (fragment of the output name, name of the matching mic if different)
    let known: &[(&str, Option<&str>)] = if cfg!(windows) {
        &[
            ("cable input", Some("CABLE Output (VB-Audio Virtual Cable)")),
            ("voicemeeter input", Some("Voicemeeter Out B1")),
        ]
    } else {
        &[
            ("blackhole", None),
            ("loopback audio", None),
            ("vb-cable", None),
        ]
    };
    let outputs = crate::devices::outputs(host)?;
    for (fragment, mic) in known {
        if let Some(o) = outputs
            .iter()
            .find(|o| o.name.to_lowercase().contains(fragment))
        {
            return Ok(VirtualMic {
                output: o.name.clone(),
                mic_name: mic.map_or_else(|| o.name.clone(), str::to_string),
                modules: Vec::new(),
            });
        }
    }
    anyhow::bail!("no virtual microphone is installed.\n  {}", install_hint())
}

#[cfg(target_os = "linux")]
mod linux {
    use anyhow::{Context, Result};

    use super::VirtualMic;
    use crate::os_volume::pactl;

    const SINK: &str = "nivel_sink";
    const SOURCE: &str = "nivel_mic";
    const SINK_LABEL: &str = "Nivel Output";
    const MIC_LABEL: &str = "Nivel Microphone";

    fn exists(kind: &str, name: &str) -> Result<bool> {
        Ok(pactl(&["list", "short", kind])?
            .lines()
            .any(|l| l.split('\t').nth(1) == Some(name)))
    }

    fn load(module: &str, args: &[String]) -> Result<String> {
        let mut argv = vec!["load-module", module];
        argv.extend(args.iter().map(String::as_str));
        Ok(pactl(&argv)?.trim().to_string())
    }

    pub(super) fn prepare() -> Result<VirtualMic> {
        let mut modules = Vec::new();
        if !exists("sinks", SINK)? {
            let id = load(
                "module-null-sink",
                &[
                    format!("sink_name={SINK}"),
                    format!("sink_properties='device.description=\"{SINK_LABEL}\"'"),
                ],
            )
            .context("creating the Nivel output sink")?;
            modules.push(id);
        }
        if !exists("sources", SOURCE)? {
            let id = load(
                "module-remap-source",
                &[
                    format!("master={SINK}.monitor"),
                    format!("source_name={SOURCE}"),
                    format!("source_properties='device.description=\"{MIC_LABEL}\"'"),
                ],
            )
            .context("creating the Nivel microphone")?;
            modules.push(id);
        }
        Ok(VirtualMic {
            output: SINK_LABEL.into(),
            mic_name: MIC_LABEL.into(),
            modules,
        })
    }

    impl Drop for VirtualMic {
        fn drop(&mut self) {
            for id in self.modules.iter().rev() {
                let _ = pactl(&["unload-module", id]);
            }
        }
    }
}
