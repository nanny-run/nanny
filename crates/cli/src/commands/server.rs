// Governance server daemon commands (nanny run --serve, nanny stop, nanny status).
//
// For single-process agents, use `nanny run` instead. This command starts a
// standalone governance server for cross-process or cross-machine enforcement.
//
// Implementation:
//   start  — build BridgeComponents from nanny.toml, call NetworkServer::start_blocking
//   stop   — send SIGTERM to PID in ~/.nanny/server.pid
//   status — TCP-connect to address in ~/.nanny/server.addr and call /health

use anyhow::{Context, Result};
use std::net::SocketAddr;
use std::path::PathBuf;

use nanny_bridge::network::NetworkServer;
use nanny_config;
use nanny_core::agent::limits::Limits;

use crate::runtime::build_bridge_components;

use super::certs::default_certs_dir;

// The governance server is `nanny run --serve`; `nanny status` and `nanny stop`
// manage it. This module holds those three entry points (`cmd_server_start`,
// `cmd_server_status`, `cmd_server_stop`) called directly from `main.rs`.

// ── Helpers ───────────────────────────────────────────────────────────────────

/// Path to ~/.nanny — created on demand.
fn nanny_state_dir() -> Result<PathBuf> {
    let dir = dirs::home_dir()
        .context("cannot determine home directory")?
        .join(".nanny");
    std::fs::create_dir_all(&dir).context("failed to create ~/.nanny")?;
    Ok(dir)
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
) -> Result<()> {
    // Load nanny.toml from CWD.
    let toml_path = std::env::current_dir()
        .context("cannot determine current directory")?
        .join("nanny.toml");
    let config = nanny_config::load(&toml_path).map_err(|e| {
        anyhow::anyhow!("failed to load nanny.toml: {e}\n\nRun `nanny init` to create one.")
    })?;

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

    // Write the listen address to ~/.nanny/server.addr so `nanny status`
    // and `nanny run` can discover the server without config.
    let state_dir = nanny_state_dir()?;
    std::fs::write(state_dir.join("server.addr"), addr.to_string())
        .context("failed to write ~/.nanny/server.addr")?;

    // Record whether this server has [proxy] allowed_hosts active, so a
    // joining `nanny run` (possibly in a different directory with its own,
    // irrelevant nanny.toml) knows whether to inject HTTPS_PROXY/HTTP_PROXY —
    // the proxy is configured on the SERVER's config, not the client's.
    std::fs::write(
        state_dir.join("server.proxy"),
        if proxy_allowed_hosts.is_some() { "1" } else { "0" },
    )
    .context("failed to write ~/.nanny/server.proxy")?;

    // Cloud forwarding (auth-free, cli-side): the same gate as `nanny run` —
    // mode = "managed" AND logged in, and not --no-sync. The engine only exposes
    // events; the forwarder that talks to the cloud lives here. `resolve_sync`
    // prints the "managed but not logged in" nudge itself.
    let session_token = uuid::Uuid::new_v4().to_string();
    let credentials = crate::credentials::Credentials::load().ok().flatten();
    let event_sink = match crate::sync::resolve_sync(&config, credentials.as_ref(), no_sync) {
        Some(target) => {
            println!(
                "nanny: syncing fleet events to {} (enforcement stays local)",
                target.endpoint
            );
            let (tx, rx) = std::sync::mpsc::channel();
            crate::sync::ServerForwarder::spawn(rx, target.endpoint, target.api_key, session_token.clone());
            Some(tx)
        }
        None => None,
    };

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
    )?;

    Ok(())
}

// ── nanny stop (governance server stop) ───────────────────────────────────────

pub fn cmd_server_stop() -> Result<()> {
    let state_dir = nanny_state_dir()?;
    let pid_file = state_dir.join("server.pid");

    let raw = std::fs::read_to_string(&pid_file).with_context(|| {
        format!(
            "no running server found (PID file not present at {})\n\
             Start the server with: nanny run --serve",
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
                 Check with: nanny status"
            );
        }
        println!("nanny: governance server stopped (PID {pid})");
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
                 Check with: nanny status"
            );
        }
        println!("nanny: governance server stopped (PID {pid})");
    }

    Ok(())
}

// ── nanny status (governance server status) ───────────────────────────────────

pub fn cmd_server_status() -> Result<()> {
    let state_dir = nanny_state_dir()?;
    let addr_file = state_dir.join("server.addr");

    // Read the stored listen address.
    let addr_str = std::fs::read_to_string(&addr_file).with_context(|| {
        format!(
            "no server address found (file not present at {})\n\
             Start the server with: nanny run --serve",
            addr_file.display()
        )
    })?;
    let addr = addr_str.trim();

    // Try a TCP connection to check reachability.
    match std::net::TcpStream::connect(addr) {
        Ok(_) => {
            println!("nanny: governance server running");
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
        }
        Err(_) => {
            println!("nanny: governance server not reachable at {addr}");
            println!("  Start with: nanny run --serve");
            std::process::exit(1);
        }
    }

    Ok(())
}
