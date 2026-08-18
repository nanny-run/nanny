// Governance server daemon commands (nanny run --serve, nanny stop, nanny status).
//
// For single-process agents, use `nanny run` instead. This command starts a
// standalone governance server for cross-process or cross-machine enforcement.
//
// Implementation:
//   start:  build BridgeComponents from nanny.toml, call NetworkServer::start_blocking
//   stop:   send SIGTERM to PID in ~/.nanny/servers/<app_id>/server.pid
//   status: TCP-connect to address in ~/.nanny/servers/<app_id>/server.addr and call /health
//
// State is keyed by app id, not global: two unrelated apps' governors on one
// machine each get their own subdirectory under ~/.nanny/servers/ and can
// never collide or overwrite each other's state files (the bug this whole
// keying scheme exists to fix).

use anyhow::{Context, Result};
use std::net::SocketAddr;
use std::path::{Path, PathBuf};

use nanny_bridge::network::NetworkServer;
use nanny_config;
use nanny_core::agent::limits::Limits;

use crate::identity::AppIdentity;
use crate::runtime::build_bridge_components;

use super::certs::default_certs_dir;

// The governance server is `nanny run --serve`; `nanny status` and `nanny stop`
// manage it. This module holds those three entry points (`cmd_server_start`,
// `cmd_server_status`, `cmd_server_stop`) called directly from `main.rs`.

// ── Helpers ───────────────────────────────────────────────────────────────────

/// The directory `.nanny/servers/<app_id>` is nested under. `NANNY_HOME`
/// overrides it when set (a real, always-available override, not just for
/// tests); otherwise falls back to the OS home directory. Reading an env var
/// is portable, unlike overriding `HOME` directly: `dirs::home_dir()` ignores
/// `HOME` on Windows, so it's the only override that works identically
/// everywhere `nanny` runs.
fn nanny_home_dir() -> Result<PathBuf> {
    if let Ok(dir) = std::env::var("NANNY_HOME") {
        if !dir.is_empty() {
            return Ok(PathBuf::from(dir));
        }
    }
    dirs::home_dir().context("cannot determine home directory")
}

/// Path to ~/.nanny/servers/<app_id>, created on demand. Public so `main.rs`
/// can resolve the same path for `nanny run --join=<appId>`.
pub fn nanny_server_state_dir(app_id: &str) -> Result<PathBuf> {
    let dir = nanny_home_dir()?.join(".nanny").join("servers").join(app_id);
    std::fs::create_dir_all(&dir)
        .with_context(|| format!("failed to create {}", dir.display()))?;
    Ok(dir)
}

/// Resolve which app's governor a command should act on: the explicit
/// `--app=<appId>` flag if given, else the identity of the app in the current
/// directory. No further fallback, a missing id either way is a loud error,
/// never a silent guess at "the" server.
fn resolve_app_id(explicit: Option<String>) -> Result<String> {
    if let Some(id) = explicit {
        return Ok(id);
    }
    let cwd = std::env::current_dir().context("cannot determine current directory")?;
    Ok(AppIdentity::load_required(&cwd)?.app_id)
}

// ── nanny run --serve (governance server start) ───────────────────────────────

/// DoS protection: hard-coded 100 req/s per client IP.
/// Not a config knob — if this is ever wrong for a real workload, bump the
/// constant and ship a new binary.  Operator tuning of this value is not a
/// use-case Nanny needs to support.
const RATE_LIMIT_RPS: u32 = 100;

pub fn cmd_server_start(
    addr: SocketAddr,
    cert: Option<PathBuf>,
    key: Option<PathBuf>,
    ca: Option<PathBuf>,
    no_sync: bool,
    env: crate::cloud::CloudEnv,
    extra_args: Vec<String>,
) -> Result<()> {
    let cwd = std::env::current_dir().context("cannot determine current directory")?;

    // Load nanny.toml from CWD.
    let toml_path = cwd.join("nanny.toml");
    let config = nanny_config::load(&toml_path).map_err(|e| {
        anyhow::anyhow!("failed to load nanny.toml: {e}\n\nRun `nanny init` to create one.")
    })?;

    // An app identity is required to key this governor's state, without it
    // two unrelated `--serve` instances on one machine would collide again,
    // exactly the bug this keying exists to fix.
    let app = AppIdentity::load_required(&cwd)?;

    // Proxy mode is opt-in.
    // If [proxy] exists but allowed_hosts is empty or omitted, proxy is treated as not configured.

    // Build BridgeComponents from config (no CLI ceiling — server uses config values).
    let limits = Limits {
        max_steps:      config.limits.max_steps,
        max_tokens: config.limits.max_tokens,
        timeout_ms:     config.limits.timeout_ms,
    };
    let components = build_bridge_components(&config, limits, false);

    // Proxy is configured only when allowed_hosts is present and non-empty.
    let proxy_allowed_hosts = config
        .proxy
        .as_ref()
        .and_then(|p| (!p.allowed_hosts.is_empty()).then(|| p.allowed_hosts.clone()));

    // Resolve cert paths: use CLI args, else fall back to ~/.nanny/certs/.
    let certs_dir = default_certs_dir();
    let cert_path = cert.unwrap_or_else(|| certs_dir.join("server.crt"));
    let key_path  = key.unwrap_or_else(|| certs_dir.join("server.key"));
    let ca_path   = ca.unwrap_or_else(|| certs_dir.join("ca.crt"));

    // Cert files are required only for non-loopback addresses (mTLS mandatory).
    // Loopback binds use plain HTTP — OS-enforced, no TLS overhead.
    if !addr.ip().is_loopback() {
        for (label, path) in [("server cert", &cert_path), ("server key", &key_path), ("CA cert", &ca_path)] {
            if !path.exists() {
                anyhow::bail!(
                    "{label} not found: {}\n\
                     \n\
                     Non-loopback addresses require mTLS. Run `nanny certs generate` to create \
                     a certificate bundle, or use --cert, --key, --ca to specify paths explicitly.\n\
                     \n\
                     For same-machine multi-agent use, bind to loopback instead:\n\
                     \n\
                     \x20   nanny run --serve\n\
                     \n\
                     (default is 127.0.0.1:62669 — no certs needed)",
                    path.display()
                );
            }
        }
    }

    // NOTE: server.addr is written by the server itself, not here. The
    // requested port is not necessarily the one it ends up on. An occupied
    // default steps forward, and `--join`/`--app` must discover the real one.
    // Only the code that owns the bound socket knows it.
    let state_dir = nanny_server_state_dir(&app.app_id)?;

    // Record whether this server has [proxy] allowed_hosts active, so a
    // joining `nanny run --join=<appId>` (possibly in a different directory with
    // its own, irrelevant nanny.toml) knows whether to inject
    // HTTPS_PROXY/HTTP_PROXY, the proxy is configured on the SERVER's config,
    // not the client's.
    std::fs::write(
        state_dir.join("server.proxy"),
        if proxy_allowed_hosts.is_some() { "1" } else { "0" },
    )
    .with_context(|| format!("failed to write {}", state_dir.join("server.proxy").display()))?;

    // Cloud forwarding: sync happens exactly when NANNY_API_KEY is set and
    // --no-sync didn't turn it off. The engine only exposes events; the
    // forwarder that talks to the cloud lives here. The status line prints
    // either way, because a governor that silently stops reporting for a whole fleet
    // is the worst version of the failure v0.5.0 shipped.
    let session_token = uuid::Uuid::new_v4().to_string();
    let target = crate::sync::resolve_sync(env, no_sync);
    println!("{}", crate::sync::sync_status_line(target.as_ref().map_err(|e| *e), Some(&app.name)));

    // Record where this governor forwards (or that it doesn't), so `nanny
    // status` can answer "is my fleet actually reporting?" without guessing.
    // A long-lived governor prints its status line once, at startup, possibly
    // weeks ago and possibly into a log nobody kept; the operator asking the
    // question later needs an answer that outlives that line. Never the key,
    // only the host.
    let sync_state = match &target {
        Ok(t) => t.endpoint.strip_suffix("/v1/ingest").unwrap_or(&t.endpoint).to_string(),
        Err(_) => "off".to_string(),
    };
    std::fs::write(state_dir.join("server.sync"), &sync_state)
        .with_context(|| format!("failed to write {}", state_dir.join("server.sync").display()))?;

    let event_sink = target.ok().map(|target| {
        let (tx, rx) = std::sync::mpsc::channel();
        crate::sync::ServerForwarder::spawn(
            rx,
            target.endpoint,
            target.api_key,
            session_token.clone(),
            &cwd,
        );
        tx
    });

    // [observability] applies to --serve exactly the same way it applies to
    // local `nanny run`: the config makes the same promise either way ("here's
    // where your event log goes"), so it must be honored the same way either
    // way. `log = "stdout"` stays a no-op here on purpose: a long-lived
    // server continuously mixing NDJSON events into its own startup/status
    // stdout output would be noisy and wrong, unlike a short-lived local run
    // where that's the whole point. Uses the same resolution logic as local
    // `nanny run` (`ObservabilityConfig::resolve_log_path`), so both paths
    // land on the same `.nanny/logs/<name>` file.
    let local_log_path = config.observability.resolve_log_path(&cwd)?;

    println!("nanny: name ({}), appId ({})", app.name, app.app_id);

    // Does this governor also have an app of its own to run?
    //
    // `[start]` already means "here is the app" everywhere else. Plain
    // `nanny run` requires it, so `--serve` honouring it is the consistent
    // reading, not a new convention. Present: governor plus that app, one
    // command, no launcher script. Absent: headless governor, the shared-
    // governor case where the apps live elsewhere and arrive via `--join`.
    //
    // Either way the governor is a full network server: launching an app of
    // its own never stops other processes or machines joining it.
    let child_command: Option<Vec<String>> = match config.start.as_ref() {
        Some(start) => {
            let mut command = shlex::split(&start.cmd).ok_or_else(|| {
                anyhow::anyhow!(
                    "invalid [start].cmd in nanny.toml: unterminated quote or invalid \
                     shell syntax: {:?}",
                    start.cmd
                )
            })?;
            if command.is_empty() {
                anyhow::bail!("[start].cmd in nanny.toml is empty");
            }
            command.extend(extra_args);
            Some(command)
        }
        None => {
            if !extra_args.is_empty() {
                anyhow::bail!(
                    "trailing arguments were given, but nanny.toml has no [start] section \
                     to append them to.\n\n\
                     Add [start] cmd = \"...\" to run an app under this governor, or drop \
                     the arguments to run it headless."
                );
            }
            None
        }
    };

    let state_dir_for_server = nanny_server_state_dir(&app.app_id)?;

    match child_command {
        // ── Headless governor ────────────────────────────────────────────────
        // Blocking: returns only when the server shuts down (CTRL-C/SIGTERM).
        None => {
            println!("nanny: no [start] in nanny.toml, running headless. Join it with --join");
            NetworkServer::start_blocking_synced(
                addr,
                cert_path,
                key_path,
                ca_path,
                components,
                proxy_allowed_hosts,
                Some(session_token),
                RATE_LIMIT_RPS,
                event_sink,
                state_dir_for_server,
                local_log_path,
            )?;
            Ok(())
        }

        // ── Governor plus its own app ────────────────────────────────────────
        Some(command) => run_governor_with_app(
            GovernorSetup {
                addr,
                cert_path,
                key_path,
                ca_path,
                components,
                proxy_allowed_hosts,
                session_token,
                event_sink,
                state_dir: state_dir_for_server,
                local_log_path,
            },
            command,
            &app.app_id,
        ),
    }
}

// ── nanny stop (governance server stop) ───────────────────────────────────────

pub fn cmd_server_stop(app: Option<String>) -> Result<()> {
    let app_id = resolve_app_id(app)?;
    let state_dir = nanny_server_state_dir(&app_id)?;
    let pid_file = state_dir.join("server.pid");

    let raw = std::fs::read_to_string(&pid_file).with_context(|| {
        format!(
            "no running server found for app '{app_id}' (PID file not present at {})\n\
             Start it with: nanny run --serve  (from that app's directory)",
            pid_file.display()
        )
    })?;

    let pid: u32 = raw.trim().parse().with_context(|| {
        format!("corrupted PID file at {} — expected an integer", pid_file.display())
    })?;

    #[cfg(unix)]
    {
        let status = std::process::Command::new("kill")
            .args(["-TERM", &pid.to_string()])
            .status()
            .context("failed to run `kill`")?;
        if !status.success() {
            anyhow::bail!(
                "failed to stop server (PID {pid}) — it may have already exited.\n\
                 Check with: nanny status --app={app_id}"
            );
        }
        println!("nanny: governance server stopped (PID {pid}, app {app_id})");
    }

    #[cfg(windows)]
    {
        let status = std::process::Command::new("taskkill")
            .args(["/PID", &pid.to_string(), "/F"])
            .status()
            .context("failed to run `taskkill`")?;
        if !status.success() {
            anyhow::bail!(
                "failed to stop server (PID {pid}) — it may have already exited.\n\
                 Check with: nanny status --app={app_id}"
            );
        }
        println!("nanny: governance server stopped (PID {pid}, app {app_id})");
    }

    Ok(())
}

// ── nanny status (governance server status) ───────────────────────────────────

/// Everything `NetworkServer::start_blocking_synced` needs, grouped so it can
/// be handed to a thread in one move.
struct GovernorSetup {
    addr: SocketAddr,
    cert_path: PathBuf,
    key_path: PathBuf,
    ca_path: PathBuf,
    components: nanny_bridge::BridgeComponents,
    proxy_allowed_hosts: Option<Vec<String>>,
    session_token: String,
    event_sink: Option<std::sync::mpsc::Sender<(String, Vec<String>)>>,
    state_dir: PathBuf,
    local_log_path: Option<PathBuf>,
}

/// Run the governance server and, underneath it, the app from `[start]`.
///
/// The governor runs on a background thread and the app on this one. That
/// ordering matters: the app is only spawned once the governor's listener is
/// bound, so there is no readiness race to poll around. That is the gap a launcher
/// script has to paper over with `until nanny status`.
///
/// Being one process also fixes what a two-command shell launcher cannot:
/// `nanny` is PID 1 in a container, so it receives SIGTERM directly and the
/// governor gets its full graceful drain; it owns the child, so the child is
/// reaped rather than orphaned; and if either side dies the other is torn
/// down instead of left running half-governed.
fn run_governor_with_app(setup: GovernorSetup, command: Vec<String>, app_id: &str) -> Result<()> {
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::{Arc, Mutex};

    let state_dir = setup.state_dir.clone();

    // ── User-interrupt handling (Ctrl-C / SIGTERM) ───────────────────────────
    //
    // Without this, a SIGINT from a terminal Ctrl-C has no installed handler
    // anywhere in this process, so the OS's default disposition kills `nanny`
    // outright — before it ever reaches the post-loop cleanup below. That
    // leaves this run's discovery files behind under `state_dir` forever, and
    // the next `nanny run --serve` fails with "has server state but isn't
    // reachable". The governed child (e.g. uvicorn) has its own signal
    // handling and shuts down fine on its own via normal terminal job-control
    // (SIGINT goes to the whole foreground process group); this handler's job
    // is only to make sure *this* process — the governor — also notices the
    // signal, stops the child if the OS hasn't already, and cleans up.
    //
    // Registered here, before the governor thread or the child even exist, so
    // a Ctrl-C during any blocking point in this function (waiting for the
    // governor to bind, waiting on the child) is still caught. `child_pid` is
    // `None` until the child is actually spawned below; the handler no-ops the
    // kill step until then, since there's nothing to kill yet.
    let child_pid: Arc<Mutex<Option<u32>>> = Arc::new(Mutex::new(None));
    {
        let child_pid = child_pid.clone();
        let state_dir = state_dir.clone();
        ctrlc::set_handler(move || {
            if let Some(pid) = *child_pid.lock().unwrap_or_else(|e| e.into_inner()) {
                force_kill_pid(pid);
            }
            remove_discovery_files(&state_dir);
            // A deliberate, immediate exit — this runs on ctrlc's dedicated
            // signal thread, not the thread running the poll loop below, so
            // there is no unwind path back into this function's normal
            // control flow to fall through to.
            std::process::exit(130);
        })
        .context("failed to install SIGINT/SIGTERM handler")?;
    }

    // The governor thread flips this on the way out, whether that was a clean
    // SIGTERM drain or a startup failure. Either way the app must not keep
    // running ungoverned.
    let governor_finished = Arc::new(AtomicBool::new(false));
    let governor_result: Arc<std::sync::Mutex<Option<anyhow::Error>>> =
        Arc::new(std::sync::Mutex::new(None));

    let finished = governor_finished.clone();
    let result_slot = governor_result.clone();
    let governor = std::thread::Builder::new()
        .name("nanny-governor".into())
        .spawn(move || {
            let outcome = NetworkServer::start_blocking_synced(
                setup.addr,
                setup.cert_path,
                setup.key_path,
                setup.ca_path,
                setup.components,
                setup.proxy_allowed_hosts,
                Some(setup.session_token),
                RATE_LIMIT_RPS,
                setup.event_sink,
                setup.state_dir,
                setup.local_log_path,
            );
            if let Err(e) = outcome {
                *result_slot.lock().unwrap_or_else(|e| e.into_inner()) = Some(e);
            }
            finished.store(true, Ordering::SeqCst);
        })
        .context("failed to start the governor thread")?;

    // Wait for the listener before spawning the app. The governor writes
    // server.addr only after a successful bind, so its appearance is the
    // readiness signal, and it carries the port actually bound, which may not
    // be the one requested if the default fell forward.
    let addr_file = state_dir.join("server.addr");
    let server = wait_for_governor(&addr_file, &governor_finished, app_id)?;

    // If the governor died during startup, surface its error rather than a
    // confusing "can't reach the server" from the child.
    if let Some(e) = governor_result.lock().unwrap_or_else(|e| e.into_inner()).take() {
        return Err(e);
    }

    let (mut cmd, run_id) = crate::build_governed_child(command, &server)?;
    crate::declare_app_to_governor(&server, Path::new("."), &run_id);

    println!("nanny: running [start] under this governor");
    println!();

    let mut child = cmd.spawn().with_context(|| {
        format!("failed to spawn '{}'", cmd.get_program().to_string_lossy())
    })?;

    // From here on, a Ctrl-C/SIGTERM lands the signal handler installed above
    // on an actual PID to kill, not a no-op.
    *child_pid.lock().unwrap_or_else(|e| e.into_inner()) = Some(child.id());

    // Poll rather than block on wait(), so a governor shutdown (SIGTERM in a
    // container) takes the app down with it instead of leaving this process
    // hanging on a child nothing is governing any more.
    let status = loop {
        match child.try_wait().context("failed to poll the app process")? {
            Some(status) => break status,
            None => {
                if governor_finished.load(Ordering::SeqCst) {
                    eprintln!("nanny: governor stopped, stopping the app it was governing");
                    let _ = child.kill();
                    let _ = child.wait();
                    break std::process::ExitStatus::default();
                }
                std::thread::sleep(std::time::Duration::from_millis(50));
            }
        }
    };

    // The app is done, so the governor has nothing left to govern. Drop the
    // discovery files first so `nanny status`/`--join` never point at a
    // governor that is on its way out.
    remove_discovery_files(&state_dir);
    drop(governor);

    if !status.success() {
        std::process::exit(status.code().unwrap_or(1));
    }
    Ok(())
}

/// Force-kill a process by PID, mirroring `std::process::Child::kill()`'s
/// semantics (SIGKILL on Unix, terminate on Windows) for the one caller that
/// only has a PID, not a `Child` handle: the SIGINT/SIGTERM handler above runs
/// on ctrlc's own signal thread, which doesn't own the `Child` value the main
/// thread is busy polling in the loop below.
fn force_kill_pid(pid: u32) {
    #[cfg(unix)]
    {
        let _ = std::process::Command::new("kill")
            .args(["-KILL", &pid.to_string()])
            .status();
    }
    #[cfg(windows)]
    {
        let _ = std::process::Command::new("taskkill")
            .args(["/PID", &pid.to_string(), "/F"])
            .status();
    }
}

/// Every discovery file a `nanny run --serve` invocation may have written
/// under `state_dir`, gathered in one place so no cleanup path (normal exit,
/// governor-died early exit, or a Ctrl-C/SIGTERM interrupt) forgets one:
/// `server.pid`, `server.addr`, `server.token`, `server.proxy_token` are
/// written by `NetworkServer` in the bridge crate; `server.proxy` and
/// `server.sync` are written by `cmd_server_start` above. A stale leftover
/// from any of the six is exactly what makes the next `nanny run --serve`
/// report "has server state but isn't reachable".
fn remove_discovery_files(state_dir: &Path) {
    for name in [
        "server.pid",
        "server.addr",
        "server.token",
        "server.proxy_token",
        "server.proxy",
        "server.sync",
    ] {
        let _ = std::fs::remove_file(state_dir.join(name));
    }
}

/// Block until the governor has bound and published its address, or until it
/// gives up. Returns the discovery info the child needs.
fn wait_for_governor(
    addr_file: &Path,
    governor_finished: &std::sync::atomic::AtomicBool,
    app_id: &str,
) -> Result<crate::NetworkServerInfo> {
    use std::sync::atomic::Ordering;

    // Generous: costs nothing on a healthy start (returns as soon as the file
    // appears) and only spends time when something is actually wrong.
    for _ in 0..200 {
        if addr_file.exists() {
            return crate::detect_joined_server(app_id);
        }
        if governor_finished.load(Ordering::SeqCst) {
            anyhow::bail!("the governance server exited before it finished starting");
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
    anyhow::bail!(
        "the governance server did not become ready within 10 s\n\n\
         Nothing was written to {}",
        addr_file.display()
    )
}

pub fn cmd_server_status(app: Option<String>) -> Result<()> {
    let app_id = resolve_app_id(app)?;
    let state_dir = nanny_server_state_dir(&app_id)?;
    let addr_file = state_dir.join("server.addr");

    // Read the stored listen address.
    let addr_str = std::fs::read_to_string(&addr_file).with_context(|| {
        format!(
            "no server address found for app '{app_id}' (file not present at {})\n\
             Start it with: nanny run --serve  (from that app's directory)",
            addr_file.display()
        )
    })?;
    let addr = addr_str.trim();

    // Try a TCP connection to check reachability.
    match std::net::TcpStream::connect(addr) {
        Ok(_) => {
            println!("nanny: governance server running");
            println!("  appId  : {app_id}");
            println!("  address: {addr}");

            // Read PID if available.
            if let Ok(pid) = std::fs::read_to_string(state_dir.join("server.pid")) {
                println!("  pid    : {}", pid.trim());
            }

            // Read token file path.
            let token_file = state_dir.join("server.token");
            if token_file.exists() {
                println!("  token  : (see {})", token_file.display());
            }

            // Whether this governor forwards to Nanny Cloud. Written at start;
            // absent means a pre-v0.6.0 governor that predates the file. Only
            // reached after the TCP connect above succeeded, so a stale file
            // from a dead governor can never be reported as live.
            match std::fs::read_to_string(state_dir.join("server.sync")) {
                Ok(s) if s.trim() == "off" => println!("  sync   : off (enforcing locally)"),
                Ok(s) if !s.trim().is_empty() => println!("  sync   : {}", s.trim()),
                _ => {}
            }
        }
        Err(_) => {
            println!("nanny: governance server not reachable at {addr}");
            println!("  Start with: nanny run --serve");
            std::process::exit(1);
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    // Regression coverage for the bug this whole per-app keying scheme exists
    // to fix: two unrelated apps' governor state must never collide, and the
    // same app id must always resolve to the same directory.

    #[test]
    fn different_app_ids_get_different_state_dirs() {
        let a = nanny_server_state_dir("app_aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa").unwrap();
        let b = nanny_server_state_dir("app_bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb").unwrap();
        assert_ne!(a, b, "two different app ids must never resolve to the same state dir");
    }

    #[test]
    fn same_app_id_is_stable_across_calls() {
        let a1 = nanny_server_state_dir("app_cccccccccccccccccccccccccccccc").unwrap();
        let a2 = nanny_server_state_dir("app_cccccccccccccccccccccccccccccc").unwrap();
        assert_eq!(a1, a2, "the same app id must always resolve to the same state dir");
    }

    #[test]
    fn state_dir_is_scoped_under_servers_by_app_id() {
        let id = "app_dddddddddddddddddddddddddddddd";
        let dir = nanny_server_state_dir(id).unwrap();
        assert!(
            dir.ends_with(format!("servers/{id}")),
            "state dir must be nested under .nanny/servers/<app_id>, got {}",
            dir.display()
        );
    }

    #[test]
    fn resolve_app_id_prefers_explicit_over_cwd() {
        // The explicit --app=<id> flag must win outright, with no fallback to
        // the current directory's own identity, no ambiguity about which
        // governor a command targets when both are available.
        let id = resolve_app_id(Some("app_explicit_wins".to_string())).unwrap();
        assert_eq!(id, "app_explicit_wins");
    }
}
