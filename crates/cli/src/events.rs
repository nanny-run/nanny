// events.rs — Structured NDJSON event log for nanny executions.
//
// One JSON object per line. Written append-only to stdout or a file.
// ExecutionStarted is always the first event.
// ExecutionStopped is always the last event — every exit path must emit it.

use anyhow::{Context, Result};
use nanny_config::{LogTarget, ObservabilityConfig};
use nanny_core::events::event::ExecutionEvent;
use std::fs::OpenOptions;
use std::io::{self, BufWriter, Write};
use std::path::Path;

// ── EventWriter ───────────────────────────────────────────────────────────────

/// Writes ExecutionEvents as NDJSON — one line per event.
///
/// Open with `EventWriter::from_config`. Write events with `write`.
/// The writer flushes on every call — no buffered surprises on kill.
pub struct EventWriter {
    out: Box<dyn Write>,
}

impl EventWriter {
    /// Open a writer from observability config.
    ///
    /// stdout → writes to stdout.
    /// file   → appends to `log_file`, creating it if it doesn't exist.
    pub fn from_config(config: &ObservabilityConfig) -> Result<Self> {
        match config.log {
            LogTarget::Stdout => Ok(Self { out: Box::new(io::stdout()) }),
            LogTarget::File => {
                let path = config
                    .log_file
                    .as_deref()
                    .context("observability.log = \"file\" requires log_file to be set")?;
                Self::file(path)
            }
        }
    }

    fn file(path: &Path) -> Result<Self> {
        let file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
            .with_context(|| format!("failed to open log file '{}'", path.display()))?;
        Ok(Self { out: Box::new(BufWriter::new(file)) })
    }

    /// Write one event as a single line of JSON, flushed immediately.
    pub fn write(&mut self, event: &ExecutionEvent) -> Result<()> {
        let line = serde_json::to_string(event).context("failed to serialize event")?;
        self.write_raw(&line)
    }

    /// Write a pre-serialised JSON line, flushed immediately.
    ///
    /// Used to forward raw event lines from the bridge (e.g. `StepCompleted`,
    /// `ToolAllowed`, `ToolDenied`) without re-parsing or re-serialising them.
    pub fn write_raw(&mut self, line: &str) -> Result<()> {
        writeln!(self.out, "{line}").context("failed to write event")?;
        self.out.flush().context("failed to flush event log")?;
        Ok(())
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use nanny_core::events::event::LimitsSnapshot;

    fn started_event() -> ExecutionEvent {
        ExecutionEvent::ExecutionStarted {
            ts: 0,
            limits: LimitsSnapshot { steps: 100, cost: 1000, timeout: 30_000 },
            limits_set: "[limits]".to_string(),
            command: "python agent.py".to_string(),
        }
    }

    fn stopped_event(reason: &str, steps: u32, cost_spent: u64, elapsed_ms: u64) -> ExecutionEvent {
        ExecutionEvent::ExecutionStopped {
            ts: 0,
            reason: reason.to_string(),
            steps,
            cost_spent,
            elapsed_ms,
        }
    }

    #[test]
    fn execution_started_is_valid_json() {
        let event = started_event();
        let json = serde_json::to_string(&event).unwrap();
        let v: serde_json::Value = serde_json::from_str(&json).unwrap();

        assert_eq!(v["event"], "ExecutionStarted");
        assert_eq!(v["limits_set"], "[limits]");
        assert_eq!(v["command"], "python agent.py");
        assert_eq!(v["limits"]["steps"], 100);
        assert_eq!(v["limits"]["cost"], 1000);
        assert_eq!(v["limits"]["timeout"], 30_000u64);
        assert!(v["ts"].is_number());
    }

    #[test]
    fn execution_stopped_is_valid_json() {
        let event = stopped_event("TimeoutExpired", 7, 0, 5_432);
        let json = serde_json::to_string(&event).unwrap();
        let v: serde_json::Value = serde_json::from_str(&json).unwrap();

        assert_eq!(v["event"], "ExecutionStopped");
        assert_eq!(v["reason"], "TimeoutExpired");
        assert_eq!(v["steps"], 7);
        assert_eq!(v["cost_spent"], 0u64);
        assert_eq!(v["elapsed_ms"], 5_432u64);
    }

    #[test]
    fn both_event_types_serialize_with_event_field() {
        let events = [
            started_event(),
            stopped_event("AgentCompleted", 0, 0, 0),
        ];
        let names = ["ExecutionStarted", "ExecutionStopped"];

        for (event, expected_name) in events.iter().zip(names.iter()) {
            let json = serde_json::to_string(event).unwrap();
            let v: serde_json::Value = serde_json::from_str(&json).unwrap();
            assert_eq!(v["event"], *expected_name, "wrong event name for {expected_name}");
        }
    }

    fn write_to_buf(events: impl IntoIterator<Item = ExecutionEvent>) -> String {
        let buf = std::sync::Arc::new(std::sync::Mutex::new(Vec::<u8>::new()));
        let buf_clone = buf.clone();
        {
            struct ArcWriter(std::sync::Arc<std::sync::Mutex<Vec<u8>>>);
            impl Write for ArcWriter {
                fn write(&mut self, data: &[u8]) -> io::Result<usize> {
                    self.0.lock().unwrap().write(data)
                }
                fn flush(&mut self) -> io::Result<()> { Ok(()) }
            }
            let mut writer = EventWriter { out: Box::new(ArcWriter(buf_clone)) };
            for event in events {
                writer.write(&event).unwrap();
            }
        }
        let bytes = buf.lock().unwrap().clone();
        String::from_utf8(bytes).unwrap()
    }

    #[test]
    fn event_writer_produces_ndjson_lines() {
        let output = write_to_buf([
            started_event(),
            stopped_event("AgentCompleted", 1, 0, 100),
        ]);

        let lines: Vec<&str> = output.lines().collect();
        for line in &lines {
            serde_json::from_str::<serde_json::Value>(line)
                .unwrap_or_else(|_| panic!("line is not valid JSON: {line}"));
        }
        assert_eq!(lines.len(), 2);
    }

    #[test]
    fn execution_started_is_first_line() {
        let output = write_to_buf([
            started_event(),
            stopped_event("AgentCompleted", 0, 0, 0),
        ]);
        let first: serde_json::Value =
            serde_json::from_str(output.lines().next().unwrap()).unwrap();
        assert_eq!(first["event"], "ExecutionStarted");
    }

    #[test]
    fn execution_stopped_is_last_line() {
        let output = write_to_buf([
            started_event(),
            stopped_event("MaxStepsReached", 100, 0, 200),
        ]);
        let last: serde_json::Value =
            serde_json::from_str(output.lines().last().unwrap()).unwrap();
        assert_eq!(last["event"], "ExecutionStopped");
    }

    #[test]
    fn file_writer_appends_to_file() {
        let dir = std::env::temp_dir();
        let path = dir.join("nanny_test_events.ndjson");
        let _ = std::fs::remove_file(&path); // clean slate

        {
            let mut writer = EventWriter::file(&path).unwrap();
            writer.write(&started_event()).unwrap();
            writer.write(&stopped_event("AgentCompleted", 0, 0, 50)).unwrap();
        }

        let content = std::fs::read_to_string(&path).unwrap();
        let lines: Vec<&str> = content.lines().collect();
        assert_eq!(lines.len(), 2);

        let first: serde_json::Value = serde_json::from_str(lines[0]).unwrap();
        let last: serde_json::Value = serde_json::from_str(lines[1]).unwrap();
        assert_eq!(first["event"], "ExecutionStarted");
        assert_eq!(last["event"], "ExecutionStopped");

        let _ = std::fs::remove_file(&path);
    }
}
