use std::sync::atomic::Ordering;
use std::thread;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use nivel_core::os_volume;
use nivel_core::{DspConfig, EngineConfig, Event, Supervisor};
use nivel_dsp::{Rider, RiderConfig};

use crate::ui;

/// How often to re-read the system volume to notice other apps changing it.
const SYNC_EVERY: Duration = Duration::from_secs(2);

pub fn run(input: Option<String>, target: f32, max_volume: u8, restore: bool) -> Result<()> {
    let running = crate::running_flag();
    let control = os_volume::open(input.as_deref())?;
    let original = control
        .volume()
        .context("reading the system input volume")?;

    println!(
        "Nivel ride · keeping \"{}\" at {target:.0} dBFS for every app",
        control.device()
    );
    println!("Talk normally; the system mic volume follows you. Ctrl+C to stop.\n");

    if control.muted().ok().flatten() == Some(true) {
        control.set_muted(false)?;
        ui::say("  ✔ the microphone was muted in the system settings: unmuted it");
    }
    let mut volume = original;
    if volume < 0.05 {
        volume = 0.5;
        control.set_volume(volume)?;
        ui::say("  ✔ the system input volume was at zero: raised it to 50%");
    }

    let cfg = RiderConfig {
        target_db: target.min(-6.0),
        max_volume: (f32::from(max_volume) / 100.0).clamp(0.05, 1.0),
        ..RiderConfig::default()
    };
    let mut rider = Rider::new(cfg, volume);

    let engine = EngineConfig {
        input,
        output: None,
        channel: None,
        dsp: DspConfig::default(),
    };
    let mut sup = Supervisor::new(engine, false);
    let mut frames = sup.telemetry().subscribe();
    let mut alerts = ui::AlertThrottle::new(Duration::from_secs(30));
    let mut last_sync = Instant::now();
    let mut external_changes = 0u32;
    let mut starved_warned = false;

    while running.load(Ordering::SeqCst) {
        let status = sup.tick();
        for e in &status.events {
            if !matches!(e, Event::Started { .. }) || status.restarts > 0 {
                ui::say(format!("  · {e}"));
            }
        }
        // The rider handles clipping itself; only surface the other alerts.
        alerts.show(
            &status
                .alerts
                .iter()
                .filter(|a| {
                    !matches!(
                        a,
                        nivel_core::Alert::Clipping { .. } | nivel_core::Alert::TooQuiet { .. }
                    )
                })
                .cloned()
                .collect::<Vec<_>>(),
        );

        if last_sync.elapsed() >= SYNC_EVERY {
            last_sync = Instant::now();
            if let Ok(actual) = control.volume()
                && (actual - rider.volume()).abs() > 0.02
            {
                external_changes += 1;
                rider.sync_volume(actual);
                if external_changes == 3 {
                    ui::say(
                        "  ! another app keeps changing the mic volume too (often Zoom/Meet/Teams \"automatically adjust\"); turn that off so they don't fight",
                    );
                }
            }
        }

        while let Ok(f) = frames.pop() {
            if let Some(v) = rider.update(&f) {
                let before = volume;
                control.set_volume(v)?;
                volume = v;
                let why = rider.speech_db().map_or_else(
                    || "clipping".to_string(),
                    |l| format!("voice {l:.0} dB, target {:.0} dB", rider.config().target_db),
                );
                ui::say(format!(
                    "  {} volume {:.0}% → {:.0}%  ({why})",
                    if v > before { "↑" } else { "↓" },
                    before * 100.0,
                    v * 100.0
                ));
            }
        }
        if rider.starved() && !starved_warned {
            starved_warned = true;
            ui::say(
                "  ! the system volume is maxed out and your voice is still quiet: move closer or raise the mic's hardware gain",
            );
        }

        let s = &status.snapshot;
        ui::status(&format!(
            "  {} {}  vol {:>3.0}%  {}",
            ui::meter(s.in_db, 24),
            ui::db(s.in_db),
            rider.volume() * 100.0,
            if s.voiced { "● voice" } else { "" },
        ));
        thread::sleep(Duration::from_millis(100));
    }

    sup.shutdown();
    ui::clear_line();
    if restore {
        control.set_volume(original)?;
        println!(
            "Restored the system input volume to {:.0}%.",
            original * 100.0
        );
    } else {
        println!("Left the system input volume at {:.0}%.", volume * 100.0);
    }
    Ok(())
}
