//! Device discovery and selection by (partial) name.

use std::fmt;

use anyhow::{Context, Result};
use cpal::traits::{DeviceTrait, HostTrait};

/// A device as shown to users.
#[derive(Debug, Clone)]
pub struct DeviceInfo {
    pub name: String,
    pub is_default: bool,
    pub sample_rate: Option<u32>,
    pub channels: Option<u16>,
    pub is_virtual: bool,
}

/// No device matched the requested name.
#[derive(Debug)]
pub struct DeviceNotFound {
    pub query: String,
    pub available: Vec<String>,
    pub kind: &'static str,
}

impl fmt::Display for DeviceNotFound {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "no {} device matches \"{}\"", self.kind, self.query)?;
        if !self.available.is_empty() {
            write!(f, " (available: {})", self.available.join(", "))?;
        }
        Ok(())
    }
}

impl std::error::Error for DeviceNotFound {}

/// Name fragments of loopback/virtual devices. Used to avoid feeding a
/// virtual mic back into itself and to find one to play into.
const VIRTUAL_HINTS: &[&str] = &[
    "blackhole",
    "loopback",
    "soundflower",
    "vb-audio",
    "vb-cable",
    "cable input",
    "cable output",
    "voicemeeter",
    "virtual",
    "nivel",
    "monitor of",
    "teams audio",
    "zoomaudio",
    "krisp",
];

pub fn is_virtual(name: &str) -> bool {
    let lower = name.to_lowercase();
    VIRTUAL_HINTS.iter().any(|h| lower.contains(h))
}

pub fn name_of(device: &cpal::Device) -> String {
    device
        .description()
        .map(|d| d.name().to_string())
        .unwrap_or_else(|_| device.to_string())
}

fn info(device: &cpal::Device, default_name: Option<&str>, input: bool) -> DeviceInfo {
    let name = name_of(device);
    let config = if input {
        device.default_input_config()
    } else {
        device.default_output_config()
    };
    DeviceInfo {
        is_default: default_name == Some(name.as_str()),
        sample_rate: config.as_ref().ok().map(|c| c.sample_rate()),
        channels: config.as_ref().ok().map(|c| c.channels()),
        is_virtual: is_virtual(&name),
        name,
    }
}

pub fn inputs(host: &cpal::Host) -> Result<Vec<DeviceInfo>> {
    let default = host.default_input_device().map(|d| name_of(&d));
    Ok(host
        .input_devices()
        .context("listing input devices")?
        .map(|d| info(&d, default.as_deref(), true))
        .collect())
}

pub fn outputs(host: &cpal::Host) -> Result<Vec<DeviceInfo>> {
    let default = host.default_output_device().map(|d| name_of(&d));
    Ok(host
        .output_devices()
        .context("listing output devices")?
        .map(|d| info(&d, default.as_deref(), false))
        .collect())
}

/// Pick by exact name (case-insensitive), then by substring.
fn pick(devices: Vec<cpal::Device>, query: &str, kind: &'static str) -> Result<cpal::Device> {
    let q = query.to_lowercase();
    let named: Vec<(String, cpal::Device)> =
        devices.into_iter().map(|d| (name_of(&d), d)).collect();
    if let Some(i) = named.iter().position(|(n, _)| n.to_lowercase() == q) {
        return Ok(named
            .into_iter()
            .nth(i)
            .map(|(_, d)| d)
            .expect("index is in range"));
    }
    if let Some(i) = named
        .iter()
        .position(|(n, _)| n.to_lowercase().contains(&q))
    {
        return Ok(named
            .into_iter()
            .nth(i)
            .map(|(_, d)| d)
            .expect("index is in range"));
    }
    Err(DeviceNotFound {
        query: query.to_string(),
        available: named.into_iter().map(|(n, _)| n).collect(),
        kind,
    }
    .into())
}

/// The selected microphone plus anything the user should know about the choice.
pub struct SelectedInput {
    pub device: cpal::Device,
    pub name: String,
    pub note: Option<String>,
}

/// Resolve the microphone to capture. With no query this is the system
/// default, unless the default is itself a virtual device (for example the
/// Nivel mic was made the system default), in which case the first real
/// microphone is used so we never capture our own output.
pub fn select_input(host: &cpal::Host, query: Option<&str>) -> Result<SelectedInput> {
    if let Some(q) = query {
        let device = pick(
            host.input_devices()
                .context("listing input devices")?
                .collect(),
            q,
            "input",
        )?;
        let name = name_of(&device);
        return Ok(SelectedInput {
            device,
            name,
            note: None,
        });
    }
    let default = host
        .default_input_device()
        .context("no default microphone; pass --input to choose one")?;
    let default_name = name_of(&default);
    if !is_virtual(&default_name) {
        return Ok(SelectedInput {
            device: default,
            name: default_name,
            note: None,
        });
    }
    let real = host
        .input_devices()
        .context("listing input devices")?
        .find(|d| !is_virtual(&name_of(d)))
        .with_context(|| {
            format!(
                "the default input \"{default_name}\" is virtual and no real microphone was found"
            )
        })?;
    let name = name_of(&real);
    Ok(SelectedInput {
        note: Some(format!(
            "system default input \"{default_name}\" is a virtual device; capturing \"{name}\" instead"
        )),
        device: real,
        name,
    })
}

pub fn select_output(host: &cpal::Host, query: &str) -> Result<cpal::Device> {
    pick(
        host.output_devices()
            .context("listing output devices")?
            .collect(),
        query,
        "output",
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recognizes_virtual_devices() {
        for n in [
            "BlackHole 2ch",
            "CABLE Input (VB-Audio Virtual Cable)",
            "Nivel Microphone",
            "Monitor of Built-in Audio",
            "Microsoft Teams Audio",
            "ZoomAudioDevice",
        ] {
            assert!(is_virtual(n), "{n}");
        }
        for n in [
            "MacBook Pro Microphone",
            "Microphone (Realtek(R) Audio)",
            "Shure MV7",
        ] {
            assert!(!is_virtual(n), "{n}");
        }
    }
}
