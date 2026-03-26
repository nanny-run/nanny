// Integration tests for `nanny run` process lifecycle.
//
// These tests build and invoke the real `nanny` binary.
// They verify the two core guarantees of v0.1.0:
//   1. A process that exits cleanly produces exit code 0.
//   2. A process that exceeds timeout_ms is killed and exits non-zero.
//
// `CARGO_BIN_EXE_nanny` is injected by Cargo automatically for integration tests.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

// ── Helpers ───────────────────────────────────────────────────────────────────

fn nanny_bin() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_nanny"))
}

static COUNTER: AtomicU64 = AtomicU64::new(0);

/// Creates a unique temp dir for each test run.
///
/// Uses timestamp + monotonic counter to stay unique even when two tests
/// start within the same OS clock tick (common on macOS under parallelism).
fn temp_dir() -> PathBuf {
    let ts = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let seq = COUNTER.fetch_add(1, Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!("nanny_test_{ts}_{seq}"));
    fs::create_dir_all(&dir).unwrap();
    dir
}

fn write_config(dir: &Path, timeout_ms: u64, cmd: &str) {
    let toml = format!(
        r#"[runtime]
mode = "local"

[start]
cmd = "{cmd}"

[limits]
steps   = 100
cost    = 1000
timeout = {timeout_ms}

[tools]
allowed = ["http_get"]

[observability]
log = "stdout"
"#
    );
    fs::write(dir.join("nanny.toml"), toml).unwrap();
}

fn config_arg(dir: &Path) -> String {
    dir.join("nanny.toml").to_string_lossy().into_owned()
}

// ── Tests ─────────────────────────────────────────────────────────────────────

/// A process that exits on its own completes cleanly — exit code 0.
#[test]
fn fast_exit_completes_cleanly() {
    let dir = temp_dir();
    write_config(&dir, 30_000, "echo hello");

    let output = Command::new(nanny_bin())
        .args(["--config", &config_arg(&dir), "run"])
        .output()
        .expect("failed to run nanny");

    let _ = fs::remove_dir_all(&dir);

    assert!(
        output.status.success(),
        "nanny must exit 0 when the command exits cleanly\nstdout: {}\nstderr: {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );

    // stdout must contain ExecutionStarted and ExecutionStopped NDJSON lines.
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("ExecutionStarted"), "stdout must have ExecutionStarted event");
    assert!(stdout.contains("ExecutionStopped"), "stdout must have ExecutionStopped event");
    assert!(stdout.contains("AgentCompleted"), "stop reason must be AgentCompleted");
}

/// A process that runs past timeout_ms is killed — exit code non-zero,
/// stderr carries the stop reason.
#[test]
fn timeout_kills_process_and_exits_nonzero() {
    let dir = temp_dir();
    write_config(&dir, 300, "sleep 60"); // 300 ms timeout — well below `sleep 60`

    let output = Command::new(nanny_bin())
        .args(["--config", &config_arg(&dir), "run"])
        .output()
        .expect("failed to run nanny");

    let _ = fs::remove_dir_all(&dir);

    assert!(
        !output.status.success(),
        "nanny must exit non-zero when timeout fires"
    );

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("TimeoutExpired"),
        "stderr must contain 'TimeoutExpired'\ngot: {stderr}"
    );

    // stdout must still have both bookend events.
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("ExecutionStarted"), "stdout must have ExecutionStarted even on timeout");
    assert!(stdout.contains("TimeoutExpired"), "ExecutionStopped reason must be TimeoutExpired");
}

/// Named limits are resolved and enforced — timeout from [limits.fast].
#[test]
fn named_limits_timeout_is_enforced() {
    let dir = temp_dir();

    // Global limits have a generous timeout; the named set is tight.
    let toml = r#"
[runtime]
mode = "local"

[start]
cmd = "sleep 60"

[limits]
steps   = 100
cost    = 1000
timeout = 30000

[limits.fast]
timeout = 300

[tools]
allowed = ["http_get"]

[observability]
log = "stdout"
"#;
    fs::write(dir.join("nanny.toml"), toml).unwrap();

    let output = Command::new(nanny_bin())
        .args([
            "--config", &config_arg(&dir),
            "run", "--limits=fast",
        ])
        .output()
        .expect("failed to run nanny");

    let _ = fs::remove_dir_all(&dir);

    assert!(
        !output.status.success(),
        "nanny must exit non-zero when named limits timeout fires"
    );

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("TimeoutExpired"),
        "stderr must contain 'TimeoutExpired' for named limits timeout\ngot: {stderr}"
    );
}

/// Bridge events (ToolCalled, ToolAllowed, …) are flushed into the NDJSON
/// stream before ExecutionStopped, so ExecutionStopped is always the last line.
///
/// This test uses `echo` as the child command — it exits immediately without
/// making any bridge tool calls, so no per-tool events are produced.  The key
/// assertion is structural: every line is valid JSON and ExecutionStopped is last.
#[test]
fn execution_stopped_is_always_last_line() {
    let dir = temp_dir();
    write_config(&dir, 30_000, "echo nanny-test");

    let output = Command::new(nanny_bin())
        .args(["--config", &config_arg(&dir), "run"])
        .output()
        .expect("failed to run nanny");

    let _ = fs::remove_dir_all(&dir);

    let stdout = String::from_utf8_lossy(&output.stdout);
    let lines: Vec<&str> = stdout.lines()
        .filter(|l| l.trim_start().starts_with('{'))
        .collect();

    assert!(!lines.is_empty(), "stdout must contain at least one NDJSON line");

    // Every line must be valid JSON.
    for line in &lines {
        serde_json::from_str::<serde_json::Value>(line)
            .unwrap_or_else(|_| panic!("stdout line is not valid JSON: {line}"));
    }

    // ExecutionStopped must be the very last JSON line.
    let last: serde_json::Value = serde_json::from_str(lines.last().unwrap()).unwrap();
    assert_eq!(
        last["event"], "ExecutionStopped",
        "ExecutionStopped must be the last NDJSON line; got: {last}"
    );
}

/// Missing [start] section exits non-zero with a clear error message.
#[test]
fn missing_start_section_exits_nonzero_with_message() {
    let dir = temp_dir();
    fs::write(
        dir.join("nanny.toml"),
        r#"[runtime]
mode = "local"

[limits]
steps   = 10
cost    = 100
timeout = 5000

[observability]
log = "stdout"
"#,
    )
    .unwrap();

    let output = Command::new(nanny_bin())
        .args(["--config", &config_arg(&dir), "run"])
        .output()
        .expect("failed to run nanny");

    let _ = fs::remove_dir_all(&dir);

    assert!(
        !output.status.success(),
        "nanny must exit non-zero when [start] is missing"
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("no start config"),
        "stderr must mention 'no start config'; got: {stderr}"
    );
}
