use anyhow::Result;
use nivel_core::devices::{self, DeviceInfo};
use nivel_core::os_volume;
use nivel_core::virtual_mic::VirtualMic;

fn print_list(title: &str, list: &[DeviceInfo]) {
    println!("{title}");
    if list.is_empty() {
        println!("    (none)");
    }
    for d in list {
        let format = match (d.sample_rate, d.channels) {
            (Some(r), Some(c)) => format!("{:.1} kHz · {c} ch", r as f32 / 1000.0),
            _ => String::new(),
        };
        let mut tags = Vec::new();
        if d.is_default {
            tags.push("default");
        }
        if d.is_virtual {
            tags.push("virtual");
        }
        println!(
            "  {} {:<44} {:<16} {}",
            if d.is_default { "●" } else { " " },
            d.name,
            format,
            tags.join(", ")
        );
    }
    println!();
}

pub fn run() -> Result<()> {
    let host = nivel_core::cpal::default_host();
    println!("Audio system: {}\n", host.id().name());
    print_list("Microphones", &devices::inputs(&host)?);
    print_list("Outputs", &devices::outputs(&host)?);

    match os_volume::open(None) {
        Ok(v) => {
            let volume = v
                .volume()
                .map(|x| format!("{:.0}%", x * 100.0))
                .unwrap_or_else(|_| "unknown".into());
            let muted = match v.muted() {
                Ok(Some(true)) => " · MUTED",
                _ => "",
            };
            println!("System input volume: {volume}{muted} (\"{}\")", v.device());
        }
        Err(e) => println!("System input volume: not adjustable ({e:#})"),
    }

    // On Linux this would create the virtual mic just to report it; describe it instead.
    if cfg!(target_os = "linux") {
        println!("Virtual microphone: created automatically by `nivel run` (\"Nivel Microphone\")");
        return Ok(());
    }
    match VirtualMic::prepare(&host) {
        Ok(vm) => println!(
            "Virtual microphone: ready — apps should select \"{}\"",
            vm.mic_name
        ),
        Err(e) => println!("Virtual microphone: {e:#}"),
    }
    Ok(())
}
