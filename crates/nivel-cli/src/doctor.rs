use std::thread;
use std::time::{Duration, Instant};

use anyhow::{Result, bail};
use nivel_core::os_volume;
use nivel_core::supervisor::Phase;
use nivel_core::{DspConfig, EngineConfig, FrameStats, Supervisor};
use nivel_dsp::util::{db_to_lin, power_to_db};

use crate::ui;

#[derive(PartialEq, PartialOrd, Clone, Copy)]
enum Grade {
    Good,
    Warn,
    Bad,
}

/// What a finding is about, which decides the advice at the end.
#[derive(PartialEq, Clone, Copy)]
enum Area {
    /// System volume or mute: `ride` fixes it.
    System,
    /// Voice level or clipping: `ride` or `run` fix it.
    Level,
    /// Background noise: `run` fixes it.
    Noise,
    /// Audio not arriving or dropping out: hardware, permissions, drivers.
    Delivery,
}

struct Finding {
    grade: Grade,
    area: Area,
    text: String,
}

fn found(grade: Grade, area: Area, text: impl Into<String>) -> Finding {
    Finding {
        grade,
        area,
        text: text.into(),
    }
}

fn mean_db(values: impl Iterator<Item = f32>) -> Option<f32> {
    let p: Vec<f32> = values.map(|db| db_to_lin(db).powi(2)).collect();
    (!p.is_empty()).then(|| power_to_db(p.iter().sum::<f32>() / p.len() as f32))
}

fn percentile(mut v: Vec<f32>, q: f32) -> Option<f32> {
    if v.is_empty() {
        return None;
    }
    v.sort_by(f32::total_cmp);
    Some(v[((v.len() - 1) as f32 * q).round() as usize])
}

pub fn run(input: Option<String>, seconds: u32) -> Result<()> {
    let running = crate::running_flag();
    let cfg = EngineConfig {
        input: input.clone(),
        output: None,
        channel: None,
        dsp: DspConfig::default(),
    };
    let mut sup = Supervisor::new(cfg, false);
    let mut rx_frames = sup.telemetry().subscribe();

    // Wait for the mic to open (or fail clearly).
    let opened = Instant::now();
    let mic = loop {
        let status = sup.tick();
        if let Some(info) = status.info {
            break info.input;
        }
        if opened.elapsed() > Duration::from_secs(6) {
            let reason = match status.phase {
                Phase::Recovering { reason } => reason,
                Phase::Running => "unknown error".into(),
            };
            bail!("could not open the microphone: {reason}");
        }
        thread::sleep(Duration::from_millis(100));
    };

    println!("Nivel doctor · listening to \"{mic}\" for {seconds} s");
    println!("Talk the way you do in calls: read something out loud, count to twenty…\n");

    let mut frames: Vec<FrameStats> = Vec::with_capacity(seconds as usize * 100);
    let start = Instant::now();
    let total = Duration::from_secs(seconds.into());
    let mut restarts = 0;
    while start.elapsed() < total && running.load(std::sync::atomic::Ordering::SeqCst) {
        let status = sup.tick();
        restarts = status.restarts;
        while let Ok(f) = rx_frames.pop() {
            frames.push(f);
        }
        let left = total.saturating_sub(start.elapsed()).as_secs() + 1;
        let s = &status.snapshot;
        ui::status(&format!(
            "  {} {}  {}  {left:>2}s left",
            ui::meter(s.in_db, 24),
            ui::db(s.in_db),
            if s.voiced { "● voice" } else { "  ·    " },
        ));
        thread::sleep(Duration::from_millis(100));
    }
    let glitches = sup.tick().snapshot.glitches();
    sup.shutdown();
    while let Ok(f) = rx_frames.pop() {
        frames.push(f);
    }
    ui::clear_line();

    let mut findings = Vec::new();
    let n = frames.len();
    if n == 0 {
        findings.push(found(
            Grade::Bad,
            Area::Delivery,
            "No audio arrived from the microphone at all.",
        ));
    }

    // System volume and mute.
    let mut can_ride = false;
    match os_volume::open(input.as_deref()) {
        Ok(v) => {
            can_ride = true;
            if v.muted().ok().flatten() == Some(true) {
                findings.push(found(Grade::Bad, Area::System, "The microphone is MUTED in the system settings. `nivel ride` unmutes it."));
            }
            if let Ok(vol) = v.volume() {
                let pct = vol * 100.0;
                if vol < 0.1 {
                    findings.push(found(Grade::Bad, Area::System, format!("System input volume is almost zero ({pct:.0}%).")));
                } else {
                    findings.push(found(Grade::Good, Area::System, format!("System input volume: {pct:.0}%.")));
                }
            }
        }
        Err(_) => findings.push(found(
            Grade::Good,
            Area::System,
            "This mic has no system volume slider (fixed hardware gain); software leveling with `nivel run` still works.",
        )),
    }

    let silent = frames.iter().filter(|f| f.silent).count();
    let voiced: Vec<&FrameStats> = frames.iter().filter(|f| f.voiced).collect();
    let clipped = frames.iter().filter(|f| f.clipped).count();
    let speech = mean_db(voiced.iter().map(|f| f.in_db));
    let floor = percentile(
        frames
            .iter()
            .filter(|f| !f.voiced && !f.silent)
            .map(|f| f.in_db)
            .collect(),
        0.2,
    );

    if n > 0 && silent as f32 / n as f32 > 0.95 {
        findings.push(found(
            Grade::Bad,
            Area::Delivery,
            "The mic sends pure digital silence. Check the hardware mute switch, and that this terminal \
             may use the microphone (macOS: System Settings → Privacy & Security → Microphone; \
             Windows: Settings → Privacy → Microphone).",
        ));
    } else if n > 0 {
        findings.push(found(
            Grade::Good,
            Area::Delivery,
            "The mic delivers audio continuously.",
        ));

        if (voiced.len() as f32) < n as f32 * 0.05 {
            findings.push(found(
                Grade::Warn,
                Area::Delivery,
                "Hardly any speech was heard. Did you talk? If you did, this may be the wrong mic \
                 (see `nivel devices`) or it is very far from you.",
            ));
        } else if let Some(level) = speech {
            let (grade, text) = match level {
                l if l < -42.0 => (
                    Grade::Bad,
                    format!(
                        "Your voice is very quiet ({l:.0} dBFS). People will struggle to hear you."
                    ),
                ),
                l if l < -32.0 => (
                    Grade::Warn,
                    format!("Your voice is on the quiet side ({l:.0} dBFS)."),
                ),
                l if l > -10.0 => (
                    Grade::Warn,
                    format!("Your voice is very hot ({l:.0} dBFS); loud moments will distort."),
                ),
                l => (
                    Grade::Good,
                    format!("Voice level is healthy ({l:.0} dBFS)."),
                ),
            };
            findings.push(found(grade, Area::Level, text));
            if let Some(f) = floor {
                let snr = level - f;
                if snr < 15.0 {
                    findings.push(found(
                        Grade::Warn,
                        Area::Noise,
                        format!("Your voice is only {snr:.0} dB above the background; it will sound buried."),
                    ));
                }
            }
        }

        if let Some(f) = floor {
            findings.push(match f {
                f if f > -45.0 => found(
                    Grade::Bad,
                    Area::Noise,
                    format!("Loud background noise ({f:.0} dBFS): fans, traffic or hum."),
                ),
                f if f > -58.0 => found(
                    Grade::Warn,
                    Area::Noise,
                    format!("Noticeable background noise ({f:.0} dBFS)."),
                ),
                f => found(
                    Grade::Good,
                    Area::Noise,
                    format!("Quiet background ({f:.0} dBFS)."),
                ),
            });
        }

        let clip_ratio = clipped as f32 / n as f32;
        if clip_ratio > 0.005 {
            findings.push(found(
                Grade::Bad,
                Area::Level,
                format!(
                    "The mic is clipping ({:.1}% of the time): its gain is too high.",
                    clip_ratio * 100.0
                ),
            ));
        } else if clipped > 0 {
            findings.push(found(
                Grade::Warn,
                Area::Level,
                "The mic clipped a few times during loud moments.",
            ));
        } else {
            findings.push(found(Grade::Good, Area::Level, "No clipping."));
        }

        if restarts > 0 || glitches > 0 {
            findings.push(found(
                Grade::Warn,
                Area::Delivery,
                format!("Audio dropped out during the test ({restarts} reconnect(s), {glitches} glitch(es))."),
            ));
        } else {
            findings.push(found(Grade::Good, Area::Delivery, "No dropouts."));
        }
    }

    println!("Results");
    for f in &findings {
        let mark = match f.grade {
            Grade::Good => "✔",
            Grade::Warn => "!",
            Grade::Bad => "✘",
        };
        println!("  {mark} {}", f.text);
    }

    let worst = findings
        .iter()
        .map(|f| f.grade)
        .fold(Grade::Good, |a, b| if b > a { b } else { a });
    let issue = |area: Area| {
        findings
            .iter()
            .any(|f| f.grade != Grade::Good && f.area == area)
    };
    let level_issue = issue(Area::Level) || issue(Area::System);
    let noise_issue = issue(Area::Noise);

    println!();
    if worst == Grade::Good {
        println!("Your mic is in great shape. For extra polish in calls, try `nivel run`.");
        return Ok(());
    }
    println!("What to do");
    if level_issue && can_ride {
        println!(
            "  • `nivel ride`  keeps the system mic volume right for every app (no driver needed)"
        );
    }
    if noise_issue || level_issue {
        println!(
            "  • `nivel run`   removes noise, levels your voice and catches peaks, through a virtual mic"
        );
    }
    if worst == Grade::Bad {
        std::process::exit(2);
    }
    Ok(())
}
