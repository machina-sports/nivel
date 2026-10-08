//! Terminal output helpers. Plain text and `\r` only, so the live line works
//! in every terminal, including the classic Windows console.

use std::collections::HashMap;
use std::io::Write;
use std::time::{Duration, Instant};

use nivel_core::Alert;

/// Width of the live status line.
const LINE: usize = 78;

pub fn clear_line() {
    print!("\r{:LINE$}\r", "");
    let _ = std::io::stdout().flush();
}

/// Overwrite the live status line.
pub fn status(text: &str) {
    let text: String = text.chars().take(LINE).collect();
    print!("\r{text:LINE$}");
    let _ = std::io::stdout().flush();
}

/// Print a permanent line above the live status line.
pub fn say(text: impl std::fmt::Display) {
    clear_line();
    println!("{text}");
}

/// Level meter for -60..0 dBFS.
pub fn meter(db: f32, width: usize) -> String {
    let frac = ((db + 60.0) / 60.0).clamp(0.0, 1.0);
    let filled = (frac * width as f32).round() as usize;
    format!("{}{}", "█".repeat(filled), "░".repeat(width - filled))
}

pub fn db(v: f32) -> String {
    if v <= -119.0 {
        "  -∞ dB".to_string()
    } else {
        format!("{v:>4.0} dB")
    }
}

pub fn duration(d: Duration) -> String {
    let s = d.as_secs();
    if s >= 3600 {
        format!("{}h{:02}m", s / 3600, (s % 3600) / 60)
    } else if s >= 60 {
        format!("{}m{:02}s", s / 60, s % 60)
    } else {
        format!("{s}s")
    }
}

/// Prints each kind of alert at most once every `every`.
pub struct AlertThrottle {
    every: Duration,
    last: HashMap<std::mem::Discriminant<Alert>, Instant>,
}

impl AlertThrottle {
    pub fn new(every: Duration) -> Self {
        Self {
            every,
            last: HashMap::new(),
        }
    }

    pub fn show(&mut self, alerts: &[Alert]) {
        let now = Instant::now();
        for a in alerts {
            let key = std::mem::discriminant(a);
            if self
                .last
                .get(&key)
                .is_none_or(|t| now.duration_since(*t) >= self.every)
            {
                self.last.insert(key, now);
                say(format!("  ! {a}"));
            }
        }
    }
}
