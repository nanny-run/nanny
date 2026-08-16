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
        r#"[start]
cmd = "{cmd}"

[limits]
steps   = 100
tokens  = 1000
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

/// Write `.nanny/app.json` directly (bypassing `nanny init`, which also wants
/// to write nanny.toml, tests that need an app id but already have their own
/// nanny.toml call this instead). Returns the id.
fn write_app_identity(dir: &Path) -> String {
    let ts = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos();
    let seq = COUNTER.fetch_add(1, Ordering::Relaxed);
    let id = format!("app_test_{ts}_{seq}");
    let dot_nanny = dir.join(".nanny");
    fs::create_dir_all(&dot_nanny).unwrap();
    fs::write(
        dot_nanny.join("app.json"),
        format!("{{\"app_id\": \"{id}\", \"name\": \"test-app\"}}\n"),
    )
    .unwrap();
    id
}

/// Ask the OS for a port that is genuinely free right now.
///
/// Binding to port 0 makes the kernel pick one, then we release it and hand
/// the number to the server the test is about to spawn.
///
/// Hardcoded port numbers do not work here. These tests run in parallel, so a
/// reused number makes a test that passes alone fail in a suite; and across
/// back-to-back `cargo test` runs the previous run's sockets linger in
/// TIME_WAIT on those exact numbers. Both failure modes look like a broken
/// server rather than a broken test, which is the worst kind of flake.
fn free_port() -> u16 {
    std::net::TcpListener::bind("127.0.0.1:0")
        .expect("the OS must be able to hand out an ephemeral port")
        .local_addr()
        .expect("a bound listener always has a local address")
        .port()
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
[start]
cmd = \"{slow_cmd}\"

[limits]
steps   = 100
tokens  = 1000
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

/// ExecutionStopped carries numeric `steps` and `tokens_spent` fields.
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
        v["tokens_spent"].is_number(),
        "ExecutionStopped must have a numeric `tokens_spent` field; got: {v}"
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
        r#"[limits]
steps   = 10
tokens  = 100
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

// ── T6: nanny run --serve — non-loopback without certs fails fast ────────────
//
// `server.rs` bails before starting the server when the bind address is
// non-loopback and cert files don't exist. Tests the error message content
// so developers know exactly what to do (nanny certs generate).

#[test]
fn server_start_nonloopback_without_certs_exits_with_message() {
    let dir   = temp_dir();
    let home  = temp_dir(); // override HOME so no ~/.nanny/certs/ exists

    // Write a minimal nanny.toml so the config load succeeds, plus an app
    // identity, `--serve` requires one to key its state.
    fs::write(
        dir.join("nanny.toml"),
        r#"[start]
cmd = "echo hello"

[limits]
steps   = 10
tokens  = 100
timeout = 5000

[observability]
log = "stdout"
"#,
    )
    .unwrap();
    write_app_identity(&dir);

    // Use a high port to avoid conflicts. Non-loopback → cert check fires.
    let output = Command::new(nanny_bin())
        .current_dir(&dir)
        .env("HOME", &home)
        .args(["run", "--serve", "--addr", "0.0.0.0:62998"])
        .output()
        .expect("nanny run --serve must run");

    let _ = fs::remove_dir_all(&dir);
    let _ = fs::remove_dir_all(&home);

    assert!(
        !output.status.success(),
        "nanny run --serve must exit non-zero when certs are missing for non-loopback"
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("mTLS") || stderr.contains("certs generate") || stderr.contains("not found"),
        "stderr must mention cert requirement; got: {stderr}"
    );
}

// ── T7: nanny run --serve — loopback does NOT require cert files ─────────────
//
// Default addr is 127.0.0.1 (loopback) → no cert check → server binds
// successfully even when ~/.nanny/certs/ doesn't exist.
// Regression guard: if the cert check accidentally runs for loopback, the
// server would fail to start and this test would catch it.
//
// Sandboxed via NANNY_HOME, not HOME: `dirs::home_dir()` ignores the `HOME`
// env override on Windows, so `nanny_server_state_dir` reads `NANNY_HOME`
// first (see `commands::server::nanny_home_dir`), which works identically on
// every platform since it's a plain env var read, not an OS profile lookup.

#[test]
fn server_start_loopback_does_not_require_cert_files() {
    let dir  = temp_dir();
    let home = temp_dir(); // fresh NANNY_HOME — no certs directory

    fs::write(
        dir.join("nanny.toml"),
        r#"[start]
cmd = "echo hello"

[limits]
steps   = 10
tokens  = 100
timeout = 5000

[observability]
log = "stdout"
"#,
    )
    .unwrap();
    write_app_identity(&dir);

    // Pick a port for the server. We'll probe it then kill the process.
    let port = free_port();

    let mut child = Command::new(nanny_bin())
        .current_dir(&dir)
        .env("NANNY_HOME", &home)
        .args(["run", "--serve", "--addr", &format!("127.0.0.1:{port}")])
        .spawn()
        .expect("nanny run --serve must spawn");

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
        "nanny run --serve on loopback must bind successfully without cert files"
    );
}

// ── T8: nanny run --join=<id> joins an explicit governance server ────────────
//
// There is no auto-detection anymore, two unrelated apps' governors on one
// machine must never collide by one silently absorbing the other's run. A
// server started with `nanny run --serve` (which requires an app identity, to
// key its state) is joined only by `nanny run --join=<that id>`.
//
// Sandboxed via NANNY_HOME, not HOME (see T7's comment for why).

#[test]
fn nanny_run_joins_explicit_server_and_prints_message() {
    let dir  = temp_dir();
    let home = temp_dir();

    // Write nanny.toml for the joining client, deliberately different from
    // the server's, to prove the client's own config isn't what matters here.
    fs::write(
        dir.join("nanny.toml"),
        r#"[start]
cmd = "echo nanny-detection-test"

[limits]
steps   = 10
tokens  = 100
timeout = 10000

[observability]
log = "stdout"
"#,
    )
    .unwrap();

    // Start a plain-HTTP governance server on a loopback port, with its own
    // app identity, `--serve` requires one to key its state.
    let server_port = free_port();
    let server_toml_dir = temp_dir();
    fs::write(
        server_toml_dir.join("nanny.toml"),
        // No [start]: these tests want a headless governor. `--serve` runs
        // [start].cmd when nanny.toml declares one, so a dummy command here
        // would launch, exit instantly, and take the governor down with it.
        r#"[limits]
steps   = 100
tokens  = 1000
timeout = 60000

[observability]
log = "stdout"
"#,
    )
    .unwrap();
    let app_id = write_app_identity(&server_toml_dir);

    let mut server = Command::new(nanny_bin())
        .current_dir(&server_toml_dir)
        .env("NANNY_HOME", &home)
        .args(["run", "--serve", "--addr", &format!("127.0.0.1:{server_port}")])
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

    // Join it explicitly by id.
    let output = Command::new(nanny_bin())
        .current_dir(&dir)
        .env("NANNY_HOME", &home)
        .args(["--config", &config_arg(&dir), "run", &format!("--join={app_id}")])
        .output()
        .expect("nanny run --join must complete");

    let _ = server.kill();
    let _ = server.wait();
    let _ = fs::remove_dir_all(&dir);
    let _ = fs::remove_dir_all(&home);
    let _ = fs::remove_dir_all(&server_toml_dir);

    assert!(
        output.status.success(),
        "nanny run --join must exit 0 when routing through the joined server\nstdout: {}\nstderr: {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("network server detected at"),
        "nanny run --join must print 'network server detected at' when the server is reachable\ngot: {stdout}"
    );
}

// ── T8b: a joined app is attributed to ITSELF, not to the governor ───────────
//
// The load-bearing property of the whole app-identity design: one governor
// holds one credential and serves many apps, and each must still land under its
// own name. If a joined app inherited the governor's identity, a fleet would
// collapse into one row on the dashboard and per-app cost would be a fiction.
//
// Sandboxed via NANNY_HOME, not HOME (see T7's comment for why).

#[test]
fn joined_app_is_attributed_to_its_own_identity() {
    let home = temp_dir();

    // The governor, with its own identity and a file event log to read back.
    let server_dir = temp_dir();
    fs::write(
        server_dir.join("nanny.toml"),
        // No [start]: these tests want a headless governor. `--serve` runs
        // [start].cmd when nanny.toml declares one, so a dummy command here
        // would launch, exit instantly, and take the governor down with it.
        r#"[limits]
steps   = 100
tokens  = 1000
timeout = 60000

[observability]
log = "file"
"#,
    )
    .unwrap();
    let governor_id = write_app_identity(&server_dir);

    // The joining app, with a DIFFERENT identity.
    let client_dir = temp_dir();
    fs::write(
        client_dir.join("nanny.toml"),
        r#"[start]
cmd = "echo joined-work"

[limits]
steps   = 10
tokens  = 100
timeout = 10000
"#,
    )
    .unwrap();
    let joiner_id = write_app_identity(&client_dir);
    assert_ne!(governor_id, joiner_id, "the two apps must be distinct for this test to mean anything");

    let server_port = free_port();
    let mut server = Command::new(nanny_bin())
        .current_dir(&server_dir)
        .env("NANNY_HOME", &home)
        .args(["run", "--serve", "--addr", &format!("127.0.0.1:{server_port}")])
        .spawn()
        .expect("governance server must spawn");

    let mut ready = false;
    for _ in 0..50 {
        if std::net::TcpStream::connect(format!("127.0.0.1:{server_port}")).is_ok() {
            ready = true;
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
    assert!(ready, "governance server must become ready within 5 s");

    let output = Command::new(nanny_bin())
        .current_dir(&client_dir)
        .env("NANNY_HOME", &home)
        .args(["--config", &config_arg(&client_dir), "run", &format!("--join={governor_id}")])
        .output()
        .expect("nanny run --join must complete");

    // The governor drains events on a 250 ms tick, so give it room to flush.
    let log_path = server_dir.join(".nanny").join("logs").join("log.ndjson");
    let mut events = String::new();
    for _ in 0..40 {
        events = fs::read_to_string(&log_path).unwrap_or_default();
        if events.contains("AppIdentified") {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
    }

    let _ = server.kill();
    let _ = server.wait();
    let _ = fs::remove_dir_all(&client_dir);
    let _ = fs::remove_dir_all(&server_dir);
    let _ = fs::remove_dir_all(&home);

    assert!(
        output.status.success(),
        "nanny run --join must exit 0\nstdout: {}\nstderr: {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );
    assert!(
        events.contains(&joiner_id),
        "the governor's log must attribute the run to the JOINING app ({joiner_id})\ngot: {events}"
    );
    assert!(
        !events.contains(&governor_id),
        "the joined run must not be filed under the governor's own id ({governor_id})\ngot: {events}"
    );
}

// ── T9: nanny run --join=<id> fails loudly when that server isn't reachable ──
//
// An explicit `--join` that doesn't find its target is a mistake worth
// surfacing, not something to quietly fall back to a local bridge for, the
// old auto-detect behavior silently cleaned up stale state and ran locally
// instead; the new explicit-join behavior errors instead, since the caller
// asked for a SPECIFIC governor by id.
//
// Sandboxed via NANNY_HOME, not HOME (see T7's comment for why).

#[test]
fn join_to_unreachable_server_fails_loudly() {
    let dir  = temp_dir();
    let home = temp_dir();

    fs::write(
        dir.join("nanny.toml"),
        r#"[start]
cmd = "echo nanny-stale-test"

[limits]
steps   = 10
tokens  = 100
timeout = 10000

[observability]
log = "stdout"
"#,
    )
    .unwrap();

    // Write server state for an app id, pointing at a port with nothing
    // listening, port 1 is typically reserved / always unreachable locally.
    let app_id = "app_test_unreachable";
    let state_dir = home.join(".nanny").join("servers").join(app_id);
    fs::create_dir_all(&state_dir).unwrap();
    fs::write(state_dir.join("server.addr"), "127.0.0.1:1").unwrap();
    fs::write(state_dir.join("server.token"), "stale-token").unwrap();

    let output = Command::new(nanny_bin())
        .current_dir(&dir)
        .env("NANNY_HOME", &home)
        .args(["--config", &config_arg(&dir), "run", &format!("--join={app_id}")])
        .output()
        .expect("nanny run --join must complete");

    let _ = fs::remove_dir_all(&dir);
    let _ = fs::remove_dir_all(&home);

    assert!(
        !output.status.success(),
        "nanny run --join must exit non-zero when the target server isn't reachable"
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("not reachable") || stderr.contains(app_id),
        "stderr must explain the join failure by app id; got: {stderr}"
    );
}

// ── T8c: `--serve` runs [start] under the governor, and stays joinable ───────
//
// The whole point of letting --serve launch the app: one command, no launcher
// script. A script has to poll for readiness, runs `sh` as PID 1 so SIGTERM
// never reaches the governor, and orphans one half if the other dies. Doing it
// in-process removes all three.
//
// Also pins that launching an app of its own does NOT make the governor
// exclusive — it is still a server, and other processes can still join it.

#[test]
fn serve_runs_the_start_command_and_remains_joinable() {
    let home = temp_dir();

    let server_dir = temp_dir();
    fs::write(
        server_dir.join("nanny.toml"),
        r#"[start]
cmd = "sh -c \"echo SERVE-RAN-THE-APP; sleep 5\""

[limits]
steps   = 100
tokens  = 1000
timeout = 60000

[observability]
log = "file"
"#,
    )
    .unwrap();
    let governor_id = write_app_identity(&server_dir);

    let server_port = free_port();
    let mut server = Command::new(nanny_bin())
        .current_dir(&server_dir)
        .env("NANNY_HOME", &home)
        .args(["run", "--serve", "--addr", &format!("127.0.0.1:{server_port}")])
        .stdout(std::process::Stdio::piped())
        .spawn()
        .expect("governor must spawn");

    let mut ready = false;
    for _ in 0..100 {
        if std::net::TcpStream::connect(format!("127.0.0.1:{server_port}")).is_ok() {
            ready = true;
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
    assert!(ready, "governor must become ready within 10 s");

    // While the governor's own app is still running, a separate process must
    // still be able to join — launching an app does not close the door.
    let client_dir = temp_dir();
    fs::write(
        client_dir.join("nanny.toml"),
        r#"[start]
cmd = "echo JOINED-WHILE-SERVING"

[limits]
steps   = 10
tokens  = 100
timeout = 10000
"#,
    )
    .unwrap();
    write_app_identity(&client_dir);

    let joined = Command::new(nanny_bin())
        .current_dir(&client_dir)
        .env("NANNY_HOME", &home)
        .args(["--config", &config_arg(&client_dir), "run", &format!("--join={governor_id}")])
        .output()
        .expect("join must complete");

    let _ = server.kill();
    let server_out = server.wait_with_output().expect("governor must be reaped");

    let _ = fs::remove_dir_all(&client_dir);
    let _ = fs::remove_dir_all(&server_dir);
    let _ = fs::remove_dir_all(&home);

    let server_stdout = String::from_utf8_lossy(&server_out.stdout);
    assert!(
        server_stdout.contains("SERVE-RAN-THE-APP"),
        "--serve must run [start].cmd, not ignore it\ngot: {server_stdout}"
    );
    assert!(
        server_stdout.contains("running [start] under this governor"),
        "--serve must say it is launching the app\ngot: {server_stdout}"
    );

    let joined_stdout = String::from_utf8_lossy(&joined.stdout);
    assert!(
        joined.status.success() && joined_stdout.contains("JOINED-WHILE-SERVING"),
        "a governor running its own app must still accept joins\nstdout: {joined_stdout}\nstderr: {}",
        String::from_utf8_lossy(&joined.stderr),
    );
}

// ── T8d: `--serve` with no [start] stays headless ────────────────────────────

#[test]
fn serve_without_a_start_section_stays_headless() {
    let home = temp_dir();
    let server_dir = temp_dir();
    fs::write(
        server_dir.join("nanny.toml"),
        r#"[limits]
steps   = 100
tokens  = 1000
timeout = 60000
"#,
    )
    .unwrap();
    write_app_identity(&server_dir);

    let server_port = free_port();
    let mut server = Command::new(nanny_bin())
        .current_dir(&server_dir)
        .env("NANNY_HOME", &home)
        .args(["run", "--serve", "--addr", &format!("127.0.0.1:{server_port}")])
        .stdout(std::process::Stdio::piped())
        .spawn()
        .expect("governor must spawn");

    let mut ready = false;
    for _ in 0..100 {
        if std::net::TcpStream::connect(format!("127.0.0.1:{server_port}")).is_ok() {
            ready = true;
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
    }

    let _ = server.kill();
    let out = server.wait_with_output().expect("governor must be reaped");
    let _ = fs::remove_dir_all(&server_dir);
    let _ = fs::remove_dir_all(&home);

    assert!(ready, "a headless governor must still come up");
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("running headless"),
        "no [start] must mean headless, and say so\ngot: {stdout}"
    );
}

// ── T9b: plain `nanny run` refuses a proxy allowlist it cannot enforce ───────
//
// The in-process bridge listens on a Unix socket and has no CONNECT proxy;
// HTTPS_PROXY can only name a host:port, so this path can never honor
// [proxy] allowed_hosts. Before this guard the section was silently inert:
// no injection, no enforcement, no warning, traffic leaving ungoverned while
// nanny.toml said otherwise — the same fail-open G8 fixed on the --join path
// while leaving it open on this one.

#[test]
fn plain_run_refuses_a_proxy_allowlist_it_cannot_enforce() {
    let dir = temp_dir();
    fs::write(
        dir.join("nanny.toml"),
        r#"[start]
cmd = "echo should-never-run"

[limits]
steps   = 10
tokens  = 100
timeout = 10000

[proxy]
allowed_hosts = ["api.openai.com"]
"#,
    )
    .unwrap();

    let output = Command::new(nanny_bin())
        .args(["--config", &config_arg(&dir), "run"])
        .output()
        .expect("nanny run must complete");

    let _ = fs::remove_dir_all(&dir);

    assert!(!output.status.success(), "must refuse to start, not fail open");

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("allowed_hosts"),
        "the error must name the setting that caused it\ngot: {stderr}"
    );
    assert!(
        stderr.contains("--serve"),
        "the error must point at the path that CAN enforce it\ngot: {stderr}"
    );

    // The whole point: the agent must not have run.
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        !stdout.contains("should-never-run"),
        "the child must never be spawned when a declared control can't be enforced\ngot: {stdout}"
    );
}

// ── T10: proxy env vars are auto-injected when the SERVER has [proxy] configured ─
//
// The server's own nanny.toml (not the client's) decides whether [proxy]
// allowed_hosts is active. When it is, cmd_run_via_network_server must set
// HTTPS_PROXY / HTTP_PROXY (and lowercase) on the child pointing at the
// governor, plus NO_PROXY for the loopback address — without the dev setting
// anything by hand. Uses a different directory/nanny.toml for the server vs.
// the client on purpose, matching how these are used in practice.
//
// Sandboxed via NANNY_HOME, not HOME (see T7's comment for why).

#[test]
fn proxy_env_vars_injected_when_server_has_proxy_configured() {
    let dir  = temp_dir();
    let home = temp_dir();

    // Client nanny.toml — deliberately has NO [proxy] section, to prove the
    // decision comes from the server's config, not this one. [start].cmd is
    // spawned directly (no shell), so "sh -c ..." only works where a real
    // `sh` is on PATH — use cmd.exe's own syntax on Windows instead.
    #[cfg(not(windows))]
    let print_cmd = "sh -c 'echo GOT:HTTPS_PROXY=$HTTPS_PROXY:HTTP_PROXY=$HTTP_PROXY:NO_PROXY=$NO_PROXY'";
    #[cfg(windows)]
    let print_cmd = "cmd /c 'echo GOT:HTTPS_PROXY=%HTTPS_PROXY%:HTTP_PROXY=%HTTP_PROXY%:NO_PROXY=%NO_PROXY%'";

    fs::write(
        dir.join("nanny.toml"),
        format!(
            r#"[start]
cmd = "{print_cmd}"

[limits]
steps   = 10
tokens  = 100
timeout = 10000

[observability]
log = "stdout"
"#
        ),
    )
    .unwrap();

    // Server nanny.toml — [proxy] allowed_hosts is what should drive injection.
    let server_toml_dir = temp_dir();
    fs::write(
        server_toml_dir.join("nanny.toml"),
        // No [start]: these tests want a headless governor. `--serve` runs
        // [start].cmd when nanny.toml declares one, so a dummy command here
        // would launch, exit instantly, and take the governor down with it.
        r#"[limits]
steps   = 100
tokens  = 1000
timeout = 60000

[proxy]
allowed_hosts = ["api.openai.com"]

[observability]
log = "stdout"
"#,
    )
    .unwrap();
    let app_id = write_app_identity(&server_toml_dir);

    let server_port = free_port();
    let mut server = Command::new(nanny_bin())
        .current_dir(&server_toml_dir)
        .env("NANNY_HOME", &home)
        .args(["run", "--serve", "--addr", &format!("127.0.0.1:{server_port}")])
        .spawn()
        .expect("governance server must spawn");

    let mut ready = false;
    for _ in 0..50 {
        if std::net::TcpStream::connect(format!("127.0.0.1:{server_port}")).is_ok() {
            ready = true;
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
    assert!(ready, "governance server must become ready within 5 s");

    // proxy_token, a separate CONNECT-only credential from the session token,
    // is embedded as userinfo in the injected proxy URL (so the child's HTTP
    // client sends it as Proxy-Authorization on CONNECT). Read the real value
    // the server wrote so the assertion below matches exactly.
    let token = fs::read_to_string(
        home.join(".nanny").join("servers").join(&app_id).join("server.proxy_token"),
    )
    .expect("server.proxy_token must exist")
    .trim()
    .to_string();

    let output = Command::new(nanny_bin())
        .current_dir(&dir)
        .env("NANNY_HOME", &home)
        .args(["--config", &config_arg(&dir), "run", &format!("--join={app_id}")])
        .output()
        .expect("nanny run must complete");

    let _ = server.kill();
    let _ = server.wait();
    let _ = fs::remove_dir_all(&dir);
    let _ = fs::remove_dir_all(&home);
    let _ = fs::remove_dir_all(&server_toml_dir);

    assert!(
        output.status.success(),
        "nanny run must exit 0\nstdout: {}\nstderr: {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );

    let stdout = String::from_utf8_lossy(&output.stdout);
    let expected = format!(
        "GOT:HTTPS_PROXY=http://{token}:@127.0.0.1:{server_port}:HTTP_PROXY=http://{token}:@127.0.0.1:{server_port}:NO_PROXY=127.0.0.1,localhost"
    );
    assert!(
        stdout.contains(&expected),
        "child must see HTTPS_PROXY/HTTP_PROXY/NO_PROXY pointed at the governor, \
         with the session token embedded as Proxy-Authorization userinfo\n\
         expected to find: {expected}\ngot stdout: {stdout}"
    );
}

// ── T11: proxy env vars are NOT injected when the server has no [proxy] ──────
//
// Regression guard for the opposite case: a server with no [proxy] section
// (or an empty allowed_hosts) must not cause the child to get HTTPS_PROXY/
// HTTP_PROXY pointed at it — that would route all outbound traffic into a
// proxy that immediately 404s every CONNECT ("proxy not configured"),
// breaking legitimate calls.
//
// Sandboxed via NANNY_HOME, not HOME (see T7's comment for why).

#[test]
fn proxy_env_vars_not_injected_when_server_has_no_proxy_configured() {
    let dir  = temp_dir();
    let home = temp_dir();

    // cmd.exe and sh disagree on how an UNSET variable prints: sh's $VAR
    // expands to empty, cmd.exe's %VAR% is left as the literal text when the
    // variable doesn't exist. The expected assertion below accounts for that.
    #[cfg(not(windows))]
    let print_cmd = "sh -c 'echo GOT:HTTPS_PROXY=[$HTTPS_PROXY]'";
    #[cfg(windows)]
    let print_cmd = "cmd /c 'echo GOT:HTTPS_PROXY=[%HTTPS_PROXY%]'";

    fs::write(
        dir.join("nanny.toml"),
        format!(
            r#"[start]
cmd = "{print_cmd}"

[limits]
steps   = 10
tokens  = 100
timeout = 10000

[observability]
log = "stdout"
"#
        ),
    )
    .unwrap();

    // Server nanny.toml — no [proxy] section at all.
    let server_toml_dir = temp_dir();
    fs::write(
        server_toml_dir.join("nanny.toml"),
        // No [start]: these tests want a headless governor. `--serve` runs
        // [start].cmd when nanny.toml declares one, so a dummy command here
        // would launch, exit instantly, and take the governor down with it.
        r#"[limits]
steps   = 100
tokens  = 1000
timeout = 60000

[observability]
log = "stdout"
"#,
    )
    .unwrap();
    let app_id = write_app_identity(&server_toml_dir);

    let server_port = free_port();
    let mut server = Command::new(nanny_bin())
        .current_dir(&server_toml_dir)
        .env("NANNY_HOME", &home)
        .args(["run", "--serve", "--addr", &format!("127.0.0.1:{server_port}")])
        .spawn()
        .expect("governance server must spawn");

    let mut ready = false;
    for _ in 0..50 {
        if std::net::TcpStream::connect(format!("127.0.0.1:{server_port}")).is_ok() {
            ready = true;
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
    assert!(ready, "governance server must become ready within 5 s");

    let output = Command::new(nanny_bin())
        .current_dir(&dir)
        .env("NANNY_HOME", &home)
        .args(["--config", &config_arg(&dir), "run", &format!("--join={app_id}")])
        .output()
        .expect("nanny run must complete");

    let _ = server.kill();
    let _ = server.wait();
    let _ = fs::remove_dir_all(&dir);
    let _ = fs::remove_dir_all(&home);
    let _ = fs::remove_dir_all(&server_toml_dir);

    assert!(
        output.status.success(),
        "nanny run must exit 0\nstdout: {}\nstderr: {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );

    let stdout = String::from_utf8_lossy(&output.stdout);
    #[cfg(not(windows))]
    let expected = "GOT:HTTPS_PROXY=[]";
    #[cfg(windows)]
    let expected = "GOT:HTTPS_PROXY=[%HTTPS_PROXY%]";
    assert!(
        stdout.contains(expected),
        "HTTPS_PROXY must be unset when the server has no [proxy] configured\ngot stdout: {stdout}"
    );
}
