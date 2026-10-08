//! PulseAudio / PipeWire (through pipewire-pulse) via `pactl`.

use std::process::Command;

use anyhow::{Context, Result, bail};

use super::{InputVolume, best_match};

const DEFAULT_SOURCE: &str = "@DEFAULT_SOURCE@";

pub(crate) fn pactl(args: &[&str]) -> Result<String> {
    let out = Command::new("pactl")
        // Labels like "Name:" are translated otherwise.
        .env("LC_ALL", "C")
        .args(args)
        .output()
        .context(
            "running `pactl` (install pulseaudio-utils, or pipewire-pulse on PipeWire systems)",
        )?;
    if !out.status.success() {
        bail!(
            "`pactl {}` failed: {}",
            args.join(" "),
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

/// (description, name) of every source in `pactl list sources` output.
fn parse_sources(listing: &str) -> Vec<(String, String)> {
    let mut out = Vec::new();
    let mut name = None;
    for line in listing.lines() {
        let line = line.trim();
        if let Some(n) = line.strip_prefix("Name: ") {
            name = Some(n.to_string());
        } else if let Some(d) = line.strip_prefix("Description: ")
            && let Some(n) = name.take()
        {
            out.push((d.to_string(), n));
        }
    }
    out
}

/// First percentage in `pactl get-source-volume` output, as 0.0–1.5.
fn parse_volume(s: &str) -> Option<f32> {
    s.split_whitespace()
        .find_map(|tok| tok.strip_suffix('%').and_then(|p| p.parse::<f32>().ok()))
        .map(|p| p / 100.0)
}

fn parse_mute(s: &str) -> Option<bool> {
    let v = s.split(':').nth(1)?.trim();
    match v {
        "yes" => Some(true),
        "no" => Some(false),
        _ => None,
    }
}

struct PulseVolume {
    source: String,
    label: String,
}

pub fn open(device: Option<&str>) -> Result<Box<dyn InputVolume>> {
    let (source, label) = match device {
        None => {
            let name = pactl(&["get-default-source"])
                .map(|s| s.trim().to_string())
                .unwrap_or_default();
            let label = parse_sources(&pactl(&["list", "sources"])?)
                .into_iter()
                .find(|(_, n)| *n == name)
                .map(|(d, _)| d)
                .unwrap_or_else(|| "default microphone".into());
            (DEFAULT_SOURCE.to_string(), label)
        }
        Some(query) => {
            let sources = parse_sources(&pactl(&["list", "sources"])?);
            let by_description = best_match(&sources, query).cloned();
            let by_name = sources.iter().find(|(_, n)| n == query).cloned();
            let (d, n) = by_description
                .or(by_name)
                .with_context(|| format!("no PulseAudio source matches \"{query}\""))?;
            (n, d)
        }
    };
    let v = PulseVolume { source, label };
    v.volume()?; // fail early if pactl can't read it
    Ok(Box::new(v))
}

impl InputVolume for PulseVolume {
    fn device(&self) -> &str {
        &self.label
    }

    fn volume(&self) -> Result<f32> {
        let out = pactl(&["get-source-volume", &self.source])?;
        parse_volume(&out).with_context(|| format!("unexpected pactl output: {out}"))
    }

    fn set_volume(&self, volume: f32) -> Result<()> {
        let pct = format!("{}%", (volume.clamp(0.0, 1.0) * 100.0).round() as u32);
        pactl(&["set-source-volume", &self.source, &pct]).map(drop)
    }

    fn muted(&self) -> Result<Option<bool>> {
        Ok(parse_mute(&pactl(&["get-source-mute", &self.source])?))
    }

    fn set_muted(&self, muted: bool) -> Result<()> {
        pactl(&[
            "set-source-mute",
            &self.source,
            if muted { "1" } else { "0" },
        ])
        .map(drop)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_pactl_output() {
        let listing = "Source #0\n\tState: SUSPENDED\n\tName: alsa_output.pci.monitor\n\tDescription: Monitor of Built-in Audio\n\
                       Source #1\n\tState: RUNNING\n\tName: alsa_input.usb-Shure\n\tDescription: Shure MV7 Mono\n";
        let s = parse_sources(listing);
        assert_eq!(
            s[1],
            ("Shure MV7 Mono".into(), "alsa_input.usb-Shure".into())
        );
        assert_eq!(
            parse_volume(
                "Volume: front-left: 52429 /  80% / -5.81 dB,   front-right: 52429 /  80% / -5.81 dB"
            ),
            Some(0.8)
        );
        assert_eq!(parse_mute("Mute: no"), Some(false));
        assert_eq!(parse_mute("Mute: yes\n"), Some(true));
    }
}
