// events.rs — Structured NDJSON event log for nanny executions.
//
// One JSON object per line. Written append-only to stdout or a file.
// ExecutionStarted is always the first event.
// ExecutionStopped is always the last event — every exit path must emit it.

use anyhow::{Context, Result};
use nanny_config::ObservabilityConfig;
use nanny_core::events::event::{ExecutionEvent, LoggedEvent};
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
    /// file   → appends to `base_dir/.nanny/logs/<name>` (default filename
    ///          "log.ndjson", auto-created directory), creating the file if
    ///          it doesn't exist. See `ObservabilityConfig::resolve_log_path`.
    pub fn from_config(config: &ObservabilityConfig, base_dir: &Path) -> Result<Self> {
        match config.resolve_log_path(base_dir)? {
            None => Ok(Self { out: Box::new(io::stdout()) }),
            Some(path) => Self::file(&path),
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
    ///
    /// Takes the run id and sequence explicitly rather than holding a counter:
    /// the bridge numbers every verdict, so a second counter here would produce
    /// two overlapping sequences for one run. The CLI only ever writes the two
    /// bookends, and it takes their numbers from the bridge.
    pub fn write(&mut self, run_id: &str, seq: u64, event: &ExecutionEvent) -> Result<()> {
        let stamped = LoggedEvent::new(run_id, seq, event.clone());
        let line = serde_json::to_string(&stamped).context("failed to serialize event")?;
        self.write_raw(&line)
    }

    /// Write a pre-serialised JSON line, flushed immediately.
    ///
    /// Used to forward raw event lines from the bridge (e.g. `ToolAllowed`,
    /// `ToolDenied`, `RuleDenied`) without re-parsing or re-serialising them.
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

    fn started_event() -> ExecutionEvent {
        ExecutionEvent::ExecutionStarted {
            ts: 0,
            command: "python agent.py".to_string(),
            allowed_tools: vec!["http_get".to_string()],
            tool_labels: [("http_get".to_string(), vec!["reads_untrusted".to_string()])]
                .into_iter()
                .collect(),
            config_hash: "deadbeef".to_string(),
        }
    }

    fn stopped_event(reason: &str, tokens_spent: u64, elapsed_ms: u64) -> ExecutionEvent {
        ExecutionEvent::ExecutionStopped {
            ts: 0,
            reason: reason.to_string(),
            tokens_spent,
            elapsed_ms,
        }
    }

    #[test]
    fn execution_started_is_valid_json() {
        let event = started_event();
        let json = serde_json::to_string(&event).unwrap();
        let v: serde_json::Value = serde_json::from_str(&json).unwrap();

        assert_eq!(v["event"], "ExecutionStarted");
        assert_eq!(v["command"], "python agent.py");
        assert!(v["ts"].is_number());
        assert_eq!(v["allowed_tools"][0], "http_get");
        assert_eq!(v["tool_labels"]["http_get"][0], "reads_untrusted");
    }

    #[test]
    fn execution_stopped_is_valid_json() {
        let event = stopped_event("ToolDenied", 0, 5_432);
        let json = serde_json::to_string(&event).unwrap();
        let v: serde_json::Value = serde_json::from_str(&json).unwrap();

        assert_eq!(v["event"], "ExecutionStopped");
        assert_eq!(v["reason"], "ToolDenied");
        assert_eq!(v["tokens_spent"], 0u64);
        assert_eq!(v["elapsed_ms"], 5_432u64);
    }

    #[test]
    fn both_event_types_serialize_with_event_field() {
        let events = [
            started_event(),
            stopped_event("AgentCompleted", 0, 0),
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
            for (seq, event) in events.into_iter().enumerate() {
                writer.write("test-run", seq as u64, &event).unwrap();
            }
        }
        let bytes = buf.lock().unwrap().clone();
        String::from_utf8(bytes).unwrap()
    }

    #[test]
    fn event_writer_produces_ndjson_lines() {
        let output = write_to_buf([
            started_event(),
            stopped_event("AgentCompleted", 0, 100),
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
            stopped_event("AgentCompleted", 0, 0),
        ]);
        let first: serde_json::Value =
            serde_json::from_str(output.lines().next().unwrap()).unwrap();
        assert_eq!(first["event"], "ExecutionStarted");
    }

    #[test]
    fn execution_stopped_is_last_line() {
        let output = write_to_buf([
            started_event(),
            stopped_event("RuleDenied", 0, 200),
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
            writer.write("test-run", 0, &started_event()).unwrap();
            writer.write("test-run", 1, &stopped_event("AgentCompleted", 0, 50)).unwrap();
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
