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
///
/// Uses a platform-specific long-running command so the test exercises
/// the real kill path on every OS:
/// - Unix:    `sleep 60`  — standard POSIX utility
/// - Windows: `ping -n 65 127.0.0.1` — always available, native PE exe,
///   ~64 s runtime (1-second intervals × 65 probes); `TerminateProcess()`
///   kills it cleanly as a direct child.
///
/// On Windows this test requires T7 (server_start_loopback_does_not_require_cert_files)
/// to be skipped. T7 writes `~/.nanny/server.addr` to the real home dir (because
/// `dirs::home_dir()` ignores the HOME override), which would cause nanny to
/// route through `cmd_run_via_network_server` — a path with no timeout kill.
#[test]
fn timeout_kills_process_and_exits_nonzero() {
    let dir = temp_dir();
    // 300 ms timeout — well below either slow command.
    #[cfg(windows)]
    write_config(&dir, 300, "ping -n 65 127.0.0.1");
    #[cfg(not(windows))]
    write_config(&dir, 300, "sleep 60");

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

    // Use a platform-specific long-running command (same reasoning as
    // `timeout_kills_process_and_exits_nonzero`).
    #[cfg(windows)]
    let slow_cmd = "ping -n 65 127.0.0.1";
    #[cfg(not(windows))]
    let slow_cmd = "sleep 60";

    // Global limits have a generous timeout; the named set is tight.
    let toml = format!(
        "\
[runtime]
mode = \"local\"

[start]
cmd = \"{slow_cmd}\"

[limits]
steps   = 100
cost    = 1000
timeout = 30000

[limits.fast]
timeout = 300

[tools]
allowed = [\"http_get\"]

[observability]
log = \"stdout\"
"
    );
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

/// Bridge events (ToolAllowed, RuleDenied, ToolDenied, …) are flushed into the NDJSON
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

/// ExecutionStopped carries numeric `steps` and `cost_spent` fields.
///
/// This test uses `echo` — no bridge tool calls — so both values are
/// legitimately 0. The point is to assert the fields are present and
/// numeric, catching any regression where they are hardcoded to 0 even
/// when tools are called.  See the bridge-level
/// `tool_call_increments_step_and_charges_cost` test for the non-zero case.
#[test]
fn execution_stopped_has_accounting_fields() {
    let dir = temp_dir();
    write_config(&dir, 30_000, "echo nanny-accounting-test");

    let output = Command::new(nanny_bin())
        .args(["--config", &config_arg(&dir), "run"])
        .output()
        .expect("failed to run nanny");

    let _ = fs::remove_dir_all(&dir);

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stopped_line = stdout
        .lines()
        .filter(|l| l.trim_start().starts_with('{'))
        .find(|l| l.contains("ExecutionStopped"))
        .expect("ExecutionStopped line must be present in stdout");

    let v: serde_json::Value =
        serde_json::from_str(stopped_line).expect("ExecutionStopped must be valid JSON");

    assert!(
        v["steps"].is_number(),
        "ExecutionStopped must have a numeric `steps` field; got: {v}"
    );
    assert!(
        v["cost_spent"].is_number(),
        "ExecutionStopped must have a numeric `cost_spent` field; got: {v}"
    );
    assert!(
        v["elapsed_ms"].is_number(),
        "ExecutionStopped must have a numeric `elapsed_ms` field; got: {v}"
    );
}

/// A process that exits with a non-zero status code produces `ProcessCrashed`.
///
/// Regression guard: before the fix, the stop reason was always `AgentCompleted`
/// regardless of the child's exit code.
#[test]
fn process_crash_emits_process_crashed_stop_reason() {
    let dir = temp_dir();
    // `false` is the POSIX command that always exits with code 1.
    write_config(&dir, 30_000, "false");

    let output = Command::new(nanny_bin())
        .args(["--config", &config_arg(&dir), "run"])
        .output()
        .expect("failed to run nanny");

    let _ = fs::remove_dir_all(&dir);

    assert!(
        !output.status.success(),
        "nanny must exit non-zero when the child crashes"
    );

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("ProcessCrashed"),
        "ExecutionStopped must carry stop_reason=ProcessCrashed; stdout: {stdout}"
    );

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("ProcessCrashed"),
        "stderr must mention ProcessCrashed; got: {stderr}"
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

// ── T6: nanny server start — non-loopback without certs fails fast ────────────
//
// `server.rs` bails before starting the server when the bind address is
// non-loopback and cert files don't exist. Tests the error message content
// so developers know exactly what to do (nanny certs generate).

#[test]
fn server_start_nonloopback_without_certs_exits_with_message() {
    let dir   = temp_dir();
    let home  = temp_dir(); // override HOME so no ~/.nanny/certs/ exists

    // Write a minimal nanny.toml so the config load succeeds.
    fs::write(
        dir.join("nanny.toml"),
        r#"[runtime]
mode = "local"

[start]
cmd = "echo hello"

[limits]
steps   = 10
cost    = 100
timeout = 5000

[observability]
log = "stdout"
"#,
    )
    .unwrap();

    // Use a high port to avoid conflicts. Non-loopback → cert check fires.
    let output = Command::new(nanny_bin())
        .current_dir(&dir)
        .env("HOME", &home)
        .args(["server", "start", "--addr", "0.0.0.0:62998"])
        .output()
        .expect("nanny server start must run");

    let _ = fs::remove_dir_all(&dir);
    let _ = fs::remove_dir_all(&home);

    assert!(
        !output.status.success(),
        "nanny server start must exit non-zero when certs are missing for non-loopback"
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("mTLS") || stderr.contains("certs generate") || stderr.contains("not found"),
        "stderr must mention cert requirement; got: {stderr}"
    );
}

// ── T7: nanny server start — loopback does NOT require cert files ─────────────
//
// Default addr is 127.0.0.1 (loopback) → no cert check → server binds
// successfully even when ~/.nanny/certs/ doesn't exist.
// Regression guard: if the cert check accidentally runs for loopback, the
// server would fail to start and this test would catch it.
//
// Skipped on Windows: `dirs::home_dir()` ignores the `HOME` env override, so
// `nanny server start` writes `server.addr` and `server.token` to the REAL
// `~/.nanny/`. When the server is killed with TerminateProcess the cleanup
// hook does not run, leaving stale files. Those stale files cause concurrent
// tests (e.g. timeout_kills_process_and_exits_nonzero) to mistakenly route
// through `cmd_run_via_network_server`, which has no timeout kill — the child
// runs to completion and the timeout test fails. Skipping T7 on Windows
// eliminates the contamination; the loopback-vs-mTLS branch is
// platform-independent code covered by the Linux/macOS run.

#[cfg(not(windows))]
#[test]
fn server_start_loopback_does_not_require_cert_files() {
    let dir  = temp_dir();
    let home = temp_dir(); // fresh HOME — no certs directory

    fs::write(
        dir.join("nanny.toml"),
        r#"[runtime]
mode = "local"

[start]
cmd = "echo hello"

[limits]
steps   = 10
cost    = 100
timeout = 5000

[observability]
log = "stdout"
"#,
    )
    .unwrap();

    // Pick a port for the server. We'll probe it then kill the process.
    let port = 15900u16; // static, unlikely to be in use during tests

    let mut child = Command::new(nanny_bin())
        .current_dir(&dir)
        .env("HOME", &home)
        .args(["server", "start", "--addr", &format!("127.0.0.1:{port}")])
        .spawn()
        .expect("nanny server start must spawn");

    // Poll until the port accepts connections (up to 5s).
    let mut ready = false;
    for _ in 0..50 {
        if std::net::TcpStream::connect(format!("127.0.0.1:{port}")).is_ok() {
            ready = true;
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
    }

    // Kill the server process.
    let _ = child.kill();
    let _ = child.wait();
    let _ = fs::remove_dir_all(&dir);
    let _ = fs::remove_dir_all(&home);

    assert!(
        ready,
        "nanny server start on loopback must bind successfully without cert files"
    );
}

// ── T8: nanny run auto-detects a running governance server ────────────────────
//
// When ~/.nanny/server.addr and ~/.nanny/server.token exist and the server is
// reachable, `nanny run` should detect it, print the confirmation message, and
// route the child process through the network server instead of starting a
// local bridge.
//
// Skipped on Windows: `dirs::home_dir()` uses the Windows API
// (`SHGetKnownFolderPath` / `USERPROFILE`) and ignores the `HOME` environment
// variable entirely. Setting `env("HOME", &temp)` has no effect — nanny reads
// from the real user home, never finds the test state files, and falls back to
// a local bridge without printing the detection message.

#[cfg(not(windows))]
#[test]
fn nanny_run_detects_network_server_and_prints_message() {
    let dir  = temp_dir();
    let home = temp_dir();

    // Write nanny.toml.
    fs::write(
        dir.join("nanny.toml"),
        r#"[runtime]
mode = "local"

[start]
cmd = "echo nanny-detection-test"

[limits]
steps   = 10
cost    = 100
timeout = 10000

[observability]
log = "stdout"
"#,
    )
    .unwrap();

    // Start a plain-HTTP governance server on a loopback port.
    // We do this by running `nanny server start` in a background process
    // with HOME=home so it writes its state files there.
    let server_port = 15901u16;
    let server_toml_dir = temp_dir();
    fs::write(
        server_toml_dir.join("nanny.toml"),
        r#"[runtime]
mode = "local"

[start]
cmd = "echo unused"

[limits]
steps   = 100
cost    = 1000
timeout = 60000

[observability]
log = "stdout"
"#,
    )
    .unwrap();

    let mut server = Command::new(nanny_bin())
        .current_dir(&server_toml_dir)
        .env("HOME", &home)
        .args(["server", "start", "--addr", &format!("127.0.0.1:{server_port}")])
        .spawn()
        .expect("governance server must spawn");

    // Wait for the server to be ready.
    let mut ready = false;
    for _ in 0..50 {
        if std::net::TcpStream::connect(format!("127.0.0.1:{server_port}")).is_ok() {
            ready = true;
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
    assert!(ready, "governance server must become ready within 5 s");

    // Run `nanny run` with HOME pointing to the same home directory.
    // try_detect_network_server reads ~/.nanny/server.addr and ~/.nanny/server.token.
    let output = Command::new(nanny_bin())
        .current_dir(&dir)
        .env("HOME", &home)
        .args(["--config", &config_arg(&dir), "run"])
        .output()
        .expect("nanny run must complete");

    let _ = server.kill();
    let _ = server.wait();
    let _ = fs::remove_dir_all(&dir);
    let _ = fs::remove_dir_all(&home);
    let _ = fs::remove_dir_all(&server_toml_dir);

    assert!(
        output.status.success(),
        "nanny run must exit 0 when routing through network server\nstdout: {}\nstderr: {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("network server detected at"),
        "nanny run must print 'network server detected at' when server is reachable\ngot: {stdout}"
    );
}

// ── T9: Stale server.addr is cleaned up when TCP probe fails ─────────────────
//
// When ~/.nanny/server.addr points to a port with nothing listening,
// try_detect_network_server must delete the stale files and fall back to
// a local bridge. nanny run must exit 0 (not crash).
//
// Skipped on Windows: same `dirs::home_dir()` / `HOME` env var limitation as T8.
// Nanny reads from the real user home, never sees the stale test files, and
// exits 0 without performing any cleanup — making the assertion vacuously false.

#[cfg(not(windows))]
#[test]
fn stale_server_addr_cleaned_up_on_probe_failure() {
    let dir  = temp_dir();
    let home = temp_dir();

    // Write nanny.toml.
    fs::write(
        dir.join("nanny.toml"),
        r#"[runtime]
mode = "local"

[start]
cmd = "echo nanny-stale-test"

[limits]
steps   = 10
cost    = 100
timeout = 10000

[observability]
log = "stdout"
"#,
    )
    .unwrap();

    // Write stale state files pointing to a port with nothing listening.
    let nanny_state = home.join(".nanny");
    fs::create_dir_all(&nanny_state).unwrap();
    // Port 1 is typically reserved / always unreachable on localhost.
    fs::write(nanny_state.join("server.addr"), "127.0.0.1:1").unwrap();
    fs::write(nanny_state.join("server.token"), "stale-token").unwrap();

    let output = Command::new(nanny_bin())
        .current_dir(&dir)
        .env("HOME", &home)
        .args(["--config", &config_arg(&dir), "run"])
        .output()
        .expect("nanny run must complete");

    // Check whether stale files were removed.
    let addr_exists  = nanny_state.join("server.addr").exists();
    let token_exists = nanny_state.join("server.token").exists();

    let _ = fs::remove_dir_all(&dir);
    let _ = fs::remove_dir_all(&home);

    assert!(
        output.status.success(),
        "nanny run must exit 0 when stale server.addr is cleaned up\nstdout: {}\nstderr: {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );
    assert!(
        !addr_exists,
        "stale server.addr must be deleted when TCP probe fails"
    );
    assert!(
        !token_exists,
        "stale server.token must be deleted when TCP probe fails"
    );
}
