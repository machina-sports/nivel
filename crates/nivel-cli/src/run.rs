use std::sync::atomic::Ordering;
use std::thread;
use std::time::{Duration, Instant};

use anyhow::Result;
use nivel_core::devices;
use nivel_core::supervisor::Phase;
use nivel_core::virtual_mic::VirtualMic;
use nivel_core::{DspConfig, EngineConfig, Supervisor};

use crate::ui;

pub fn run(
    input: Option<String>,
    output: Option<String>,
    channel: Option<usize>,
    fallback: bool,
    dsp: DspConfig,
) -> Result<()> {
    let running = crate::running_flag();
    let host = nivel_core::cpal::default_host();

    // Keep the virtual mic alive (on Linux it is removed when dropped).
    let (output, mic_name, virtual_mic) = match output {
        Some(o) => (o, None, None),
        None => {
            let vm = VirtualMic::prepare(&host)?;
            (vm.output.clone(), Some(vm.mic_name.clone()), Some(vm))
        }
    };

    println!(
        "Nivel {} · your mic, on the level",
        env!("CARGO_PKG_VERSION")
    );
    match &mic_name {
        Some(m) => {
            println!("In Zoom, Meet, Teams, Discord, OBS… choose \"{m}\" as the microphone.")
        }
        None if !devices::is_virtual(&output) => {
            println!(
                "Playing to \"{output}\": use headphones, or the speakers will feed back into the mic."
            )
        }
        None => {}
    }
    println!("Ctrl+C to stop.\n");

    let cfg = EngineConfig {
        input,
        output: Some(output),
        channel: channel.map(|c| c.saturating_sub(1)),
        dsp,
    };
    let mut sup = Supervisor::new(cfg, fallback);
    let mut alerts = ui::AlertThrottle::new(Duration::from_secs(30));
    let started = Instant::now();
    let (mut restarts, mut glitches) = (0, 0);

    while running.load(Ordering::SeqCst) {
        let status = sup.tick();
        for e in &status.events {
            ui::say(format!("  · {e}"));
        }
        alerts.show(&status.alerts);

        let s = &status.snapshot;
        let line = match &status.phase {
            Phase::Running => format!(
                "  in {} {}  out {} {}  {:+5.1} dB {}",
                ui::meter(s.in_db, 12),
                ui::db(s.in_db),
                ui::meter(s.out_db, 12),
                ui::db(s.out_db),
                s.agc_db,
                if s.voiced { "● voice" } else { "" },
            ),
            Phase::Recovering { reason } => format!("  … waiting for audio: {reason}"),
        };
        ui::status(&line);
        restarts = status.restarts;
        glitches = s.glitches();
        thread::sleep(Duration::from_millis(100));
    }

    sup.shutdown();
    drop(virtual_mic);
    ui::clear_line();
    println!(
        "Stopped after {}. Reconnects: {}. Glitches: {}.",
        ui::duration(started.elapsed()),
        restarts,
        glitches
    );
    Ok(())
}
