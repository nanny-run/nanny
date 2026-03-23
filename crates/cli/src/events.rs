// events.rs — Structured NDJSON event log for nanny executions.
//
// One JSON object per line. Written append-only to stdout or a file.
// ExecutionStarted is always the first event.
// ExecutionStopped is always the last event — every exit path must emit it.

use anyhow::{Context, Result};
use nanny_config::{LogTarget, ObservabilityConfig};
use nanny_core::agent::limits::Limits;
use serde::Serialize;
use std::fs::OpenOptions;
use std::io::{self, BufWriter, Write};
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

// ── Timestamp ─────────────────────────────────────────────────────────────────

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

// ── Event schema ──────────────────────────────────────────────────────────────

#[derive(Serialize)]
pub struct LimitsSnapshot {
    pub steps: u32,
    pub cost: u64,
    pub timeout: u64,
}

/// All structured events emitted by the nanny runtime.
///
/// Serialized as NDJSON — one JSON object per line.
/// The `"event"` field identifies the event type (the enum variant name).
///
/// `ExecutionStarted` is always the first event.
/// `ExecutionStopped` is always the last event.
///
/// `StepCompleted`, `ToolCalled`, `ToolAllowed`, `ToolDenied` are emitted
/// by the bridge in v0.2.0 when macros instrument the child process.
#[allow(dead_code)] // bridge variants wired in v0.2.0
#[derive(Serialize)]
#[serde(tag = "event")]
pub enum Event {
    ExecutionStarted {
        ts: u64,
        limits: LimitsSnapshot,
        limits_set: String,
        command: String,
    },
    StepCompleted {
        ts: u64,
        step: u32,
    },
    ToolCalled {
        ts: u64,
        tool: String,
    },
    ToolAllowed {
        ts: u64,
        tool: String,
    },
    ToolDenied {
        ts: u64,
        tool: String,
        reason: String,
    },
    ExecutionStopped {
        ts: u64,
        reason: String,
        steps: u32,
        cost_spent: u64,
        elapsed_ms: u64,
    },
}

impl Event {
    pub fn execution_started(limits: &Limits, limits_set: &str, command: &str) -> Self {
        Event::ExecutionStarted {
            ts: now_ms(),
            limits: LimitsSnapshot {
                steps: limits.max_steps,
                cost: limits.max_cost_units,
                timeout: limits.timeout_ms,
            },
            limits_set: limits_set.to_string(),
            command: command.to_string(),
        }
    }

    pub fn execution_stopped(reason: &str, steps: u32, cost_spent: u64, elapsed_ms: u64) -> Self {
        Event::ExecutionStopped {
            ts: now_ms(),
            reason: reason.to_string(),
            steps,
            cost_spent,
            elapsed_ms,
        }
    }
}

// ── EventWriter ───────────────────────────────────────────────────────────────

/// Writes Events as NDJSON — one line per event.
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
    pub fn write(&mut self, event: &Event) -> Result<()> {
        let line = serde_json::to_string(event).context("failed to serialize event")?;
        writeln!(self.out, "{line}").context("failed to write event")?;
        self.out.flush().context("failed to flush event log")?;
        Ok(())
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn test_limits() -> Limits {
        Limits { max_steps: 100, max_cost_units: 1000, timeout_ms: 30_000 }
    }

    #[test]
    fn execution_started_is_valid_json() {
        let event = Event::execution_started(&test_limits(), "[limits]", "python agent.py");
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
        let event = Event::execution_stopped("TimeoutExpired", 7, 0, 5_432);
        let json = serde_json::to_string(&event).unwrap();
        let v: serde_json::Value = serde_json::from_str(&json).unwrap();

        assert_eq!(v["event"], "ExecutionStopped");
        assert_eq!(v["reason"], "TimeoutExpired");
        assert_eq!(v["steps"], 7);
        assert_eq!(v["cost_spent"], 0u64);
        assert_eq!(v["elapsed_ms"], 5_432u64);
    }

    #[test]
    fn all_event_types_serialize_with_event_field() {
        let events: Vec<Event> = vec![
            Event::execution_started(&test_limits(), "[limits]", "cmd"),
            Event::StepCompleted { ts: 0, step: 1 },
            Event::ToolCalled { ts: 0, tool: "http_get".into() },
            Event::ToolAllowed { ts: 0, tool: "http_get".into() },
            Event::ToolDenied { ts: 0, tool: "write_file".into(), reason: "ToolDenied".into() },
            Event::execution_stopped("AgentCompleted", 0, 0, 0),
        ];
        let names = ["ExecutionStarted", "StepCompleted", "ToolCalled", "ToolAllowed", "ToolDenied", "ExecutionStopped"];

        for (event, expected_name) in events.iter().zip(names.iter()) {
            let json = serde_json::to_string(event).unwrap();
            let v: serde_json::Value = serde_json::from_str(&json).unwrap();
            assert_eq!(v["event"], *expected_name, "wrong event name for {expected_name}");
        }
    }

    fn write_to_buf(events: impl IntoIterator<Item = Event>) -> String {
        // EventWriter owns a Box<dyn Write>, so it can't hold a bare &mut Vec<u8>
        // (the borrow would outlive the writer). Use Arc<Mutex<Vec>> so the writer
        // and the caller can both reach the buffer — writer via ArcWriter, caller
        // after the writer drops.
        let buf = std::sync::Arc::new(std::sync::Mutex::new(Vec::<u8>::new()));
        let buf_clone = buf.clone();
        {
            // We can't Box a reference to the Mutex guard, so write to a Vec directly
            // by using a helper struct that proxies writes into the Arc<Mutex<Vec>>.
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
            Event::execution_started(&test_limits(), "[limits]", "echo hi"),
            Event::StepCompleted { ts: 0, step: 1 },
            Event::execution_stopped("AgentCompleted", 1, 0, 100),
        ]);

        let lines: Vec<&str> = output.lines().collect();
        for line in &lines {
            serde_json::from_str::<serde_json::Value>(line)
                .unwrap_or_else(|_| panic!("line is not valid JSON: {line}"));
        }
        assert_eq!(lines.len(), 3);
    }

    #[test]
    fn execution_started_is_first_line() {
        let output = write_to_buf([
            Event::execution_started(&test_limits(), "[limits]", "cmd"),
            Event::execution_stopped("AgentCompleted", 0, 0, 0),
        ]);
        let first: serde_json::Value =
            serde_json::from_str(output.lines().next().unwrap()).unwrap();
        assert_eq!(first["event"], "ExecutionStarted");
    }

    #[test]
    fn execution_stopped_is_last_line() {
        let output = write_to_buf([
            Event::execution_started(&test_limits(), "[limits]", "cmd"),
            Event::StepCompleted { ts: 0, step: 1 },
            Event::execution_stopped("MaxStepsReached", 100, 0, 200),
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
            writer.write(&Event::execution_started(&test_limits(), "[limits]", "cmd")).unwrap();
            writer.write(&Event::execution_stopped("AgentCompleted", 0, 0, 50)).unwrap();
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
