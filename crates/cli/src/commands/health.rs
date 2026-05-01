// nanny health — show the status of all active Nanny components.
//
// Checks three things:
//   1. Local bridge   — is the per-run bridge socket/port accepting connections?
//   2. Network server — is NANNY_BRIDGE_ADDR reachable? (v0.2.0)
//   3. Certs          — do ~/.nanny/certs/ exist and when do they expire?
//
// Exits 0 if every *active* component is healthy.
// Exits 1 if any active component is unhealthy.
//
// "Active" means: the relevant env var or file is present. A component that
// was never started is not checked and does not cause a non-zero exit.

use anyhow::{Context, Result};
use std::path::{Path, PathBuf};
use time::format_description::well_known::Rfc3339;
use time::OffsetDateTime;

use super::certs::{default_certs_dir, read_meta};

// ── Entry point ───────────────────────────────────────────────────────────────

pub fn cmd_health() -> Result<()> {
    let mut all_healthy = true;

    // ── 1. Local bridge ───────────────────────────────────────────────────────
    // The bridge is started by `nanny run` — it injects NANNY_BRIDGE_SOCKET
    // (Unix) or NANNY_BRIDGE_PORT (Windows) into the child process env.
    // Checking health from *within* a governed process makes sense; checking
    // from a separate terminal won't see these vars.
    let bridge_status = check_local_bridge();
    match &bridge_status {
        BridgeStatus::Running => {
            println!("local bridge  : running");
        }
        BridgeStatus::NotRunning => {
            println!("local bridge  : not running");
        }
        BridgeStatus::Unreachable(detail) => {
            println!("local bridge  : unreachable — {detail}");
            all_healthy = false;
        }
    }

    // ── 2. Network server ─────────────────────────────────────────────────────
    // Set by `nanny run` (or manually) when NANNY_BRIDGE_ADDR points at a
    // remote governance server started with `nanny server start`.
    let server_status = check_network_server();
    match &server_status {
        ServerStatus::NotConfigured => {
            println!("network server: not running");
        }
        ServerStatus::Reachable(addr, how) => {
            println!("network server: running  ({addr})  [{how}]");
        }
        ServerStatus::Unreachable(addr, detail) => {
            println!("network server: unreachable  ({addr}) — {detail}");
            all_healthy = false;
        }
    }

    // ── 3. Certs ──────────────────────────────────────────────────────────────
    let cert_dir = default_certs_dir();
    let cert_status = check_certs(&cert_dir);
    match &cert_status {
        CertStatus::NotFound => {
            println!("certs         : not found  (run `nanny certs generate`)");
        }
        CertStatus::Valid { expires } => {
            let formatted = expires.format(&Rfc3339).unwrap_or_else(|_| "?".to_string());
            println!("certs         : valid  (expires {formatted})");

            // Warn 30 days before expiry — still healthy, but worth flagging.
            let days_left = (*expires - OffsetDateTime::now_utc()).whole_days();
            if days_left <= 30 {
                eprintln!(
                    "nanny health  : warning — certs expire in {days_left} day(s). \
                     Run `nanny certs rotate` to renew."
                );
            }
        }
        CertStatus::Expired { expires } => {
            let formatted = expires.format(&Rfc3339).unwrap_or_else(|_| "?".to_string());
            println!("certs         : EXPIRED  (expired {formatted})");
            all_healthy = false;
        }
        CertStatus::Unreadable(detail) => {
            println!("certs         : unreadable — {detail}");
            all_healthy = false;
        }
    }

    if !all_healthy {
        std::process::exit(1);
    }

    Ok(())
}

// ── Local bridge check ────────────────────────────────────────────────────────

enum BridgeStatus {
    Running,
    NotRunning,
    Unreachable(String),
}

fn check_local_bridge() -> BridgeStatus {
    // On Unix: NANNY_BRIDGE_SOCKET points at the Unix domain socket.
    #[cfg(unix)]
    if let Ok(socket_path) = std::env::var("NANNY_BRIDGE_SOCKET") {
        return match std::os::unix::net::UnixStream::connect(&socket_path) {
            Ok(_) => BridgeStatus::Running,
            Err(e) => BridgeStatus::Unreachable(format!("cannot connect to {socket_path}: {e}")),
        };
    }

    // On Windows (and Unix fallback): NANNY_BRIDGE_PORT is a TCP loopback port.
    if let Ok(port_str) = std::env::var("NANNY_BRIDGE_PORT") {
        if let Ok(port) = port_str.parse::<u16>() {
            return match std::net::TcpStream::connect(("127.0.0.1", port)) {
                Ok(_) => BridgeStatus::Running,
                Err(e) => {
                    BridgeStatus::Unreachable(format!("cannot connect to 127.0.0.1:{port}: {e}"))
                }
            };
        }
        return BridgeStatus::Unreachable(format!("invalid NANNY_BRIDGE_PORT: {port_str}"));
    }

    BridgeStatus::NotRunning
}

// ── Network server check ──────────────────────────────────────────────────────

enum ServerStatus {
    NotConfigured,
    /// Server is reachable. `how` describes the verification method used.
    Reachable(String, &'static str),
    Unreachable(String, String),
}

fn check_network_server() -> ServerStatus {
    let addr = match std::env::var("NANNY_BRIDGE_ADDR") {
        Ok(a) => a,
        Err(_) => return ServerStatus::NotConfigured,
    };

    // Prefer a full mTLS health check (actual HTTPS request with client cert)
    // over a raw TCP probe — it validates the TLS handshake, CA trust, and the
    // /health response in one shot.
    let cert_dir    = default_certs_dir();
    let client_cert = cert_dir.join("client.crt");
    let client_key  = cert_dir.join("client.key");
    let ca_cert     = cert_dir.join("ca.crt");

    if client_cert.exists() && client_key.exists() && ca_cert.exists() {
        match mtls_health_check(&addr, &client_cert, &client_key, &ca_cert) {
            MtlsResult::Running => ServerStatus::Reachable(addr, "mTLS ok"),
            MtlsResult::Stopped => ServerStatus::Reachable(addr, "mTLS ok, server stopped"),
            MtlsResult::CertError(detail) => ServerStatus::Unreachable(
                addr,
                format!("cert mismatch — run `nanny certs show`: {detail}"),
            ),
            MtlsResult::ConnectError(detail) => ServerStatus::Unreachable(
                addr,
                format!("connection failed: {detail}"),
            ),
        }
    } else {
        // No local certs — fall back to TCP ping (e.g. loopback dev server).
        tcp_probe_status(addr)
    }
}

// ── mTLS health check ─────────────────────────────────────────────────────────

enum MtlsResult {
    Running,
    Stopped,
    CertError(String),
    ConnectError(String),
}

fn mtls_health_check(
    addr: &str,
    client_cert: &Path,
    client_key: &Path,
    ca: &Path,
) -> MtlsResult {
    match do_mtls_get(addr, client_cert, client_key, ca) {
        Err(e) => {
            let msg = e.to_string();
            // reqwest surfaces TLS/cert errors with these substrings.
            if msg.contains("certificate") || msg.contains("tls") || msg.contains("TLS")
                || msg.contains("handshake") || msg.contains("invalid peer")
            {
                MtlsResult::CertError(msg)
            } else {
                MtlsResult::ConnectError(msg)
            }
        }
        Ok(state) if state.as_deref() == Some("running") => MtlsResult::Running,
        Ok(_) => MtlsResult::Stopped,
    }
}

/// Perform a real HTTPS GET /health using mutual TLS and return the `state` field.
fn do_mtls_get(
    addr: &str,
    client_cert: &Path,
    client_key: &Path,
    ca: &Path,
) -> Result<Option<String>> {
    // reqwest::Identity requires combined cert+key PEM in one buffer.
    let cert_pem = std::fs::read(client_cert)
        .with_context(|| format!("failed to read {}", client_cert.display()))?;
    let key_pem = std::fs::read(client_key)
        .with_context(|| format!("failed to read {}", client_key.display()))?;
    let mut combined = cert_pem;
    combined.push(b'\n');
    combined.extend_from_slice(&key_pem);

    let identity = reqwest::Identity::from_pem(&combined)
        .context("failed to build client identity from cert + key")?;

    let ca_pem = std::fs::read(ca)
        .with_context(|| format!("failed to read {}", ca.display()))?;
    let ca_cert = reqwest::Certificate::from_pem(&ca_pem)
        .context("failed to load CA certificate")?;

    let client = reqwest::blocking::Client::builder()
        .identity(identity)
        .add_root_certificate(ca_cert)
        .timeout(std::time::Duration::from_secs(5))
        .build()
        .context("failed to build HTTP client")?;

    let url   = format!("https://{addr}/health");
    let bytes = client
        .get(&url)
        .send()
        .with_context(|| format!("GET {url} failed"))?
        .bytes()
        .context("failed to read /health response body")?;
    let body: serde_json::Value =
        serde_json::from_slice(&bytes).context("invalid JSON from /health")?;

    Ok(body.get("state").and_then(|v| v.as_str()).map(|s| s.to_string()))
}

// ── TCP-only probe (fallback when no local certs) ─────────────────────────────

fn tcp_probe_status(addr: String) -> ServerStatus {
    let reachable = addr.rsplit_once(':').and_then(|(host, port_str)| {
        port_str
            .parse::<u16>()
            .ok()
            .map(|port| std::net::TcpStream::connect((host, port)).is_ok())
    });

    match reachable {
        Some(true) => ServerStatus::Reachable(addr, "TCP ping"),
        _ => ServerStatus::Unreachable(
            addr,
            "TCP connection refused — is `nanny server start` running?".to_string(),
        ),
    }
}

// ── Cert status check ─────────────────────────────────────────────────────────

enum CertStatus {
    NotFound,
    Valid { expires: OffsetDateTime },
    Expired { expires: OffsetDateTime },
    Unreadable(String),
}

fn check_certs(dir: &PathBuf) -> CertStatus {
    if !dir.join("ca.crt").exists() {
        return CertStatus::NotFound;
    }

    match read_meta(dir) {
        Ok(meta) => match time::OffsetDateTime::parse(&meta.expires, &Rfc3339) {
            Ok(expires) => {
                if expires < OffsetDateTime::now_utc() {
                    CertStatus::Expired { expires }
                } else {
                    CertStatus::Valid { expires }
                }
            }
            Err(e) => CertStatus::Unreadable(format!("cannot parse expiry from meta.json: {e}")),
        },
        Err(_) => {
            // meta.json missing — try to parse server.crt directly.
            let cert_path = dir.join("server.crt");
            if !cert_path.exists() {
                return CertStatus::NotFound;
            }
            match std::fs::read(&cert_path) {
                Ok(pem) => match super::certs::cert_expiry_from_pem_pub(&pem) {
                    Ok(expires) => {
                        if expires < OffsetDateTime::now_utc() {
                            CertStatus::Expired { expires }
                        } else {
                            CertStatus::Valid { expires }
                        }
                    }
                    Err(e) => CertStatus::Unreadable(e.to_string()),
                },
                Err(e) => CertStatus::Unreadable(e.to_string()),
            }
        }
    }
}
