//! Keeps an [`Engine`] running no matter what the hardware does.
//!
//! - Rebuilds the engine when a device errors, stalls or goes silent.
//! - Falls back to the default mic when the requested one is missing, and
//!   switches back as soon as it returns.
//! - Follows the system default mic when no mic was requested.
//! - Retries with backoff while nothing works.

use std::fmt;
use std::sync::Arc;
use std::time::{Duration, Instant};

use crate::devices::{self, DeviceNotFound};
use crate::engine::{Engine, EngineConfig, EngineInfo};
use crate::telemetry::{Snapshot, Telemetry};
use crate::watchdog::{Alert, Watchdog, WatchdogConfig};

#[derive(Debug, Clone)]
pub enum Phase {
    Running,
    /// No engine right now; retrying.
    Recovering {
        reason: String,
    },
}

/// Things that happened since the previous [`Supervisor::tick`].
#[derive(Debug, Clone)]
pub enum Event {
    Started {
        input: String,
        output: Option<String>,
    },
    Note(String),
    Lost(String),
    FellBack {
        wanted: String,
        using: String,
    },
    Failed(String),
}

impl fmt::Display for Event {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Event::Started {
                input,
                output: Some(o),
            } => write!(f, "listening to \"{input}\" → \"{o}\""),
            Event::Started {
                input,
                output: None,
            } => write!(f, "listening to \"{input}\""),
            Event::Note(n) => f.write_str(n),
            Event::Lost(r) => write!(f, "reconnecting: {r}"),
            Event::FellBack { wanted, using } => {
                write!(
                    f,
                    "\"{wanted}\" is not connected; using \"{using}\" until it returns"
                )
            }
            Event::Failed(e) => write!(f, "cannot start audio: {e}"),
        }
    }
}

pub struct Status {
    pub phase: Phase,
    pub info: Option<EngineInfo>,
    pub snapshot: Snapshot,
    pub alerts: Vec<Alert>,
    pub events: Vec<Event>,
    pub restarts: u32,
}

const RECHECK_EVERY: Duration = Duration::from_secs(3);
const MIN_BACKOFF: Duration = Duration::from_millis(500);
const MAX_BACKOFF: Duration = Duration::from_secs(5);

pub struct Supervisor {
    host: cpal::Host,
    cfg: EngineConfig,
    fallback: bool,
    telemetry: Arc<Telemetry>,
    engine: Option<Engine>,
    watchdog: Watchdog,
    on_fallback: bool,
    next_try: Instant,
    backoff: Duration,
    failures: u32,
    restarts: u32,
    last_recheck: Instant,
    last_tick: Instant,
    reason: String,
    last_failure: Option<String>,
    events: Vec<Event>,
}

impl Supervisor {
    /// `fallback`: when the requested mic is missing, use the default one
    /// meanwhile instead of waiting.
    pub fn new(cfg: EngineConfig, fallback: bool) -> Self {
        let now = Instant::now();
        Self {
            host: cpal::default_host(),
            cfg,
            fallback,
            telemetry: Arc::new(Telemetry::default()),
            engine: None,
            watchdog: Watchdog::new(WatchdogConfig::default()),
            on_fallback: false,
            next_try: now,
            backoff: MIN_BACKOFF,
            failures: 0,
            restarts: 0,
            last_recheck: now,
            last_tick: now,
            reason: "starting".into(),
            last_failure: None,
            events: Vec::new(),
        }
    }

    pub fn telemetry(&self) -> &Arc<Telemetry> {
        &self.telemetry
    }

    /// Drive the supervisor. Call it every 50–250 ms.
    pub fn tick(&mut self) -> Status {
        let now = Instant::now();
        let dt = now.duration_since(self.last_tick).as_secs_f32();
        self.last_tick = now;

        if self.engine.is_none() && now >= self.next_try {
            self.try_start(now);
        }

        let snapshot = self.telemetry.snapshot();
        let mut alerts = Vec::new();
        if let Some(engine) = &self.engine {
            let has_output = engine.info().output.is_some();
            let current_input = engine.info().input.clone();
            if let Some(err) = self.telemetry.take_error() {
                self.lose(format!("device error: {err}"), now);
            } else {
                let verdict = self.watchdog.check(&snapshot, has_output, dt);
                alerts = verdict.alerts;
                if let Some(reason) = verdict.restart {
                    self.lose(reason, now);
                } else if now.duration_since(self.last_recheck) >= RECHECK_EVERY {
                    self.last_recheck = now;
                    self.recheck_input(&current_input, now);
                }
            }
        }

        Status {
            phase: if self.engine.is_some() {
                Phase::Running
            } else {
                Phase::Recovering {
                    reason: self.reason.clone(),
                }
            },
            info: self.engine.as_ref().map(|e| e.info().clone()),
            snapshot,
            alerts,
            events: std::mem::take(&mut self.events),
            restarts: self.restarts,
        }
    }

    /// Stop audio (e.g. before exiting).
    pub fn shutdown(&mut self) {
        self.engine = None;
    }

    /// Switch back to the preferred mic, or follow the system default.
    fn recheck_input(&mut self, current: &str, now: Instant) {
        match &self.cfg.input {
            Some(wanted)
                if self.on_fallback && devices::select_input(&self.host, Some(wanted)).is_ok() =>
            {
                self.lose(format!("\"{wanted}\" is back"), now);
                self.next_try = now;
            }
            None => {
                if let Ok(default) = devices::select_input(&self.host, None)
                    && default.name != current
                {
                    self.lose(
                        format!("the default microphone is now \"{}\"", default.name),
                        now,
                    );
                    self.next_try = now;
                }
            }
            _ => {}
        }
    }

    fn lose(&mut self, reason: String, now: Instant) {
        self.engine = None;
        self.watchdog.reset();
        self.restarts += 1;
        self.events.push(Event::Lost(reason.clone()));
        self.reason = reason;
        self.next_try = now + Duration::from_millis(300);
    }

    fn try_start(&mut self, now: Instant) {
        let err = match Engine::start(&self.host, &self.cfg, self.telemetry.clone()) {
            Ok(engine) => return self.started(engine, false),
            Err(err) => err,
        };

        let mic_missing = err
            .downcast_ref::<DeviceNotFound>()
            .is_some_and(|e| e.kind == "input");
        if mic_missing && self.fallback && self.cfg.input.is_some() {
            let cfg = EngineConfig {
                input: None,
                ..self.cfg.clone()
            };
            if let Ok(engine) = Engine::start(&self.host, &cfg, self.telemetry.clone()) {
                if !self.on_fallback {
                    self.events.push(Event::FellBack {
                        wanted: self.cfg.input.clone().unwrap_or_default(),
                        using: engine.info().input.clone(),
                    });
                }
                return self.started(engine, true);
            }
        }

        self.failures += 1;
        let msg = format!("{err:#}");
        if self.last_failure.as_deref() != Some(msg.as_str()) {
            self.events.push(Event::Failed(msg.clone()));
            self.last_failure = Some(msg.clone());
        }
        self.reason = msg;
        // A restarted sound server can leave the host handle stale.
        if self.failures.is_multiple_of(3) {
            self.host = cpal::default_host();
        }
        self.next_try = now + self.backoff;
        self.backoff = (self.backoff * 2).min(MAX_BACKOFF);
    }

    fn started(&mut self, engine: Engine, on_fallback: bool) {
        let info = engine.info();
        self.events.push(Event::Started {
            input: info.input.clone(),
            output: info.output.clone(),
        });
        self.events
            .extend(info.notes.iter().cloned().map(Event::Note));
        self.engine = Some(engine);
        self.on_fallback = on_fallback;
        self.failures = 0;
        self.backoff = MIN_BACKOFF;
        self.last_failure = None;
        self.watchdog.reset();
        self.last_recheck = Instant::now();
    }
}
