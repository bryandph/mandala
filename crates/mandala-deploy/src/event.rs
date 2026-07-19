use std::fs::{File, OpenOptions};
use std::io::{self, Write};
use std::path::Path;
use std::sync::Mutex;

use serde::Serialize;

#[derive(Clone, Copy, Debug, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Level {
    Trace,
    Debug,
    Info,
    Warn,
    Error,
}

pub trait EventSink: Send + Sync {
    fn emit(&self, level: Level, message: &str);
}

#[derive(Serialize)]
struct Record<'a> {
    host: &'a str,
    level: Level,
    message: &'a str,
}

/// A deliberately simple spike sink: one append-only stream per host. The
/// Stage-B engine will adapt this trait onto Mandala's versioned registry
/// writer; the vendored controller never owns global logging.
pub struct JsonlSink {
    host: String,
    file: Mutex<File>,
}

impl JsonlSink {
    pub fn new(host: impl Into<String>, path: impl AsRef<Path>) -> io::Result<Self> {
        let file = OpenOptions::new()
            .create(true)
            .truncate(true)
            .write(true)
            .open(path)?;
        Ok(Self {
            host: host.into(),
            file: Mutex::new(file),
        })
    }
}

impl EventSink for JsonlSink {
    fn emit(&self, level: Level, message: &str) {
        let Ok(line) = serde_json::to_string(&Record {
            host: &self.host,
            level,
            message,
        }) else {
            return;
        };
        if let Ok(mut file) = self.file.lock() {
            let _ = writeln!(file, "{line}");
        }
    }
}

pub(crate) fn emit_output(sink: &dyn EventSink, output: &std::process::Output) {
    for line in String::from_utf8_lossy(&output.stdout).lines() {
        sink.emit(Level::Info, line);
    }
    for line in String::from_utf8_lossy(&output.stderr).lines() {
        sink.emit(Level::Warn, line);
    }
}
