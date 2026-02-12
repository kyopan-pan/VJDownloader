use std::collections::VecDeque;
use std::time::{Duration, Instant};

use time::OffsetDateTime;
use time::macros::format_description;

const MAX_ENTRIES: usize = 1000;

pub struct AppLogger {
    entries: VecDeque<LogEntry>,
}

impl AppLogger {
    pub fn new() -> Self {
        Self {
            entries: VecDeque::with_capacity(MAX_ENTRIES),
        }
    }

    pub fn push(&mut self, message: impl Into<String>) {
        let message = message.into();
        if message.is_empty() {
            return;
        }

        let timestamp = current_time_text();
        let line = format!("[{timestamp}] {message}");
        println!("{line}");

        self.entries.push_back(LogEntry {
            at: Instant::now(),
            line,
        });

        while self.entries.len() > MAX_ENTRIES {
            self.entries.pop_front();
        }
    }

    pub fn clear(&mut self) {
        self.entries.clear();
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    pub fn lines(&self) -> impl Iterator<Item = &str> {
        self.entries.iter().map(|entry| entry.line.as_str())
    }

    pub fn build_recent_snapshot(&self, duration: Duration) -> String {
        if duration.is_zero() {
            return String::new();
        }

        let cutoff = Instant::now().checked_sub(duration);
        let mut out = String::new();
        for entry in &self.entries {
            if let Some(cutoff) = cutoff {
                if entry.at < cutoff {
                    continue;
                }
            }
            if !out.is_empty() {
                out.push('\n');
            }
            out.push_str(&entry.line);
        }
        out
    }
}

impl Default for AppLogger {
    fn default() -> Self {
        Self::new()
    }
}

struct LogEntry {
    at: Instant,
    line: String,
}

fn current_time_text() -> String {
    let now = OffsetDateTime::now_local().unwrap_or_else(|_| OffsetDateTime::now_utc());
    now.format(&format_description!("[hour]:[minute]:[second]"))
        .unwrap_or_else(|_| "00:00:00".to_string())
}
