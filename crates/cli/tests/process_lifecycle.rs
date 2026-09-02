// Integration tests for `nanny run` process lifecycle.
//
// These tests build and invoke the real `nanny` binary.
// They verify that a process exiting cleanly produces exit code 0, that a
// crash surfaces as a typed stop reason, and that the NDJSON stream is
// well-formed and correctly bookended.
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

fn write_config(dir: &Path, cmd: &str) {
    let toml = format!(
        r#"[start]
cmd = "{cmd}"

[tools]
allowed = ["http_get"]

[observability]
log = "stdout"
"#
    );
    fs::write(dir.join("nanny.toml"), toml).unwrap();
}

/// Config that logs to a file rather than stdout, so a test can read the
/// append-only log back off disk the way an operator would.
fn write_config_logging_to_file(dir: &Path, cmd: &str) {
    let toml = format!(
        r#"[start]
cmd = "{cmd}"

[tools]
allowed = ["http_get"]

[observability]
log = "file"
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
    let ts = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
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

/// Polls until the governor for `app_id` records the address it bound, and
/// returns it.
///
/// Tests ask for `127.0.0.1:0` and read the port back from here rather than
/// picking one up front. Probing for a free port and then handing the number
/// to a child process leaves a window in which any other process can take it,
/// and `--addr` is exact-or-error for everything but the default address, so
/// the governor exits with "address already in use" instead of stepping aside.
/// Under `cargo test --workspace`, where many test binaries claim ephemeral
/// ports at once, that window is hit often enough to matter. Asking for port 0
/// closes it: the kernel assigns the port at bind time and cannot hand the
/// same one to anyone else.
///
/// The server writes this file only after a successful bind, so its appearance
/// is also the readiness signal a port probe used to provide.
fn wait_for_bound_addr(home: &Path, app_id: &str, attempts: u32) -> Option<String> {
    let addr_file = home
        .join(".nanny")
        .join("servers")
        .join(app_id)
        .join("server.addr");
    for _ in 0..attempts {
        // Written whole via one `write`, but read defensively anyway: an empty
        // read here would otherwise surface as an unparseable address later.
        if let Ok(addr) = std::fs::read_to_string(&addr_file) {
            let addr = addr.trim().to_string();
            if !addr.is_empty() {
                return Some(addr);
            }
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
    None
}

/// Polls until `path` exists, or `attempts` × 100 ms elapse.
///
/// Used where "the port is listening" is not the property under test. The
/// governor binds its listener before it spawns `[start].cmd`, so a test that
/// asserts the app ran has to wait for the app, not for the socket.
fn wait_for_file(path: &Path, attempts: u32) -> bool {
    for _ in 0..attempts {
        if path.exists() {
            return true;
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
    false
}

// ── Tests ─────────────────────────────────────────────────────────────────────

/// A process that exits on its own completes cleanly: exit code 0.
#[test]
fn fast_exit_completes_cleanly() {
    let dir = temp_dir();
    write_config(&dir, "echo hello");

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
    assert!(
        stdout.contains("ExecutionStarted"),
        "stdout must have ExecutionStarted event"
    );
    assert!(
        stdout.contains("ExecutionStopped"),
        "stdout must have ExecutionStopped event"
    );
    assert!(
        stdout.contains("AgentCompleted"),
        "stop reason must be AgentCompleted"
    );
}

/// Bridge events (ToolAllowed, RuleDenied, ToolDenied, …) are flushed into the NDJSON
/// stream before ExecutionStopped, so ExecutionStopped is always the last line.
///
/// This test uses `echo` as the child command: it exits immediately without
/// making any bridge tool calls, so no per-tool events are produced.  The key
/// assertion is structural: every line is valid JSON and ExecutionStopped is last.
#[test]
fn execution_stopped_is_always_last_line() {
    let dir = temp_dir();
    write_config(&dir, "echo nanny-test");

    let output = Command::new(nanny_bin())
        .args(["--config", &config_arg(&dir), "run"])
        .output()
        .expect("failed to run nanny");

    let _ = fs::remove_dir_all(&dir);

    let stdout = String::from_utf8_lossy(&output.stdout);
    let lines: Vec<&str> = stdout
        .lines()
        .filter(|l| l.trim_start().starts_with('{'))
        .collect();

    assert!(
        !lines.is_empty(),
        "stdout must contain at least one NDJSON line"
    );

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
/// This test uses `echo`: no bridge tool calls, so both values are
/// legitimately 0. The point is to assert the fields are present and
/// numeric, catching any regression where they are hardcoded to 0 even
/// when tools are called.  See the bridge-level
/// `tool_call_increments_step_and_charges_cost` test for the non-zero case.
#[test]
fn execution_stopped_has_accounting_fields() {
    let dir = temp_dir();
    write_config(&dir, "echo nanny-accounting-test");

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
    write_config(&dir, "false");

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
        r#"[observability]
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

// ── nanny run --serve: non-loopback without certs fails fast ────────────
//
// `server.rs` bails before starting the server when the bind address is
// non-loopback and cert files don't exist. Tests the error message content
// so developers know exactly what to do (nanny certs generate).

#[test]
fn server_start_nonloopback_without_certs_exits_with_message() {
    let dir = temp_dir();
    let home = temp_dir(); // override HOME so no ~/.nanny/certs/ exists

    // Write a minimal nanny.toml so the config load succeeds, plus an app
    // identity, `--serve` requires one to key its state.
    fs::write(
        dir.join("nanny.toml"),
        r#"[start]
cmd = "echo hello"

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
        stderr.contains("mTLS")
            || stderr.contains("certs generate")
            || stderr.contains("not found"),
        "stderr must mention cert requirement; got: {stderr}"
    );
}

// ── nanny run --serve: loopback does NOT require cert files ─────────────
//
// Default addr is 127.0.0.1 (loopback) → no cert check → server binds
// successfully even when ~/.nanny/certs/ doesn't exist.
// Regression guard: if the cert check accidentally runs for loopback, the
// server would fail to start and this test would catch it.
//
// Deliberately no [start]: `--serve` runs [start] under the governor and tears
// the governor down the moment that app exits, so a fixture like
// `cmd = "echo hello"` leaves the listener open for only a few milliseconds.
// This test is about the bind, not about running an app, so it keeps the
// governor headless and therefore alive until the test kills it. The
// serve-plus-app path is covered further down.
//
// Sandboxed via NANNY_HOME, not HOME: `dirs::home_dir()` ignores the `HOME`
// env override on Windows, so `nanny_server_state_dir` reads `NANNY_HOME`
// first (see `commands::server::nanny_home_dir`), which works identically on
// every platform since it's a plain env var read, not an OS profile lookup.

#[test]
fn server_start_loopback_does_not_require_cert_files() {
    let dir = temp_dir();
    let home = temp_dir(); // fresh NANNY_HOME, no certs directory

    fs::write(
        dir.join("nanny.toml"),
        r#"[observability]
log = "stdout"
"#,
    )
    .unwrap();
    let app_id = write_app_identity(&dir);

    let mut child = Command::new(nanny_bin())
        .current_dir(&dir)
        .env("NANNY_HOME", &home)
        .args(["run", "--serve", "--addr", "127.0.0.1:0"])
        .spawn()
        .expect("nanny run --serve must spawn");

    // Wait for the governor to record the address it bound (up to 5s).
    let ready = wait_for_bound_addr(&home, &app_id, 50).is_some();

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

// ── nanny run --join=<id> joins an explicit governance server ────────────
//
// There is no auto-detection anymore, two unrelated apps' governors on one
// machine must never collide by one silently absorbing the other's run. A
// server started with `nanny run --serve` (which requires an app identity, to
// key its state) is joined only by `nanny run --join=<that id>`.
//
// Sandboxed via NANNY_HOME, not HOME, for the reason given above the
// headless-bind test.

#[test]
fn nanny_run_joins_explicit_server_and_prints_message() {
    let dir = temp_dir();
    let home = temp_dir();

    // Write nanny.toml for the joining client, deliberately different from
    // the server's, to prove the client's own config isn't what matters here.
    fs::write(
        dir.join("nanny.toml"),
        r#"[start]
cmd = "echo nanny-detection-test"

[observability]
log = "stdout"
"#,
    )
    .unwrap();

    // Start a plain-HTTP governance server on a loopback port, with its own
    // app identity, `--serve` requires one to key its state.
    let server_toml_dir = temp_dir();
    fs::write(
        server_toml_dir.join("nanny.toml"),
        // No [start]: these tests want a headless governor. `--serve` runs
        // [start].cmd when nanny.toml declares one, so a dummy command here
        // would launch, exit instantly, and take the governor down with it.
        r#"[observability]
log = "stdout"
"#,
    )
    .unwrap();
    let app_id = write_app_identity(&server_toml_dir);

    let mut server = Command::new(nanny_bin())
        .current_dir(&server_toml_dir)
        .env("NANNY_HOME", &home)
        .args(["run", "--serve", "--addr", "127.0.0.1:0"])
        .spawn()
        .expect("governance server must spawn");

    // Wait for the server to be ready.
    let ready = wait_for_bound_addr(&home, &app_id, 50).is_some();
    assert!(ready, "governance server must become ready within 5 s");

    // Join it explicitly by id.
    let output = Command::new(nanny_bin())
        .current_dir(&dir)
        .env("NANNY_HOME", &home)
        .args([
            "--config",
            &config_arg(&dir),
            "run",
            &format!("--join={app_id}"),
        ])
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

// ── A joined app is attributed to ITSELF, not to the governor ───────────────
//
// The load-bearing property of the whole app-identity design: one governor
// holds one credential and serves many apps, and each must still land under its
// own name. If a joined app inherited the governor's identity, a fleet would
// collapse into one row on the dashboard and per-app cost would be a fiction.
//
// Sandboxed via NANNY_HOME, not HOME, for the reason given above the
// headless-bind test.

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
        r#"[observability]
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

"#,
    )
    .unwrap();
    let joiner_id = write_app_identity(&client_dir);
    assert_ne!(
        governor_id, joiner_id,
        "the two apps must be distinct for this test to mean anything"
    );

    let mut server = Command::new(nanny_bin())
        .current_dir(&server_dir)
        .env("NANNY_HOME", &home)
        .args(["run", "--serve", "--addr", "127.0.0.1:0"])
        .spawn()
        .expect("governance server must spawn");

    let ready = wait_for_bound_addr(&home, &governor_id, 50).is_some();
    assert!(ready, "governance server must become ready within 5 s");

    let output = Command::new(nanny_bin())
        .current_dir(&client_dir)
        .env("NANNY_HOME", &home)
        .args([
            "--config",
            &config_arg(&client_dir),
            "run",
            &format!("--join={governor_id}"),
        ])
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

// ── nanny run --join=<id> fails loudly when that server isn't reachable ──
//
// An explicit `--join` that doesn't find its target is a mistake worth
// surfacing, not something to quietly fall back to a local bridge for, the
// old auto-detect behavior silently cleaned up stale state and ran locally
// instead; the new explicit-join behavior errors instead, since the caller
// asked for a SPECIFIC governor by id.
//
// Sandboxed via NANNY_HOME, not HOME, for the reason given above the
// headless-bind test.

#[test]
fn join_to_unreachable_server_fails_loudly() {
    let dir = temp_dir();
    let home = temp_dir();

    fs::write(
        dir.join("nanny.toml"),
        r#"[start]
cmd = "echo nanny-stale-test"

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
        .args([
            "--config",
            &config_arg(&dir),
            "run",
            &format!("--join={app_id}"),
        ])
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

// ── `--serve` runs [start] under the governor, and stays joinable ───────────
//
// The whole point of letting --serve launch the app: one command, no launcher
// script. A script has to poll for readiness, runs `sh` as PID 1 so SIGTERM
// never reaches the governor, and orphans one half if the other dies. Doing it
// in-process removes all three.
//
// Also pins that launching an app of its own does NOT make the governor
// exclusive: it is still a server, and other processes can still join it.

#[test]
fn serve_runs_the_start_command_and_remains_joinable() {
    let home = temp_dir();

    let server_dir = temp_dir();
    // The app touches a marker before sleeping, so the test can wait for the
    // app itself rather than for the governor's socket.
    let app_marker = server_dir.join("app-started");
    // `sh -c "..."` is itself embedded in a TOML double-quoted string, so the
    // marker path must survive both TOML and shell escaping. Windows paths
    // contain backslashes, which TOML treats as escape characters (e.g. `\U`
    // is a unicode escape) and would corrupt the file. Forward slashes are
    // accepted as path separators on Windows too, so normalize to those
    // instead of escaping backslashes twice over.
    let app_marker_display = app_marker.display().to_string().replace('\\', "/");
    fs::write(
        server_dir.join("nanny.toml"),
        format!(
            r#"[start]
cmd = "sh -c \"echo SERVE-RAN-THE-APP; touch {}; sleep 5\""

[observability]
log = "file"
"#,
            app_marker_display
        ),
    )
    .unwrap();
    let governor_id = write_app_identity(&server_dir);

    let mut server = Command::new(nanny_bin())
        .current_dir(&server_dir)
        .env("NANNY_HOME", &home)
        .args(["run", "--serve", "--addr", "127.0.0.1:0"])
        .stdout(std::process::Stdio::piped())
        .spawn()
        .expect("governor must spawn");

    let ready = wait_for_bound_addr(&home, &governor_id, 100).is_some();
    assert!(ready, "governor must become ready within 10 s");

    // Binding the listener and launching [start].cmd are separate steps, and
    // this test is about the second one. Waiting only on the port kills the
    // governor in the gap between them.
    //
    // Recorded, not asserted here: a panic before `server.kill()` below leaks
    // a live governor that outlives the test run and holds its port, which
    // then breaks unrelated tests. Assert after the process is reaped.
    let app_started = wait_for_file(&app_marker, 100);

    // While the governor's own app is still running, a separate process must
    // still be able to join. Launching an app does not close the door.
    let client_dir = temp_dir();
    fs::write(
        client_dir.join("nanny.toml"),
        r#"[start]
cmd = "echo JOINED-WHILE-SERVING"

"#,
    )
    .unwrap();
    write_app_identity(&client_dir);

    let joined = Command::new(nanny_bin())
        .current_dir(&client_dir)
        .env("NANNY_HOME", &home)
        .args([
            "--config",
            &config_arg(&client_dir),
            "run",
            &format!("--join={governor_id}"),
        ])
        .output()
        .expect("join must complete");

    let _ = server.kill();
    let server_out = server.wait_with_output().expect("governor must be reaped");

    let _ = fs::remove_dir_all(&client_dir);
    let _ = fs::remove_dir_all(&server_dir);
    let _ = fs::remove_dir_all(&home);

    assert!(
        app_started,
        "--serve must launch [start].cmd within 10 s of becoming ready"
    );

    let server_stdout = String::from_utf8_lossy(&server_out.stdout);
    assert!(
        server_stdout.contains("SERVE-RAN-THE-APP"),
        "--serve must run [start].cmd, not ignore it\ngot: {server_stdout}"
    );
    assert!(
        server_stdout.contains("running [start] under this governor"),
        "--serve must say it is launching the app\ngot: {server_stdout}"
    );
    assert!(
        server_stdout.contains("under this governor (plain HTTP, loopback)"),
        "and must carry the transport, so the line adds something the block \
         above it did not already say\ngot: {server_stdout}"
    );
    assert!(
        !server_stdout.contains("governance server started  ("),
        "the transport belongs on one line only, not on the block header too\n\
         got: {server_stdout}"
    );

    let joined_stdout = String::from_utf8_lossy(&joined.stdout);
    assert!(
        joined.status.success() && joined_stdout.contains("JOINED-WHILE-SERVING"),
        "a governor running its own app must still accept joins\nstdout: {joined_stdout}\nstderr: {}",
        String::from_utf8_lossy(&joined.stderr),
    );
}

// ── `--serve` with no [start] stays headless ────────────────────────────────

#[test]
fn serve_without_a_start_section_stays_headless() {
    let home = temp_dir();
    let server_dir = temp_dir();
    fs::write(server_dir.join("nanny.toml"), r#""#).unwrap();
    let app_id = write_app_identity(&server_dir);

    let mut server = Command::new(nanny_bin())
        .current_dir(&server_dir)
        .env("NANNY_HOME", &home)
        .args(["run", "--serve", "--addr", "127.0.0.1:0"])
        .stdout(std::process::Stdio::piped())
        .spawn()
        .expect("governor must spawn");

    let ready = wait_for_bound_addr(&home, &app_id, 100).is_some();

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

// ── Run attribution ───────────────────────────────────────────────────────────

/// Two runs of the same project append to the same log file. Every line must
/// say which run it belongs to.
///
/// This is the shape `nanny run --serve` produces by default, where one
/// governor drains many concurrent runs into one file. It cannot be recovered
/// after the fact: draining is per-run and batched, so sorting by `ts` does not
/// reconstruct the interleaving, and pairing `ExecutionStarted` with
/// `ExecutionStopped` fails on exactly the runs worth investigating, because a
/// missing stop is a documented outcome (the process crashed) rather than a
/// parse error.
#[test]
fn two_runs_sharing_one_log_stay_attributable() {
    let dir = temp_dir();
    write_config_logging_to_file(&dir, "echo hello");

    for _ in 0..2 {
        let status = Command::new(nanny_bin())
            .args(["run", "--config", &config_arg(&dir)])
            .current_dir(&dir)
            .status()
            .expect("nanny run must execute");
        assert!(status.success(), "run must exit cleanly");
    }

    let log = fs::read_to_string(dir.join(".nanny/logs/log.ndjson"))
        .expect("file logging must produce a log");
    let lines: Vec<serde_json::Value> = log
        .lines()
        .filter(|l| !l.trim().is_empty())
        .map(|l| serde_json::from_str(l).expect("every line is JSON"))
        .collect();

    assert!(
        lines.len() >= 4,
        "two runs produce at least two bookends each"
    );

    let mut runs: std::collections::BTreeMap<String, Vec<u64>> = Default::default();
    for line in &lines {
        let run_id = line["run_id"]
            .as_str()
            .expect("every line carries a run id");
        assert!(!run_id.is_empty(), "run id must not be blank");
        runs.entry(run_id.to_string())
            .or_default()
            .push(line["seq"].as_u64().expect("every line carries a seq"));
    }

    assert_eq!(
        runs.len(),
        2,
        "two invocations are two runs, not one stream"
    );

    for (run_id, seqs) in runs {
        let mut sorted = seqs.clone();
        sorted.sort_unstable();
        sorted.dedup();
        assert_eq!(
            sorted.len(),
            seqs.len(),
            "run {run_id} must not reuse a seq"
        );
        assert_eq!(sorted[0], 0, "run {run_id} must start at seq 0");
    }
}

/// A run that never writes `ExecutionStopped` is still fully segmentable.
///
/// The event enum documents a missing stop as an auditable fact: the process
/// crashed. Any scheme that recovers attribution by pairing the bookends fails
/// here, which is why attribution rides on every line instead.
#[test]
fn a_run_without_a_stop_event_is_still_attributable() {
    let dir = temp_dir();
    write_config_logging_to_file(&dir, "echo hello");

    let status = Command::new(nanny_bin())
        .args(["run", "--config", &config_arg(&dir)])
        .current_dir(&dir)
        .status()
        .expect("nanny run must execute");
    assert!(status.success());

    let path = dir.join(".nanny/logs/log.ndjson");
    let log = fs::read_to_string(&path).expect("log exists");

    // Drop the trailing ExecutionStopped, simulating a crashed run.
    let kept: Vec<&str> = log.lines().filter(|l| !l.trim().is_empty()).collect();
    let truncated: Vec<&str> = kept[..kept.len() - 1].to_vec();
    assert!(!truncated.is_empty());

    for line in truncated {
        let v: serde_json::Value = serde_json::from_str(line).unwrap();
        assert!(
            v["run_id"].as_str().is_some_and(|s| !s.is_empty()),
            "attribution must not depend on the stop event"
        );
    }
}

/// `ExecutionStarted` carries the fingerprint of the config that governed the
/// run, and two runs under an unchanged config report the same one.
#[test]
fn execution_started_carries_a_stable_config_hash() {
    let dir = temp_dir();
    write_config_logging_to_file(&dir, "echo hello");

    for _ in 0..2 {
        Command::new(nanny_bin())
            .args(["run", "--config", &config_arg(&dir)])
            .current_dir(&dir)
            .status()
            .expect("nanny run must execute");
    }

    let log = fs::read_to_string(dir.join(".nanny/logs/log.ndjson")).unwrap();
    let hashes: Vec<String> = log
        .lines()
        .filter(|l| !l.trim().is_empty())
        .map(|l| serde_json::from_str::<serde_json::Value>(l).unwrap())
        .filter(|v| v["event"] == "ExecutionStarted")
        .map(|v| {
            v["config_hash"]
                .as_str()
                .expect("a hash is always present")
                .to_string()
        })
        .collect();

    assert_eq!(hashes.len(), 2, "each run bookends with its own grant");
    assert_eq!(hashes[0], hashes[1], "an unchanged config is one policy");
    assert_eq!(hashes[0].len(), 64, "sha256 hex");
}

/// `ExecutionStarted` carries the runtime's own version, so a fleet
/// operator can answer "which of my machines are on an old runtime" from the
/// log alone, without cross-referencing which binary happened to be deployed
/// where.
#[test]
fn execution_started_carries_the_runtime_version() {
    let dir = temp_dir();
    write_config_logging_to_file(&dir, "echo hello");

    Command::new(nanny_bin())
        .args(["run", "--config", &config_arg(&dir)])
        .current_dir(&dir)
        .status()
        .expect("nanny run must execute");

    let log = fs::read_to_string(dir.join(".nanny/logs/log.ndjson")).unwrap();
    let started = log
        .lines()
        .filter(|l| !l.trim().is_empty())
        .map(|l| serde_json::from_str::<serde_json::Value>(l).unwrap())
        .find(|v| v["event"] == "ExecutionStarted")
        .expect("ExecutionStarted is always the first event");

    assert_eq!(
        started["runtime_version"],
        env!("CARGO_PKG_VERSION"),
        "must match the binary that actually produced this run"
    );
}

// ── Rule packs ────────────────────────────────────────────────────────────────

/// A pack declared in config but absent from disk stops the run before it
/// starts.
///
/// Fail-closed applies to what Nanny was asked to govern. The operator asked
/// for these controls; running without them would be an agent that is less
/// governed than its own config claims, which is the one failure a governance
/// tool cannot have.
#[test]
fn a_missing_rule_pack_refuses_to_start() {
    let dir = temp_dir();
    fs::write(
        dir.join("nanny.toml"),
        r#"[start]
cmd = "echo hello"

[tools]
allowed = ["http_get"]

[rules]
extends = ["nanny:owasp@2.1.0"]

[observability]
log = "stdout"
"#,
    )
    .unwrap();

    let out = Command::new(nanny_bin())
        .args(["run", "--config", &config_arg(&dir)])
        .current_dir(&dir)
        .output()
        .expect("nanny run must execute");

    assert!(!out.status.success(), "a missing control must not start");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("nanny rules add nanny:owasp@2.1.0"),
        "the error must say how to fix it, got: {stderr}"
    );
}

/// An unpinned pack is a config error, not a resolution guess.
#[test]
fn an_unpinned_rule_pack_refuses_to_start() {
    let dir = temp_dir();
    fs::write(
        dir.join("nanny.toml"),
        r#"[start]
cmd = "echo hello"

[tools]
allowed = ["http_get"]

[rules]
extends = ["nanny:owasp"]

[observability]
log = "stdout"
"#,
    )
    .unwrap();

    let out = Command::new(nanny_bin())
        .args(["run", "--config", &config_arg(&dir)])
        .current_dir(&dir)
        .output()
        .expect("nanny run must execute");

    assert!(!out.status.success());
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("has no version"), "got: {stderr}");
}

/// An installed pack loads and the run proceeds.
#[test]
fn an_installed_rule_pack_starts_normally() {
    let dir = temp_dir();
    fs::write(
        dir.join("nanny.toml"),
        r#"[start]
cmd = "echo hello"

[tools]
allowed = ["http_get"]

[rules]
extends = ["nanny:recommended@1.0.0"]

[observability]
log = "stdout"
"#,
    )
    .unwrap();

    let pack = dir.join(".nanny/rules/nanny-recommended@1.0.0");
    fs::create_dir_all(&pack).unwrap();
    fs::write(
        pack.join("pack.toml"),
        "name = \"nanny:recommended\"\nversion = \"1.0.0\"\nrules = [\"no_send_after_read\"]\n",
    )
    .unwrap();

    let out = Command::new(nanny_bin())
        .args(["run", "--config", &config_arg(&dir)])
        .current_dir(&dir)
        .output()
        .expect("nanny run must execute");

    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("nanny:recommended@1.0.0"),
        "the run must name its packs"
    );
}

/// `nanny rules add` vendors the pack and declares it, and never touches source.
#[test]
fn rules_add_vendors_the_pack_and_declares_it() {
    let dir = temp_dir();
    fs::write(
        dir.join("nanny.toml"),
        "# governs the outreach agent\n[start]\ncmd = \"echo hello\"\n\n[tools]\nallowed = [\"http_get\"]\n",
    )
    .unwrap();
    fs::write(dir.join("agent.py"), "# untouched\n").unwrap();

    let src = temp_dir();
    fs::write(
        src.join("pack.toml"),
        "name = \"nanny:owasp\"\nversion = \"2.1.0\"\nrules = [\"no_send_after_read\"]\n",
    )
    .unwrap();
    fs::write(
        src.join("rules.py"),
        "def no_send_after_read(ctx): return True\n",
    )
    .unwrap();

    let out = Command::new(nanny_bin())
        .args([
            "rules",
            "add",
            "nanny:owasp@2.1.0",
            "--from",
            &src.to_string_lossy(),
        ])
        .current_dir(&dir)
        .output()
        .expect("nanny rules add must execute");
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );

    // Vendored, so the same controls run on every machine.
    assert!(dir.join(".nanny/rules/nanny-owasp@2.1.0/rules.py").exists());

    // Declared, pinned, and the operator's comment survives.
    let toml = fs::read_to_string(dir.join("nanny.toml")).unwrap();
    assert!(toml.contains("nanny:owasp@2.1.0"), "got: {toml}");
    assert!(
        toml.contains("# governs the outreach agent"),
        "comments must survive"
    );

    // Source untouched: an installed rule is never pasted into user code.
    assert_eq!(
        fs::read_to_string(dir.join("agent.py")).unwrap(),
        "# untouched\n"
    );

    // The run now starts, because the declared pack is present.
    let run = Command::new(nanny_bin())
        .args(["run", "--config", &config_arg(&dir)])
        .current_dir(&dir)
        .output()
        .unwrap();
    assert!(
        run.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&run.stderr)
    );

    // And removing it undeclares it again.
    let rm = Command::new(nanny_bin())
        .args(["rules", "remove", "nanny:owasp@2.1.0"])
        .current_dir(&dir)
        .output()
        .unwrap();
    assert!(rm.status.success());
    assert!(!fs::read_to_string(dir.join("nanny.toml"))
        .unwrap()
        .contains("nanny:owasp@2.1.0"));
}

/// A pack whose contents do not match its declared digest is refused.
#[test]
fn rules_add_refuses_a_tampered_pack() {
    let dir = temp_dir();
    fs::write(
        dir.join("nanny.toml"),
        "[start]\ncmd = \"echo hi\"\n\n[tools]\nallowed = []\n",
    )
    .unwrap();

    let src = temp_dir();
    fs::write(src.join("rules.py"), "def r(ctx): return True\n").unwrap();
    fs::write(
        src.join("pack.toml"),
        "name = \"acme:pack\"\nversion = \"1.0.0\"\nsignature = \"0000000000000000000000000000000000000000000000000000000000000000\"\n",
    )
    .unwrap();

    let out = Command::new(nanny_bin())
        .args([
            "rules",
            "add",
            "acme:pack@1.0.0",
            "--from",
            &src.to_string_lossy(),
        ])
        .current_dir(&dir)
        .output()
        .unwrap();

    assert!(!out.status.success(), "a tampered pack must not install");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("failed integrity check"), "got: {stderr}");
    assert!(
        !dir.join(".nanny/rules").exists(),
        "nothing may be written on failure"
    );
}

// ── Fail-closed on a missing rule pack, on both start paths ──────────────────

/// A config declaring a pack that is not on disk, plus an app identity so
/// `--serve` gets far enough to reach the pack check.
fn write_config_declaring_a_missing_pack(dir: &Path) {
    fs::write(
        dir.join("nanny.toml"),
        "[start]\ncmd = \"true\"\n\n[tools]\nallowed = [\"a\"]\n\n\
         [rules]\nextends = [\"nanny:owasp@1.0.0\"]\n",
    )
    .unwrap();
    fs::create_dir_all(dir.join(".nanny")).unwrap();
    fs::write(
        dir.join(".nanny/app.json"),
        "{\"app_id\":\"app_packcheck00000000000000000000\",\"name\":\"packcheck\"}",
    )
    .unwrap();
}

/// Both start paths must refuse, and this is asserted as a pair on purpose.
///
/// The guarantee is that a pack named in `[rules] extends` and missing from
/// disk stops the run: the operator believes controls are in force that are
/// not. It was implemented in `cmd_run` only, so it held for local development
/// and not for `--serve`, which is the shape every container runs: an image
/// missing its vendored pack booted and ran unguarded, silently, because
/// nothing else checks. Testing one path would have passed throughout.
#[test]
fn a_missing_rule_pack_refuses_to_start_on_both_paths() {
    for args in [vec!["run"], vec!["run", "--serve"]] {
        let dir = temp_dir();
        write_config_declaring_a_missing_pack(&dir);

        let out = Command::new(nanny_bin())
            .args(&args)
            .current_dir(&dir)
            .output()
            .expect("nanny runs");

        let stderr = String::from_utf8_lossy(&out.stderr);
        assert!(
            !out.status.success(),
            "`nanny {}` started with a declared pack missing from disk; \
             stdout: {} stderr: {stderr}",
            args.join(" "),
            String::from_utf8_lossy(&out.stdout),
        );
        assert!(
            stderr.contains("nanny:owasp"),
            "`nanny {}` must name the pack it could not find: {stderr}",
            args.join(" "),
        );
    }
}
