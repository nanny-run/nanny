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
use std::path::PathBuf;

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

    // Write the listen address to ~/.nanny/servers/<app_id>/server.addr so
    // `nanny status --app=<appId>` and `nanny run --join=<appId>` can discover this
    // exact server without config, and never collide with a different app's
    // governor on the same machine.
    let state_dir = nanny_server_state_dir(&app.app_id)?;
    std::fs::write(state_dir.join("server.addr"), addr.to_string())
        .with_context(|| format!("failed to write {}", state_dir.join("server.addr").display()))?;

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
    // either way — a governor that silently stops reporting for a whole fleet
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
        crate::sync::ServerForwarder::spawn(rx, target.endpoint, target.api_key, session_token.clone());
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

    // Blocking — returns only when the server shuts down (CTRL-C / SIGTERM).
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
        nanny_server_state_dir(&app.app_id)?,
        local_log_path,
    )?;

    Ok(())
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
