// nanny server — governance server daemon commands.
//
// For single-process agents, use `nanny run` instead. This command starts a
// standalone governance server for cross-process or cross-machine enforcement.
//
// Implementation:
//   start  — build BridgeComponents from nanny.toml, call NetworkServer::start_blocking
//   stop   — send SIGTERM to PID in ~/.nanny/server.pid
//   status — TCP-connect to address in ~/.nanny/server.addr and call /health

use anyhow::{Context, Result};
use clap::Subcommand;
use std::net::SocketAddr;
use std::path::PathBuf;

use nanny_bridge::network::NetworkServer;
use nanny_config;
use nanny_core::agent::limits::Limits;

use crate::runtime::build_bridge_components;

use super::certs::default_certs_dir;

// ── Command shape ─────────────────────────────────────────────────────────────

#[derive(Subcommand)]
pub enum ServerCommand {
    /// Start the governance server daemon.
    ///
    /// For single-process agents, use `nanny run` instead. This command starts
    /// a standalone governance server for cross-process or cross-machine
    /// enforcement over TCP with mutual TLS.
    ///
    /// Governance API and HTTP CONNECT proxy (when [proxy] is configured in nanny.toml) share one port.
    /// Default port 62669 spells NANNY on a phone keypad.
    Start {
        /// Listen address. Governance API and proxy share this port.
        #[arg(long, default_value = "0.0.0.0:62669")]
        addr: SocketAddr,

        /// Path to the server certificate PEM.
        /// Defaults to ~/.nanny/certs/server.crt.
        /// Generate with: nanny certs generate
        #[arg(long)]
        cert: Option<PathBuf>,

        /// Path to the server private key PEM.
        /// Defaults to ~/.nanny/certs/server.key.
        #[arg(long)]
        key: Option<PathBuf>,

        /// Path to the CA certificate PEM used to validate client certs.
        /// Defaults to ~/.nanny/certs/ca.crt.
        #[arg(long)]
        ca: Option<PathBuf>,

    },

    /// Stop the running governance server.
    Stop,

    /// Show the live status of the running server.
    ///
    /// Prints: listen address, number of connected agents, current budget state.
    Status,
}

// ── Entry point ───────────────────────────────────────────────────────────────

pub fn cmd_server(action: ServerCommand) -> Result<()> {
    match action {
        ServerCommand::Start { addr, cert, key, ca } =>
            cmd_server_start(addr, cert, key, ca),
        ServerCommand::Stop => cmd_server_stop(),
        ServerCommand::Status => cmd_server_status(),
    }
}

// ── Helpers ───────────────────────────────────────────────────────────────────

/// Path to ~/.nanny — created on demand.
fn nanny_state_dir() -> Result<PathBuf> {
    let dir = dirs::home_dir()
        .context("cannot determine home directory")?
        .join(".nanny");
    std::fs::create_dir_all(&dir).context("failed to create ~/.nanny")?;
    Ok(dir)
}

// ── nanny server start ────────────────────────────────────────────────────────

/// DoS protection: hard-coded 100 req/s per client IP.
/// Not a config knob — if this is ever wrong for a real workload, bump the
/// constant and ship a new binary.  Operator tuning of this value is not a
/// use-case Nanny needs to support.
const RATE_LIMIT_RPS: u32 = 100;

fn cmd_server_start(
    addr: SocketAddr,
    cert: Option<PathBuf>,
    key: Option<PathBuf>,
    ca: Option<PathBuf>,
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
        max_cost_units: config.limits.max_cost_units,
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

    // Verify cert files exist before attempting to bind.
    for (label, path) in [("server cert", &cert_path), ("server key", &key_path), ("CA cert", &ca_path)] {
        if !path.exists() {
            anyhow::bail!(
                "{label} not found: {}\n\
                 \n\
                 Run `nanny certs generate` to create a certificate bundle, or\n\
                 use --cert, --key, --ca to specify paths explicitly.",
                path.display()
            );
        }
    }

    // Write the listen address to ~/.nanny/server.addr so `nanny server status`
    // and `nanny run` can discover the server without config.
    let state_dir = nanny_state_dir()?;
    std::fs::write(state_dir.join("server.addr"), addr.to_string())
        .context("failed to write ~/.nanny/server.addr")?;

    // Blocking — returns only when the server shuts down (CTRL-C / SIGTERM).
    NetworkServer::start_blocking(
        addr,
        cert_path,
        key_path,
        ca_path,
        components,
        proxy_allowed_hosts,
        None,
        RATE_LIMIT_RPS,
    )?;

    Ok(())
}

// ── nanny server stop ─────────────────────────────────────────────────────────

fn cmd_server_stop() -> Result<()> {
    let state_dir = nanny_state_dir()?;
    let pid_file = state_dir.join("server.pid");

    let raw = std::fs::read_to_string(&pid_file).with_context(|| {
        format!(
            "no running server found (PID file not present at {})\n\
             Start the server with: nanny server start",
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
                 Check with: nanny server status"
            );
        }
        println!("nanny server: stopped (PID {pid})");
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
                 Check with: nanny server status"
            );
        }
        println!("nanny server: stopped (PID {pid})");
    }

    Ok(())
}

// ── nanny server status ───────────────────────────────────────────────────────

fn cmd_server_status() -> Result<()> {
    let state_dir = nanny_state_dir()?;
    let addr_file = state_dir.join("server.addr");

    // Read the stored listen address.
    let addr_str = std::fs::read_to_string(&addr_file).with_context(|| {
        format!(
            "no server address found (file not present at {})\n\
             Start the server with: nanny server start",
            addr_file.display()
        )
    })?;
    let addr = addr_str.trim();

    // Try a TCP connection to check reachability.
    match std::net::TcpStream::connect(addr) {
        Ok(_) => {
            println!("nanny server: running");
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
            println!("nanny server: not reachable at {addr}");
            println!("  Start with: nanny server start");
            std::process::exit(1);
        }
    }

    Ok(())
}
