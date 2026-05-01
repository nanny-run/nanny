// network.rs — TCP + mTLS governance server for cross-process enforcement.
//
// Started by `nanny server start`. Multiple agents on the same or different
// machines connect to it. All connections share one execution context —
// shared ledger, shared step count, shared tool call history. This is
// cross-process budget enforcement without cloud dependency.
//
// Transport: axum (HTTP routing) + rustls (mTLS, both sides present certs).
// Auth:      session token (X-Nanny-Session-Token header) + mTLS client cert.
// Together:  mTLS ensures only certified clients connect; session token is
//            defense-in-depth and per-execution identity.
//
// Usage from CLI:
//     nanny server start [--addr 0.0.0.0:62669] [--cert ...] [--key ...] [--ca ...]
//
// Agents point to the server via:
//     NANNY_BRIDGE_ADDR=host:port
//     NANNY_SESSION_TOKEN=<token>
//     NANNY_BRIDGE_CERT=~/.nanny/certs/client.crt
//     NANNY_BRIDGE_KEY=~/.nanny/certs/client.key
//     NANNY_BRIDGE_CA=~/.nanny/certs/ca.crt

use std::net::{IpAddr, SocketAddr};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Instant;

use anyhow::{Context, Result};
use axum::{
    body::Bytes,
    extract::{ConnectInfo, Request, State},
    http::StatusCode,
    middleware::{self, Next},
    response::{IntoResponse, Response},
    routing::{get, post},
    Router,
};
use nanny_core::events::event::ExecutionEvent;
use uuid::Uuid;

use nanny_runtime::ToolRegistry;

use super::{
    append_event, now_ms,
    BridgeComponents, BridgeResp, BridgeState, ContentType,
    handle_agent_enter, handle_agent_exit, handle_events, handle_health,
    handle_rule_evaluate, handle_status, handle_step, handle_stop,
    handle_tool_call, init_shared_state, is_stopped,
};

// ── Per-IP rate limiter ───────────────────────────────────────────────────────

/// Sliding-window per-IP rate limiter.  DoS protection only — never a
/// business-tier gate and never in nanny.toml.  Hardcoded safe default;
/// power users override with `--rate-limit` on `nanny server start`.
#[derive(Clone)]
struct RateLimiter {
    inner: Arc<Mutex<std::collections::HashMap<IpAddr, (u32, Instant)>>>,
    rps:   u32,
}

impl RateLimiter {
    fn new(rps: u32) -> Self {
        Self { inner: Arc::new(Mutex::new(std::collections::HashMap::new())), rps }
    }

    /// Returns `true` if the request is within the rate limit.
    fn check(&self, ip: IpAddr) -> bool {
        let mut map = self.inner.lock().unwrap();
        let now = Instant::now();
        let entry = map.entry(ip).or_insert((0u32, now));
        if now.duration_since(entry.1).as_secs() >= 1 {
            // New second window — reset counter.
            *entry = (1u32, now);
            true
        } else if entry.0 < self.rps {
            entry.0 += 1;
            true
        } else {
            false
        }
    }
}

// ── Shared axum state ─────────────────────────────────────────────────────────

#[derive(Clone)]
struct AppState {
    shared: Arc<Mutex<BridgeState>>,
    registry: Arc<ToolRegistry>,
    /// Session token stored separately for fast auth check without locking.
    session_token: String,
    /// Optional proxy allowlist. When present and non-empty, CONNECT requests
    /// are treated as proxy traffic and enforced against this list.
    proxy_allowed_hosts: Option<Vec<String>>,
    /// Per-IP rate limiter — DoS protection.
    rate_limiter: RateLimiter,
}

// ── Token auth middleware ─────────────────────────────────────────────────────

async fn require_token(
    State(app): State<AppState>,
    req: Request,
    next: Next,
) -> Response {
    let ok = req
        .headers()
        .get("x-nanny-session-token")
        .and_then(|v| v.to_str().ok())
        == Some(&app.session_token);

    if !ok {
        return (StatusCode::UNAUTHORIZED, r#"{"error":"Unauthorized"}"#).into_response();
    }
    next.run(req).await
}

// ── Rate-limit middleware ─────────────────────────────────────────────────────

async fn rate_limit_middleware(
    State(app): State<AppState>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    req: Request,
    next: Next,
) -> Response {
    if !app.rate_limiter.check(peer.ip()) {
        return (
            StatusCode::TOO_MANY_REQUESTS,
            r#"{"error":"rate limit exceeded"}"#,
        )
        .into_response();
    }
    next.run(req).await
}

// ── Response conversion ───────────────────────────────────────────────────────

fn to_response(resp: BridgeResp) -> Response {
    let ct = match resp.content_type {
        ContentType::Json   => "application/json",
        ContentType::Ndjson => "application/x-ndjson",
    };
    let status = StatusCode::from_u16(resp.status)
        .unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);
    (status, [(axum::http::header::CONTENT_TYPE, ct)], resp.body).into_response()
}

// ── Route handlers ────────────────────────────────────────────────────────────

async fn route_health(State(app): State<AppState>) -> Response {
    to_response(handle_health(&app.shared))
}

async fn route_status(State(app): State<AppState>) -> Response {
    to_response(handle_status(&app.shared))
}

async fn route_events(State(app): State<AppState>) -> Response {
    to_response(handle_events(&app.shared))
}

async fn route_stop(State(app): State<AppState>, body: Bytes) -> Response {
    to_response(handle_stop(&body, &app.shared))
}

async fn route_tool_call(State(app): State<AppState>, body: Bytes) -> Response {
    // Action endpoints return 410 after execution stops.
    if is_stopped(&app.shared) {
        return (StatusCode::from_u16(410).unwrap(), r#"{"error":"execution stopped"}"#).into_response();
    }
    to_response(handle_tool_call(&body, &app.shared, &app.registry))
}

async fn route_rule_evaluate(State(app): State<AppState>, body: Bytes) -> Response {
    if is_stopped(&app.shared) {
        return (StatusCode::from_u16(410).unwrap(), r#"{"error":"execution stopped"}"#).into_response();
    }
    to_response(handle_rule_evaluate(&body, &app.shared))
}

async fn route_agent_enter(State(app): State<AppState>, body: Bytes) -> Response {
    if is_stopped(&app.shared) {
        return (StatusCode::from_u16(410).unwrap(), r#"{"error":"execution stopped"}"#).into_response();
    }
    to_response(handle_agent_enter(&body, &app.shared))
}

async fn route_agent_exit(State(app): State<AppState>) -> Response {
    to_response(handle_agent_exit(&app.shared))
}

async fn route_step(State(app): State<AppState>) -> Response {
    if is_stopped(&app.shared) {
        return (StatusCode::from_u16(410).unwrap(), r#"{"error":"execution stopped"}"#).into_response();
    }
    to_response(handle_step(&app.shared))
}

// ── IP / host SSRF guard ──────────────────────────────────────────────────────

/// Returns `true` if the host must be blocked regardless of the allowlist.
///
/// Blocks loopback (127.x.x.x / ::1), link-local (169.254.x.x — cloud metadata
/// endpoint), RFC-1918 private ranges, broadcast, and the "localhost" name.
/// Proxying to these would give a compromised agent a path to the host network
/// or cloud metadata APIs. There is intentionally no config escape hatch — the
/// security property is unconditional.
fn is_blocked_host(host: &str) -> bool {
    use std::net::IpAddr;

    // Reject "localhost" by name.
    if host.eq_ignore_ascii_case("localhost") {
        return true;
    }

    // Non-IP hostnames (e.g. "api.openai.com") are not blocked here — the
    // allowlist check handles them.
    let Ok(ip) = host.parse::<IpAddr>() else {
        return false;
    };

    match ip {
        IpAddr::V4(v4) => {
            let [a, b, ..] = v4.octets();
            v4.is_loopback()            // 127.x.x.x
            || (a == 169 && b == 254)   // 169.254.x.x — link-local / cloud metadata
            || v4.is_private()          // 10.x, 172.16–31.x, 192.168.x
            || v4.is_broadcast()        // 255.255.255.255
        }
        IpAddr::V6(v6) => {
            v6.is_loopback()                               // ::1
            || (v6.segments()[0] & 0xffc0) == 0xfe80      // fe80::/10  link-local
            || (v6.segments()[0] & 0xfe00) == 0xfc00      // fc00::/7   unique local
        }
    }
}

// ── Proxy allowlist ───────────────────────────────────────────────────────────

/// Returns `true` if `host` matches any pattern in `patterns`.
///
/// Supported patterns:
/// - Exact hostname: `"api.openai.com"`
/// - Single leading wildcard: `"*.openai.com"` — matches `api.openai.com` but
///   NOT `openai.com` or `evil.openai.com.attacker.com`
fn host_is_allowed(host: &str, patterns: &[String]) -> bool {
    patterns.iter().any(|p| {
        if p == host {
            return true;
        }
        // "*.example.com" matches any single subdomain but not the bare domain.
        // The leading "." in format!(".{suffix}") prevents partial suffix attacks:
        // "evil.openai.com.attacker.com".ends_with(".openai.com") == false ✓
        if let Some(suffix) = p.strip_prefix("*.") {
            return host.ends_with(&format!(".{suffix}"));
        }
        false
    })
}

/// Validate `[proxy] allowed_hosts` entries at server startup.
///
/// Fails loudly on empty strings or unsupported glob patterns so misconfigured
/// servers are caught before they bind a socket.
pub fn validate_allowed_hosts(entries: &[String]) -> Result<()> {
    for entry in entries {
        if entry.is_empty() {
            anyhow::bail!("[proxy] allowed_hosts: entry must not be empty");
        }
        if entry.contains("**") {
            anyhow::bail!(
                "[proxy] allowed_hosts: '**' globs are not supported (got {entry:?})"
            );
        }
        if let Some(suffix) = entry.strip_prefix("*.") {
            if suffix.contains('*') {
                anyhow::bail!(
                    "[proxy] allowed_hosts: only a single leading '*.' is allowed (got {entry:?})"
                );
            }
            if suffix.is_empty() || !suffix.contains('.') {
                anyhow::bail!(
                    "[proxy] allowed_hosts: '*.{suffix}' must include at least one dot \
                     (e.g. '*.openai.com', not '*.com')"
                );
            }
        } else if entry.contains('*') {
            anyhow::bail!(
                "[proxy] allowed_hosts: wildcards are only supported as a leading '*.' \
                 prefix (got {entry:?})"
            );
        }
    }
    Ok(())
}

// ── Proxy (HTTP CONNECT) ──────────────────────────────────────────────────────

async fn route_proxy(State(app): State<AppState>, req: Request) -> Response {
    // Everything that isn't a CONNECT request falls through to this fallback.
    if req.method().as_str() != "CONNECT" {
        return (StatusCode::NOT_FOUND, r#"{"error":"Not Found"}"#).into_response();
    }

    let Some(allowed) = app.proxy_allowed_hosts.as_deref() else {
        return (
            StatusCode::NOT_FOUND,
            r#"{"error":"proxy not configured"}"#,
        )
            .into_response();
    };

    // CONNECT uses authority-form: "host:port".
    let authority = req
        .uri()
        .authority()
        .map(|a| a.as_str().to_string())
        .unwrap_or_default();
    let host = authority.split(':').next().unwrap_or("").to_string();

    if host.is_empty() {
        return (
            StatusCode::BAD_REQUEST,
            r#"{"error":"invalid CONNECT target"}"#,
        )
            .into_response();
    }

    // ── SSRF guard ────────────────────────────────────────────────────────────
    // Loopback, link-local, and RFC-1918 ranges are always blocked, regardless
    // of what is in allowed_hosts. No config escape hatch.
    if is_blocked_host(&host) {
        {
            let mut guard = app.shared.lock().unwrap();
            append_event(&mut guard, ExecutionEvent::ToolDenied {
                ts:   now_ms(),
                tool: format!("http_proxy:{host}"),
            });
        }
        eprintln!("nanny proxy: blocked SSRF attempt to {host}");
        return (
            StatusCode::FORBIDDEN,
            format!(r#"{{"error":"proxy destination blocked","host":"{host}"}}"#),
        )
            .into_response();
    }

    // ── Allowlist check ───────────────────────────────────────────────────────
    if !host_is_allowed(&host, allowed) {
        {
            let mut guard = app.shared.lock().unwrap();
            append_event(&mut guard, ExecutionEvent::ToolDenied {
                ts:   now_ms(),
                tool: format!("http_proxy:{host}"),
            });
        }
        eprintln!("nanny proxy: denied host {host}");
        return (
            StatusCode::FORBIDDEN,
            format!(r#"{{"error":"proxy destination denied","host":"{host}"}}"#),
        )
            .into_response();
    }

    // ── Allowed — emit ToolAllowed before tunneling ───────────────────────────
    {
        let mut guard = app.shared.lock().unwrap();
        append_event(&mut guard, ExecutionEvent::ToolAllowed {
            ts:   now_ms(),
            tool: format!("http_proxy:{host}"),
        });
    }

    // ── Tunnel ────────────────────────────────────────────────────────────────
    // CRITICAL: call hyper::upgrade::on(req) BEFORE returning the response.
    // This removes the `Pending` extension from the request, which signals
    // hyper to keep the connection alive after sending the 200 instead of
    // closing it. Calling `on` inside the spawned task (after the return) is
    // too late — hyper would close the connection first.
    //
    // Flow after 200:
    //   client ──mTLS──► bridge (hyper Upgraded stream)
    //   bridge ──TCP───► target (TcpStream)
    //   tokio::io::copy_bidirectional relays bytes in both directions.
    let on_upgrade = hyper::upgrade::on(req);

    let authority_for_task = authority.clone();
    tokio::task::spawn(async move {
        match on_upgrade.await {
            Ok(upgraded) => {
                let mut upgraded = hyper_util::rt::TokioIo::new(upgraded);
                match tokio::net::TcpStream::connect(&authority_for_task).await {
                    Ok(mut target) => {
                        // Normal I/O errors (client disconnect, timeout) are not
                        // worth logging — they happen on every clean client disconnect.
                        let _ = tokio::io::copy_bidirectional(&mut upgraded, &mut target).await;
                    }
                    Err(e) => {
                        eprintln!("nanny proxy: connect to {authority_for_task} failed: {e}");
                    }
                }
            }
            Err(e) => eprintln!("nanny proxy: upgrade error: {e}"),
        }
    });

    // Return 200 — hyper completes sending the response headers and then hands
    // the raw connection to the spawned task via the upgrade mechanism.
    StatusCode::OK.into_response()
}

// ── Router ────────────────────────────────────────────────────────────────────

fn build_router(app: AppState) -> Router {
    Router::new()
        // Read-only — always available
        .route("/health",        get(route_health))
        .route("/status",        get(route_status))
        .route("/events",        get(route_events))
        // /stop — always accepted (idempotent)
        .route("/stop",          post(route_stop))
        // Action endpoints — return 410 when stopped
        .route("/tool/call",     post(route_tool_call))
        .route("/rule/evaluate", post(route_rule_evaluate))
        .route("/agent/enter",   post(route_agent_enter))
        .route("/agent/exit",    post(route_agent_exit))
        .route("/step",          post(route_step))
        // Token auth — runs before every handler.
        .layer(middleware::from_fn_with_state(app.clone(), require_token))
        // Rate limiting — outermost layer, runs first.
        // Requires into_make_service_with_connect_info for ConnectInfo extraction.
        .layer(middleware::from_fn_with_state(app.clone(), rate_limit_middleware))
        // Fallback — used for CONNECT proxy traffic.
        .fallback(route_proxy)
        .with_state(app)
}

// ── TLS config ────────────────────────────────────────────────────────────────

fn build_tls_config(
    cert_path: &Path,
    key_path: &Path,
    ca_path: &Path,
) -> Result<rustls::ServerConfig> {
    use rustls::pki_types::{CertificateDer, PrivateKeyDer};
    use rustls::server::WebPkiClientVerifier;
    use rustls::RootCertStore;

    // Load CA cert — used to verify client certificates.
    let ca_pem = std::fs::read(ca_path)
        .with_context(|| format!("failed to read CA cert: {}", ca_path.display()))?;
    let ca_certs: Vec<CertificateDer<'static>> = rustls_pemfile::certs(&mut ca_pem.as_ref())
        .collect::<std::result::Result<Vec<_>, _>>()
        .context("failed to parse CA cert PEM")?;

    let mut root_store = RootCertStore::empty();
    for cert in ca_certs {
        root_store.add(cert).context("failed to add CA cert to root store")?;
    }

    // Require client certificate signed by our CA.
    let verifier = WebPkiClientVerifier::builder(Arc::new(root_store))
        .build()
        .map_err(|e| anyhow::anyhow!("failed to build client cert verifier: {e}"))?;

    // Load server certificate chain.
    let cert_pem = std::fs::read(cert_path)
        .with_context(|| format!("failed to read server cert: {}", cert_path.display()))?;
    let certs: Vec<CertificateDer<'static>> = rustls_pemfile::certs(&mut cert_pem.as_ref())
        .collect::<std::result::Result<Vec<_>, _>>()
        .context("failed to parse server cert PEM")?;

    // Load server private key.
    let key_pem = std::fs::read(key_path)
        .with_context(|| format!("failed to read server key: {}", key_path.display()))?;
    let key: PrivateKeyDer<'static> = rustls_pemfile::private_key(&mut key_pem.as_ref())
        .context("failed to parse server key PEM")?
        .ok_or_else(|| anyhow::anyhow!("no private key found in {}", key_path.display()))?;

    rustls::ServerConfig::builder()
        .with_client_cert_verifier(verifier)
        .with_single_cert(certs, key)
        .context("failed to build TLS ServerConfig")
}

// ── Graceful shutdown signal ──────────────────────────────────────────────────

/// Resolves when SIGTERM arrives (Unix) or Ctrl-C is pressed (Windows).
async fn graceful_shutdown_signal() {
    #[cfg(unix)]
    {
        use tokio::signal::unix::{signal, SignalKind};
        let mut sigterm = signal(SignalKind::terminate())
            .expect("failed to install SIGTERM handler");
        sigterm.recv().await;
    }
    #[cfg(not(unix))]
    {
        let _ = tokio::signal::ctrl_c().await;
    }
}

// ── NetworkServer ─────────────────────────────────────────────────────────────

/// A running network governance server.
///
/// Call `start_blocking` to start the server. It blocks until CTRL-C or
/// `nanny server stop` sends SIGTERM.
pub struct NetworkServer;

impl NetworkServer {
    /// Start the mTLS governance server and block until shutdown.
    ///
    /// `session_token`: if `Some`, use that token; if `None`, generate a fresh UUID.
    /// The token is printed to stdout and written to `~/.nanny/server.token` so
    /// `nanny run` can auto-inject it into child environments.
    pub fn start_blocking(
        addr: SocketAddr,
        cert_path: PathBuf,
        key_path: PathBuf,
        ca_path: PathBuf,
        components: BridgeComponents,
        proxy_allowed_hosts: Option<Vec<String>>,
        session_token: Option<String>,
        rate_limit_rps: u32,  // max req/s per client IP — DoS protection, default 100
    ) -> Result<()> {
        // Install ring crypto provider — safe to call multiple times.
        let _ = rustls::crypto::ring::default_provider().install_default();

        // Validate proxy allowlist entries before binding — fail loudly at startup.
        if let Some(ref hosts) = proxy_allowed_hosts {
            validate_allowed_hosts(hosts)?;
        }

        let token = session_token.unwrap_or_else(|| Uuid::new_v4().to_string());
        let (shared, registry) = init_shared_state(components, token.clone());

        let tls_config = build_tls_config(&cert_path, &key_path, &ca_path)?;
        let app = AppState {
            shared,
            registry,
            session_token: token.clone(),
            proxy_allowed_hosts,
            rate_limiter: RateLimiter::new(rate_limit_rps),
        };

        // Write token to ~/.nanny/server.token for auto-injection by `nanny run`.
        let nanny_dir = dirs::home_dir()
            .context("cannot find home directory")?
            .join(".nanny");
        std::fs::create_dir_all(&nanny_dir)
            .context("failed to create ~/.nanny")?;

        let token_file = nanny_dir.join("server.token");
        std::fs::write(&token_file, &token)
            .context("failed to write ~/.nanny/server.token")?;

        // Restrict token file to owner-read-only. The token is a shared secret —
        // other users on the same machine must not be able to read it.
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = std::fs::set_permissions(&token_file, std::fs::Permissions::from_mode(0o600));
        }

        // PID file so `nanny server stop` can send SIGTERM.
        let pid_file = nanny_dir.join("server.pid");
        std::fs::write(&pid_file, std::process::id().to_string())
            .context("failed to write ~/.nanny/server.pid")?;

        println!("nanny server: started");
        println!("  address      : {addr}");
        println!("  session token: {token}");
        println!("  token file   : {}", token_file.display());
        println!();
        println!("Agents on this machine:");
        println!("  export NANNY_BRIDGE_ADDR={addr}");
        println!("  export NANNY_SESSION_TOKEN={token}");
        println!();
        println!("Cross-machine agents: also set NANNY_BRIDGE_CERT, NANNY_BRIDGE_KEY, NANNY_BRIDGE_CA");
        println!("  (auto-injected by `nanny run` on this machine from ~/.nanny/certs/)");
        println!();
        println!("Press CTRL-C to stop.");

        let rt = tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()
            .context("failed to build tokio runtime")?;

        let result = rt.block_on(async {
            let rustls_config =
                axum_server::tls_rustls::RustlsConfig::from_config(Arc::new(tls_config));

            // ── Graceful SIGTERM drain ────────────────────────────────────────
            // `nanny server stop` sends SIGTERM (Unix) / taskkill (Windows).
            // We give in-flight requests 10 s to complete before forcing exit.
            let server_handle = axum_server::Handle::new();
            {
                let drain = server_handle.clone();
                tokio::spawn(async move {
                    graceful_shutdown_signal().await;
                    eprintln!(
                        "nanny server: shutdown signal received — \
                         draining connections (10 s grace)…"
                    );
                    drain.graceful_shutdown(Some(std::time::Duration::from_secs(10)));
                });
            }

            // ── Cert hot-reload ───────────────────────────────────────────────
            // Watch the directory containing the server cert. When any file
            // changes (from `nanny certs rotate` or `nanny certs import`), rebuild
            // the TLS config and swap it in without restarting the server.
            // New connections use the new cert; in-flight connections finish on
            // the old one. If the new cert files fail to parse we log the error
            // and keep the old config — the server never goes down on a bad write.
            if let Some(cert_dir) = cert_path.parent().map(|p| p.to_path_buf()) {
                use notify::{RecommendedWatcher, RecursiveMode, Watcher};
                let (tx, rx) = std::sync::mpsc::channel();
                match RecommendedWatcher::new(tx, notify::Config::default()) {
                    Ok(mut watcher) => {
                        if watcher.watch(&cert_dir, RecursiveMode::NonRecursive).is_ok() {
                            // Leak the watcher — it must stay alive for the
                            // lifetime of the process to keep delivering events.
                            std::mem::forget(watcher);

                            let rc  = rustls_config.clone();
                            let cp  = cert_path.clone();
                            let kp  = key_path.clone();
                            let cap = ca_path.clone();

                            std::thread::spawn(move || {
                                while rx.recv().is_ok() {
                                    // Drain burst events — a single rotate/import
                                    // writes multiple files and fires many events.
                                    while rx.try_recv().is_ok() {}
                                    // Brief settle delay so all files are flushed
                                    // to disk before we re-read them.
                                    std::thread::sleep(std::time::Duration::from_millis(150));

                                    match build_tls_config(&cp, &kp, &cap) {
                                        Ok(new_cfg) => {
                                            rc.reload_from_config(Arc::new(new_cfg));
                                            eprintln!("nanny server: certs hot-reloaded");
                                        }
                                        Err(e) => {
                                            eprintln!(
                                                "nanny server: cert reload failed — \
                                                 keeping current certs: {e:#}"
                                            );
                                        }
                                    }
                                }
                            });
                        }
                    }
                    Err(e) => {
                        eprintln!(
                            "nanny server: cert watcher failed to start \
                             (hot-reload disabled): {e}"
                        );
                    }
                }
            }

            axum_server::bind_rustls(addr, rustls_config)
                .handle(server_handle)
                .serve(
                    build_router(app)
                        .into_make_service_with_connect_info::<SocketAddr>(),
                )
                .await
                .context("server error")
        });

        // Clean up PID and token files on shutdown.
        let _ = std::fs::remove_file(&pid_file);
        let _ = std::fs::remove_file(&token_file);

        result
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use nanny_core::agent::limits::Limits;
    use nanny_runtime::ToolRegistry;
    use std::collections::HashMap;
    use std::io::Write as _;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::time::Duration;

    // ── Day 6/7 unit tests ────────────────────────────────────────────────────

    // host_is_allowed ─────────────────────────────────────────────────────────

    #[test]
    fn exact_match_is_allowed() {
        assert!(host_is_allowed("api.openai.com", &["api.openai.com".into()]));
    }

    #[test]
    fn exact_match_different_host_denied() {
        assert!(!host_is_allowed("evil.com", &["api.openai.com".into()]));
    }

    #[test]
    fn glob_matches_subdomain() {
        assert!(host_is_allowed("api.openai.com", &["*.openai.com".into()]));
        assert!(host_is_allowed("x.openai.com",   &["*.openai.com".into()]));
    }

    #[test]
    fn glob_does_not_match_bare_domain() {
        // "*.openai.com" must NOT match "openai.com" itself
        assert!(!host_is_allowed("openai.com", &["*.openai.com".into()]));
    }

    #[test]
    fn glob_does_not_match_suffix_attack() {
        // "evil.openai.com.attacker.com" must NOT match "*.openai.com"
        assert!(!host_is_allowed(
            "evil.openai.com.attacker.com",
            &["*.openai.com".into()]
        ));
    }

    #[test]
    fn empty_allowlist_denies_everything() {
        assert!(!host_is_allowed("api.openai.com", &[]));
    }

    #[test]
    fn multiple_patterns_any_match_allows() {
        let patterns = vec!["*.openai.com".into(), "api.groq.com".into()];
        assert!(host_is_allowed("chat.openai.com", &patterns));
        assert!(host_is_allowed("api.groq.com",    &patterns));
        assert!(!host_is_allowed("evil.com",        &patterns));
    }

    // is_blocked_host ─────────────────────────────────────────────────────────

    #[test]
    fn localhost_name_is_blocked() {
        assert!(is_blocked_host("localhost"));
        assert!(is_blocked_host("LOCALHOST"));
    }

    #[test]
    fn loopback_ipv4_is_blocked() {
        assert!(is_blocked_host("127.0.0.1"));
        assert!(is_blocked_host("127.0.0.2"));
        assert!(is_blocked_host("127.255.255.255"));
    }

    #[test]
    fn link_local_metadata_ip_is_blocked() {
        // AWS/GCP/Azure cloud metadata endpoint — primary SSRF target
        assert!(is_blocked_host("169.254.169.254"));
        assert!(is_blocked_host("169.254.0.1"));
    }

    #[test]
    fn rfc1918_ranges_are_blocked() {
        assert!(is_blocked_host("10.0.0.1"));
        assert!(is_blocked_host("10.255.255.255"));
        assert!(is_blocked_host("172.16.0.1"));
        assert!(is_blocked_host("172.31.255.255"));
        assert!(is_blocked_host("192.168.0.1"));
        assert!(is_blocked_host("192.168.255.255"));
    }

    #[test]
    fn loopback_ipv6_is_blocked() {
        assert!(is_blocked_host("::1"));
    }

    #[test]
    fn link_local_ipv6_is_blocked() {
        assert!(is_blocked_host("fe80::1"));
    }

    #[test]
    fn public_ips_are_not_blocked() {
        assert!(!is_blocked_host("8.8.8.8"));
        assert!(!is_blocked_host("1.1.1.1"));
        assert!(!is_blocked_host("2606:4700:4700::1111")); // Cloudflare IPv6
    }

    #[test]
    fn public_hostnames_are_not_blocked() {
        assert!(!is_blocked_host("api.openai.com"));
        assert!(!is_blocked_host("api.groq.com"));
    }

    // validate_allowed_hosts ──────────────────────────────────────────────────

    #[test]
    fn valid_entries_pass_validation() {
        let entries = vec![
            "api.openai.com".into(),
            "*.openai.com".into(),
            "api.groq.com".into(),
        ];
        assert!(validate_allowed_hosts(&entries).is_ok());
    }

    #[test]
    fn empty_entry_fails_validation() {
        assert!(validate_allowed_hosts(&["".into()]).is_err());
    }

    #[test]
    fn double_star_glob_fails_validation() {
        assert!(validate_allowed_hosts(&["**.openai.com".into()]).is_err());
    }

    #[test]
    fn wildcard_in_middle_fails_validation() {
        assert!(validate_allowed_hosts(&["api.*.com".into()]).is_err());
    }

    #[test]
    fn glob_with_no_dot_fails_validation() {
        // "*.com" is too broad (TLD-level wildcard)
        assert!(validate_allowed_hosts(&["*.com".into()]).is_err());
    }

    #[test]
    fn valid_glob_passes_validation() {
        assert!(validate_allowed_hosts(&["*.openai.com".into()]).is_ok());
    }

    // ── Test fixtures ─────────────────────────────────────────────────────────

    fn test_components() -> BridgeComponents {
        BridgeComponents {
            registry: ToolRegistry::new(),
            limits: Limits { max_steps: 100, max_cost_units: 1000, timeout_ms: 30_000 },
            named_limits: HashMap::new(),
            allowed_tools: vec!["echo".to_string()],
            per_tool_max_calls: HashMap::new(),
        }
    }

    /// Pick a unique port for each test so parallel tests don't collide.
    fn next_port() -> u16 {
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let n = COUNTER.fetch_add(1, Ordering::SeqCst);
        // Start at 15200; each test uses the next port
        15200u16 + (n as u16 % 200)
    }

    fn test_certs_dir() -> PathBuf {
        use std::sync::atomic::AtomicU64;
        static CNT: AtomicU64 = AtomicU64::new(0);
        let id = CNT.fetch_add(1, Ordering::SeqCst);
        let dir = std::env::temp_dir()
            .join(format!("nanny-net-test-{}-{}", std::process::id(), id));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    // ── Tests ─────────────────────────────────────────────────────────────────

    #[test]
    fn raw_tcp_tls_handshake_works() {
        // Diagnostic: verifies that a real TCP mTLS handshake works WITHOUT
        // axum-server — pure tokio-rustls. If this passes but the axum-server
        // tests fail, axum-server is causing the issue.
        use rustls::pki_types::{CertificateDer, PrivateKeyDer, ServerName};

        let _ = rustls::crypto::ring::default_provider().install_default();

        let dir = test_certs_dir();
        gen_certs_for_test(&dir);

        let port = next_port();
        let addr_str = format!("127.0.0.1:{port}");

        // ── Build server TLS config (pure rustls, ring explicit) ──────────────
        let ca_pem    = std::fs::read(dir.join("ca.crt")).unwrap();
        let srv_pem   = std::fs::read(dir.join("server.crt")).unwrap();
        let srv_key   = std::fs::read(dir.join("server.key")).unwrap();
        let cli_pem   = std::fs::read(dir.join("client.crt")).unwrap();
        let cli_key   = std::fs::read(dir.join("client.key")).unwrap();

        let provider = Arc::new(rustls::crypto::ring::default_provider());

        // Build server config
        let ca_der: Vec<CertificateDer<'static>> =
            rustls_pemfile::certs(&mut ca_pem.as_ref()).collect::<Result<Vec<_>, _>>().unwrap();
        let mut srv_root = rustls::RootCertStore::empty();
        for c in ca_der.clone() { srv_root.add(c).unwrap(); }

        let srv_verifier = rustls::server::WebPkiClientVerifier::builder_with_provider(
            Arc::new(srv_root), provider.clone()
        ).build().unwrap();

        let srv_certs: Vec<CertificateDer<'static>> =
            rustls_pemfile::certs(&mut srv_pem.as_ref()).collect::<Result<Vec<_>, _>>().unwrap();
        let srv_private: PrivateKeyDer<'static> =
            rustls_pemfile::private_key(&mut srv_key.as_ref()).unwrap().unwrap();

        let mut server_config = rustls::ServerConfig::builder_with_provider(provider.clone())
            .with_safe_default_protocol_versions().unwrap()
            .with_client_cert_verifier(srv_verifier)
            .with_single_cert(srv_certs, srv_private).unwrap();
        server_config.alpn_protocols = vec![b"http/1.1".to_vec()];

        // ── Build client TLS config ────────────────────────────────────────────
        let mut cli_root = rustls::RootCertStore::empty();
        for c in ca_der { cli_root.add(c).unwrap(); }

        let cli_certs: Vec<CertificateDer<'static>> =
            rustls_pemfile::certs(&mut cli_pem.as_ref()).collect::<Result<Vec<_>, _>>().unwrap();
        let cli_private: PrivateKeyDer<'static> =
            rustls_pemfile::private_key(&mut cli_key.as_ref()).unwrap().unwrap();

        let client_config = rustls::ClientConfig::builder_with_provider(provider)
            .with_safe_default_protocol_versions().unwrap()
            .with_root_certificates(cli_root)
            .with_client_auth_cert(cli_certs, cli_private).unwrap();

        // ── Start a simple TCP+TLS echo server in background ──────────────────
        let srv_cfg = Arc::new(server_config);
        let addr_clone = addr_str.clone();
        std::thread::spawn(move || {
            let rt = tokio::runtime::Builder::new_current_thread()
                .enable_all().build().unwrap();
            rt.block_on(async move {
                use tokio::net::TcpListener;
                let listener = TcpListener::bind(&addr_clone).await.unwrap();
                let acceptor = tokio_rustls::TlsAcceptor::from(srv_cfg);
                if let Ok((stream, _)) = listener.accept().await {
                    // Accept and immediately close — we just want the handshake
                    let _ = acceptor.accept(stream).await;
                }
            });
        });
        std::thread::sleep(Duration::from_millis(200));

        // ── Connect as client ──────────────────────────────────────────────────
        let tcp = std::net::TcpStream::connect(&addr_str).unwrap();
        let tls_cfg = Arc::new(client_config);
        let server_name = ServerName::try_from("localhost").unwrap().to_owned();
        let conn = rustls::ClientConnection::new(tls_cfg, server_name).unwrap();
        let mut stream = rustls::StreamOwned::new(conn, tcp);

        // Write something to trigger the handshake
        let result = stream.write_all(b"GET / HTTP/1.1\r\n\r\n");
        assert!(result.is_ok(), "raw tokio-rustls mTLS handshake must succeed: {result:?}");

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn cert_chain_validates_locally() {
        // Diagnostic: verify rcgen generates a cert that rustls can validate
        // without any network involvement. If this fails, the bug is in cert
        // generation; if this passes but TLS tests fail, the bug is in server setup.
        let _ = rustls::crypto::ring::default_provider().install_default();
        let dir = test_certs_dir();
        gen_certs_for_test(&dir);

        let ca_pem  = std::fs::read(dir.join("ca.crt")).unwrap();
        let srv_pem = std::fs::read(dir.join("server.crt")).unwrap();
        let key_pem = std::fs::read(dir.join("server.key")).unwrap();

        use rustls::pki_types::{CertificateDer, PrivateKeyDer};
        let ca_der: Vec<CertificateDer<'static>> =
            rustls_pemfile::certs(&mut ca_pem.as_ref()).collect::<Result<Vec<_>, _>>().unwrap();
        let srv_der: Vec<CertificateDer<'static>> =
            rustls_pemfile::certs(&mut srv_pem.as_ref()).collect::<Result<Vec<_>, _>>().unwrap();
        let key: PrivateKeyDer<'static> =
            rustls_pemfile::private_key(&mut key_pem.as_ref()).unwrap().unwrap();

        assert!(!ca_der.is_empty(), "CA cert must parse");
        assert!(!srv_der.is_empty(), "server cert must parse");

        let mut root_store = rustls::RootCertStore::empty();
        for cert in ca_der { root_store.add(cert).unwrap(); }

        // Build a ServerConfig — proves cert+key are a valid pair.
        let server_cfg = rustls::ServerConfig::builder()
            .with_no_client_auth()
            .with_single_cert(srv_der.clone(), key)
            .expect("server cert+key must form a valid pair");

        // Build a ClientConfig that trusts the CA — proves the CA cert is usable.
        let client_cfg = rustls::ClientConfig::builder()
            .with_root_certificates(root_store)
            .with_no_client_auth();

        // Verify the server cert is trusted by the client's CA store.
        // We do this by doing a TLS handshake in memory using rustls directly.
        use rustls::pki_types::ServerName;
        let server_name = ServerName::try_from("localhost").unwrap().to_owned();
        let mut client_conn = rustls::ClientConnection::new(
            Arc::new(client_cfg), server_name
        ).unwrap();
        let mut server_conn = rustls::ServerConnection::new(Arc::new(server_cfg)).unwrap();

        // Run the handshake in memory.
        let mut handshake_done = false;
        for _ in 0..20 {
            if !client_conn.wants_write() && !server_conn.wants_write()
                && !client_conn.is_handshaking() && !server_conn.is_handshaking() {
                handshake_done = true;
                break;
            }
            let mut buf = Vec::new();
            if client_conn.wants_write() {
                client_conn.write_tls(&mut buf).unwrap();
                server_conn.read_tls(&mut std::io::Cursor::new(&buf)).unwrap();
                server_conn.process_new_packets().unwrap();
            }
            let mut buf = Vec::new();
            if server_conn.wants_write() {
                server_conn.write_tls(&mut buf).unwrap();
                client_conn.read_tls(&mut std::io::Cursor::new(&buf)).unwrap();
                client_conn.process_new_packets().unwrap();
            }
        }
        assert!(handshake_done, "TLS handshake must complete in memory");
    }

    #[test]
    fn server_health_responds_with_running() {
        let dir = test_certs_dir();
        // Use rcgen to generate test certs directly in network tests
        gen_certs_for_test(&dir);

        let port = next_port();
        let addr: SocketAddr = format!("127.0.0.1:{port}").parse().unwrap();
        let cert = dir.join("server.crt");
        let key = dir.join("server.key");
        let ca = dir.join("ca.crt");
        let client_cert = dir.join("client.crt");
        let client_key = dir.join("client.key");
        let token = "test-server-token-health".to_string();

        // Start server in background thread
        let cert2 = cert.clone();
        let key2 = key.clone();
        let ca2 = ca.clone();
        let token2 = token.clone();
        std::thread::spawn(move || {
            NetworkServer::start_blocking(addr, cert2, key2, ca2, test_components(), None, Some(token2), 100)
                .ok();
        });

        // Wait for the server to bind (poll instead of fixed sleep).
        wait_for_port(port);

        // Connect with valid client cert
        let ca_pem = std::fs::read(&ca).unwrap();
        let ca_cert = reqwest::Certificate::from_pem(&ca_pem).unwrap();
        let cert_pem = std::fs::read(&client_cert).unwrap();
        let key_pem = std::fs::read(&client_key).unwrap();
        let identity = reqwest::Identity::from_pem(&[cert_pem, key_pem].concat()).unwrap();

        let client = reqwest::blocking::Client::builder()
            .add_root_certificate(ca_cert)
            .identity(identity)
            .use_rustls_tls()                      // Identity::from_pem = rustls identity
            .danger_accept_invalid_hostnames(true) // test certs use "localhost"
            .timeout(std::time::Duration::from_secs(5))
            .build()
            .unwrap();

        let resp = client
            .get(format!("https://127.0.0.1:{port}/health"))
            .header("X-Nanny-Session-Token", &token)
            .send()
            .expect("health request must succeed");

        assert_eq!(resp.status(), 200);
        let body: serde_json::Value = resp.json().unwrap();
        assert_eq!(body["state"], "running");

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn missing_token_returns_401() {
        let dir = test_certs_dir();
        gen_certs_for_test(&dir);

        let port = next_port();
        let addr: SocketAddr = format!("127.0.0.1:{port}").parse().unwrap();
        let token = "test-server-token-401".to_string();

        let cert = dir.join("server.crt");
        let key = dir.join("server.key");
        let ca = dir.join("ca.crt");
        let client_cert = dir.join("client.crt");
        let client_key = dir.join("client.key");

        let cert2 = cert.clone();
        let key2 = key.clone();
        let ca2 = ca.clone();
        let tok2 = token.clone();
        std::thread::spawn(move || {
            NetworkServer::start_blocking(addr, cert2, key2, ca2, test_components(), None, Some(tok2), 100).ok();
        });
        wait_for_port(port);

        let ca_pem = std::fs::read(&ca).unwrap();
        let ca_cert = reqwest::Certificate::from_pem(&ca_pem).unwrap();
        let cert_pem = std::fs::read(&client_cert).unwrap();
        let key_pem = std::fs::read(&client_key).unwrap();
        let identity = reqwest::Identity::from_pem(&[cert_pem, key_pem].concat()).unwrap();

        let client = reqwest::blocking::Client::builder()
            .add_root_certificate(ca_cert)
            .identity(identity)
            .use_rustls_tls()
            .danger_accept_invalid_hostnames(true)
            .timeout(std::time::Duration::from_secs(5))
            .build()
            .unwrap();

        let resp = client
            .get(format!("https://127.0.0.1:{port}/health"))
            // No token header → 401
            .send()
            .expect("request must complete");

        assert_eq!(resp.status(), 401);

        std::fs::remove_dir_all(&dir).ok();
    }

    // ── Proxy integration tests ───────────────────────────────────────────────
    //
    // These tests send raw CONNECT requests over a direct TLS connection so we
    // can control exactly what goes into the CONNECT line and the
    // X-Nanny-Session-Token header — reqwest's proxy API abstracts too much.

    /// Build a blocking rustls client stream to the test server.
    /// Connects to `server_addr`, presents the client cert, and verifies the
    /// server cert against the CA. Returns the TLS-wrapped stream ready for raw
    /// HTTP bytes. We use "localhost" as the SNI because the test certs have
    /// "localhost" as a SAN.
    fn tls_connect_raw(
        server_addr: &str,
        ca_pem: &[u8],
        client_cert_pem: &[u8],
        client_key_pem: &[u8],
    ) -> impl std::io::Read + std::io::Write {
        use rustls::pki_types::{CertificateDer, PrivateKeyDer, ServerName};

        let ca_certs: Vec<CertificateDer<'static>> =
            rustls_pemfile::certs(&mut ca_pem.as_ref())
                .collect::<std::result::Result<Vec<_>, _>>()
                .unwrap();
        let mut root_store = rustls::RootCertStore::empty();
        for cert in ca_certs {
            root_store.add(cert).unwrap();
        }

        let client_certs: Vec<CertificateDer<'static>> =
            rustls_pemfile::certs(&mut client_cert_pem.as_ref())
                .collect::<std::result::Result<Vec<_>, _>>()
                .unwrap();
        let client_key: PrivateKeyDer<'static> =
            rustls_pemfile::private_key(&mut client_key_pem.as_ref())
                .unwrap()
                .unwrap();

        let tls_config = Arc::new(
            rustls::ClientConfig::builder()
                .with_root_certificates(root_store)
                .with_client_auth_cert(client_certs, client_key)
                .unwrap(),
        );
        let server_name = ServerName::try_from("localhost").unwrap().to_owned();
        let tcp = std::net::TcpStream::connect(server_addr).unwrap();
        // Timeout prevents hanging forever if the server is down or stale.
        tcp.set_read_timeout(Some(std::time::Duration::from_secs(5))).unwrap();
        tcp.set_write_timeout(Some(std::time::Duration::from_secs(5))).unwrap();
        let conn = rustls::ClientConnection::new(tls_config, server_name).unwrap();
        rustls::StreamOwned::new(conn, tcp)
    }

    /// Send a raw CONNECT request and return (status_code, response_body).
    ///
    /// Uses `read` (not `read_exact`) for the header loop so that an unexpected
    /// EOF from the peer does not panic. This matters for the allowed-CONNECT
    /// case: the tunnel task spawned by the handler tries to reach the target
    /// (e.g. api.openai.com) which is unreachable in tests, so it drops the
    /// upgraded connection without TLS close_notify. The 200 response is already
    /// in the OS receive buffer before that happens — we will have read it on a
    /// prior `read` call and broken out of the loop. The subsequent EOF arrives
    /// on the *next* read, which we handle by simply stopping.
    fn send_connect<S: std::io::Read + std::io::Write>(
        stream: &mut S,
        target: &str,
        token: &str,
    ) -> (u16, String) {
        let req = format!(
            "CONNECT {target} HTTP/1.1\r\nHost: {target}\r\nX-Nanny-Session-Token: {token}\r\n\r\n"
        );
        stream.write_all(req.as_bytes()).unwrap();

        // Read until the end-of-headers marker (\r\n\r\n) is found, or EOF/error.
        // A single read() call frequently delivers the entire response (headers +
        // body) in one chunk, so body bytes may already be present in `raw` past
        // the \r\n\r\n boundary.
        let mut raw: Vec<u8> = Vec::new();
        let mut buf = [0u8; 1024];
        loop {
            match stream.read(&mut buf) {
                Ok(0) => break, // clean EOF
                Ok(n) => {
                    raw.extend_from_slice(&buf[..n]);
                    if raw.windows(4).any(|w| w == b"\r\n\r\n") {
                        break; // end-of-headers found
                    }
                }
                Err(_) => break, // UnexpectedEof (no TLS close_notify) or I/O error
            }
            if raw.len() > 8192 {
                break;
            }
        }

        // Split at the end-of-headers marker.
        let header_end = raw
            .windows(4)
            .position(|w| w == b"\r\n\r\n")
            .map(|p| p + 4)
            .unwrap_or(raw.len());

        let header_bytes = &raw[..header_end];
        // Body bytes that arrived in the same TCP segment as the last header bytes.
        let mut body_bytes: Vec<u8> = raw[header_end..].to_vec();

        let header_str = String::from_utf8_lossy(header_bytes);

        let status = header_str
            .lines()
            .next()
            .and_then(|l| l.split_whitespace().nth(1))
            .and_then(|s| s.parse().ok())
            .unwrap_or(0u16);

        // 200 = tunnel established — no body, connection is upgraded. Return early.
        // For all other status codes, collect the body.
        if status != 200 {
            let content_length: Option<usize> = header_str.lines().find_map(|l| {
                let (k, v) = l.split_once(':')?;
                if k.trim().eq_ignore_ascii_case("content-length") {
                    v.trim().parse().ok()
                } else {
                    None
                }
            });

            if let Some(cl) = content_length {
                // Only read bytes not already buffered.
                let remaining = cl.saturating_sub(body_bytes.len());
                if remaining > 0 {
                    let start = body_bytes.len();
                    body_bytes.resize(start + remaining, 0);
                    let _ = stream.read_exact(&mut body_bytes[start..]);
                }
                body_bytes.truncate(cl);
            } else {
                // No Content-Length: read until EOF / timeout (short error body).
                loop {
                    match stream.read(&mut buf) {
                        Ok(0) | Err(_) => break,
                        Ok(n) => body_bytes.extend_from_slice(&buf[..n]),
                    }
                    if body_bytes.len() > 65536 {
                        break;
                    }
                }
            }
        }

        (status, String::from_utf8_lossy(&body_bytes).to_string())
    }

    /// Poll TCP connect until the port is accepting connections (up to 3 s).
    /// Replaces fixed sleep(350ms): under heavy parallel test load, the fixed
    /// sleep is not enough.  Polling is both faster on a quiet machine and
    /// robust on a loaded one.
    fn wait_for_port(port: u16) {
        for _ in 0..60 {
            if std::net::TcpStream::connect(format!("127.0.0.1:{port}")).is_ok() {
                return;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        panic!("server on port {port} never became ready within 3 s");
    }

    /// Start a proxy-enabled network server in a background thread.
    /// Returns (port, session_token, cert_dir).
    fn start_proxy_server(proxy_config: Option<Vec<String>>) -> (u16, String, PathBuf) {
        let dir = test_certs_dir();
        gen_certs_for_test(&dir);

        let port = next_port();
        let addr: SocketAddr = format!("127.0.0.1:{port}").parse().unwrap();
        let token = format!("proxy-test-token-{port}");

        let cert = dir.join("server.crt");
        let key  = dir.join("server.key");
        let ca   = dir.join("ca.crt");
        let tok2 = token.clone();

        std::thread::spawn(move || {
            NetworkServer::start_blocking(
                addr, cert, key, ca, test_components(), proxy_config, Some(tok2), 100,
            )
            .ok();
        });
        wait_for_port(port);
        (port, token, dir)
    }

    #[test]
    fn proxy_not_configured_returns_404() {
        // No proxy_allowed_hosts — CONNECT should return 404.
        let (port, token, dir) = start_proxy_server(None);

        let ca   = std::fs::read(dir.join("ca.crt")).unwrap();
        let cert = std::fs::read(dir.join("client.crt")).unwrap();
        let key  = std::fs::read(dir.join("client.key")).unwrap();
        let mut stream = tls_connect_raw(&format!("127.0.0.1:{port}"), &ca, &cert, &key);
        let (status, _) = send_connect(&mut stream, "api.openai.com:443", &token);
        assert_eq!(status, 404, "CONNECT without proxy config must return 404");
    }

    #[test]
    fn proxy_denies_non_allowlisted_host() {
        let (port, token, dir) = start_proxy_server(Some(vec!["api.openai.com".into()]));

        let ca   = std::fs::read(dir.join("ca.crt")).unwrap();
        let cert = std::fs::read(dir.join("client.crt")).unwrap();
        let key  = std::fs::read(dir.join("client.key")).unwrap();
        let mut stream = tls_connect_raw(&format!("127.0.0.1:{port}"), &ca, &cert, &key);
        let (status, body) = send_connect(&mut stream, "evil.com:443", &token);
        assert_eq!(status, 403, "CONNECT to non-allowlisted host must return 403");
        assert!(body.contains("denied"), "body must indicate the reason");
    }

    #[test]
    fn proxy_blocks_ssrf_loopback() {
        let (port, token, dir) =
            start_proxy_server(Some(vec!["127.0.0.1".into(), "localhost".into()]));

        let ca   = std::fs::read(dir.join("ca.crt")).unwrap();
        let cert = std::fs::read(dir.join("client.crt")).unwrap();
        let key  = std::fs::read(dir.join("client.key")).unwrap();
        let mut stream = tls_connect_raw(&format!("127.0.0.1:{port}"), &ca, &cert, &key);
        // Loopback must be blocked even when explicitly listed in allowed_hosts
        let (status, body) = send_connect(&mut stream, "127.0.0.1:80", &token);
        assert_eq!(status, 403, "loopback must be blocked regardless of allowlist");
        assert!(body.contains("blocked"), "body must say 'blocked', not 'denied'");
    }

    #[test]
    fn proxy_blocks_ssrf_cloud_metadata() {
        let (port, token, dir) =
            start_proxy_server(Some(vec!["169.254.169.254".into()]));

        let ca   = std::fs::read(dir.join("ca.crt")).unwrap();
        let cert = std::fs::read(dir.join("client.crt")).unwrap();
        let key  = std::fs::read(dir.join("client.key")).unwrap();
        let mut stream = tls_connect_raw(&format!("127.0.0.1:{port}"), &ca, &cert, &key);
        let (status, body) = send_connect(&mut stream, "169.254.169.254:80", &token);
        assert_eq!(status, 403, "cloud metadata IP must be blocked regardless of allowlist");
        assert!(body.contains("blocked"), "body must say 'blocked'");
    }

    #[test]
    fn proxy_denial_emits_tool_denied_event() {
        let (port, token, dir) = start_proxy_server(Some(vec!["api.openai.com".into()]));

        let ca   = std::fs::read(dir.join("ca.crt")).unwrap();
        let cert = std::fs::read(dir.join("client.crt")).unwrap();
        let key  = std::fs::read(dir.join("client.key")).unwrap();
        let mut stream = tls_connect_raw(&format!("127.0.0.1:{port}"), &ca, &cert, &key);
        let (status, _) = send_connect(&mut stream, "evil.com:443", &token);
        assert_eq!(status, 403);

        // Check the ToolDenied event appeared in /events via a reqwest call
        // (reuse the existing mTLS client from the existing test helpers).
        std::thread::sleep(Duration::from_millis(50)); // let event flush

        let ca_pem   = std::fs::read(dir.join("ca.crt")).unwrap();
        let cert_pem = std::fs::read(dir.join("client.crt")).unwrap();
        let key_pem  = std::fs::read(dir.join("client.key")).unwrap();
        let ca_cert  = reqwest::Certificate::from_pem(&ca_pem).unwrap();
        let identity = reqwest::Identity::from_pem(&[cert_pem, key_pem].concat()).unwrap();

        let client = reqwest::blocking::Client::builder()
            .add_root_certificate(ca_cert)
            .identity(identity)
            .use_rustls_tls()
            .danger_accept_invalid_hostnames(true)
            .timeout(std::time::Duration::from_secs(5))
            .build()
            .unwrap();

        let resp = client
            .get(format!("https://127.0.0.1:{port}/events"))
            .header("X-Nanny-Session-Token", &token)
            .send()
            .unwrap();

        let body = resp.text().unwrap();
        let has_tool_denied = body.lines().any(|l| {
            serde_json::from_str::<serde_json::Value>(l)
                .map(|v| v["event"] == "ToolDenied")
                .unwrap_or(false)
        });
        assert!(has_tool_denied, "ToolDenied event must appear after proxy denial\ngot: {body}");
    }

    #[test]
    fn proxy_allowed_host_emits_tool_allowed_event() {
        // "api.openai.com" is on the allowlist. We verify ToolAllowed fires.
        //
        // The ToolAllowed event is written before the hyper upgrade attempt, so
        // it appears in /events even if the tunnel fails (which it always does in
        // tests since api.openai.com is unreachable on loopback).
        //
        // Strategy: send the CONNECT in a background thread (so the main thread
        // is free to poll /events).  The background thread calls send_connect
        // which does a blocking read — this drives the TLS send of the request
        // bytes AND waits for the server's response.  The main thread waits for
        // the server to process the event, then checks /events.
        let (port, token, dir) = start_proxy_server(Some(vec!["api.openai.com".into()]));

        let ca   = std::fs::read(dir.join("ca.crt")).unwrap();
        let cert = std::fs::read(dir.join("client.crt")).unwrap();
        let key  = std::fs::read(dir.join("client.key")).unwrap();

        // Send CONNECT in a background thread — don't assert the status here
        // because the upgrade attempt (api.openai.com TCP connect) always fails
        // in tests.  We only care that the server received the request and emitted
        // the ToolAllowed event.
        {
            let ca2    = ca.clone();
            let cert2  = cert.clone();
            let key2   = key.clone();
            let token2 = token.clone();
            std::thread::spawn(move || {
                let mut stream =
                    tls_connect_raw(&format!("127.0.0.1:{port}"), &ca2, &cert2, &key2);
                // send_connect does write + blocking read; the read drives the
                // TLS flush so the server receives the request before we return.
                let _status = send_connect(&mut stream, "api.openai.com:443", &token2);
                // Status may be 0 (upgrade canceled) or 200 — either is fine for
                // this test.  We discard the value.
            });
        }

        // Give the background thread time to send the request and the server
        // time to process it and write the event.
        std::thread::sleep(Duration::from_millis(250));

        let ca_pem   = std::fs::read(dir.join("ca.crt")).unwrap();
        let cert_pem = std::fs::read(dir.join("client.crt")).unwrap();
        let key_pem  = std::fs::read(dir.join("client.key")).unwrap();
        let ca_cert  = reqwest::Certificate::from_pem(&ca_pem).unwrap();
        let identity = reqwest::Identity::from_pem(&[cert_pem, key_pem].concat()).unwrap();

        let client = reqwest::blocking::Client::builder()
            .add_root_certificate(ca_cert)
            .identity(identity)
            .use_rustls_tls()
            .danger_accept_invalid_hostnames(true)
            .timeout(std::time::Duration::from_secs(5))
            .build()
            .unwrap();

        let resp = client
            .get(format!("https://127.0.0.1:{port}/events"))
            .header("X-Nanny-Session-Token", &token)
            .send()
            .unwrap();

        let body = resp.text().unwrap();
        let has_tool_allowed = body.lines().any(|l| {
            serde_json::from_str::<serde_json::Value>(l)
                .map(|v| v["event"] == "ToolAllowed")
                .unwrap_or(false)
        });
        assert!(has_tool_allowed, "ToolAllowed event must appear after allowed CONNECT\ngot: {body}");
    }

    #[test]
    fn no_client_cert_is_rejected_at_tls() {
        let dir = test_certs_dir();
        gen_certs_for_test(&dir);

        let port = next_port();
        let addr: SocketAddr = format!("127.0.0.1:{port}").parse().unwrap();
        let token = "test-server-token-nocert".to_string();

        let cert = dir.join("server.crt");
        let key = dir.join("server.key");
        let ca = dir.join("ca.crt");

        let cert2 = cert.clone();
        let key2 = key.clone();
        let ca2 = ca.clone();
        let tok2 = token.clone();
        std::thread::spawn(move || {
            NetworkServer::start_blocking(addr, cert2, key2, ca2, test_components(), None, Some(tok2), 100).ok();
        });
        wait_for_port(port);

        // Connect WITHOUT a client cert — TLS handshake must fail
        let ca_pem = std::fs::read(&ca).unwrap();
        let ca_cert = reqwest::Certificate::from_pem(&ca_pem).unwrap();

        let client = reqwest::blocking::Client::builder()
            .add_root_certificate(ca_cert)
            .use_rustls_tls()
            .danger_accept_invalid_hostnames(true)
            .timeout(std::time::Duration::from_secs(5))
            // No .identity(...) — no client cert
            .build()
            .unwrap();

        let result = client
            .get(format!("https://127.0.0.1:{port}/health"))
            .header("X-Nanny-Session-Token", &token)
            .send();

        // Must fail — server requires client cert
        assert!(result.is_err(), "connection without client cert must be rejected");

        std::fs::remove_dir_all(&dir).ok();
    }

    // ── Shared helpers (Day 8 + Day 9) ───────────────────────────────────────

    /// Build a blocking mTLS reqwest client that trusts the test CA and
    /// presents the test client cert.
    fn make_mtls_client(dir: &PathBuf, _port: u16) -> reqwest::blocking::Client {
        let ca_pem   = std::fs::read(dir.join("ca.crt")).unwrap();
        let ca_cert  = reqwest::Certificate::from_pem(&ca_pem).unwrap();
        let cert_pem = std::fs::read(dir.join("client.crt")).unwrap();
        let key_pem  = std::fs::read(dir.join("client.key")).unwrap();
        let identity = reqwest::Identity::from_pem(&[cert_pem, key_pem].concat()).unwrap();
        reqwest::blocking::Client::builder()
            .add_root_certificate(ca_cert)
            .identity(identity)
            .use_rustls_tls()
            .danger_accept_invalid_hostnames(true)
            .timeout(Duration::from_secs(5))
            .build()
            .unwrap()
    }

    /// Start a server with full control over components, rps limit, and a
    /// returned `axum_server::Handle` so tests can trigger graceful shutdown.
    fn start_server_with_handle(
        components: BridgeComponents,
        port: u16,
        token: String,
        dir: &PathBuf,
        rps: u32,
    ) -> axum_server::Handle {
        let addr: SocketAddr = format!("127.0.0.1:{port}").parse().unwrap();
        let cert = dir.join("server.crt");
        let key  = dir.join("server.key");
        let ca   = dir.join("ca.crt");

        let handle      = axum_server::Handle::new();
        let handle_inner = handle.clone();
        let tok         = token.clone();

        std::thread::spawn(move || {
            let _ = rustls::crypto::ring::default_provider().install_default();
            let tls_config = build_tls_config(&cert, &key, &ca).unwrap();
            let (shared, registry) = init_shared_state(components, tok.clone());
            let app = AppState {
                shared,
                registry,
                session_token: tok,
                proxy_allowed_hosts: None,
                rate_limiter: RateLimiter::new(rps),
            };
            let rt = tokio::runtime::Builder::new_multi_thread()
                .enable_all()
                .build()
                .unwrap();
            let _ = rt.block_on(async move {
                let rc = axum_server::tls_rustls::RustlsConfig::from_config(
                    Arc::new(tls_config),
                );
                axum_server::bind_rustls(addr, rc)
                    .handle(handle_inner)
                    .serve(
                        build_router(app)
                            .into_make_service_with_connect_info::<SocketAddr>(),
                    )
                    .await
            });
        });
        wait_for_port(port);
        handle
    }

    /// BridgeComponents with a custom max_cost_units ceiling.
    fn test_components_with_cost(max_cost: u64) -> BridgeComponents {
        BridgeComponents {
            registry:          ToolRegistry::new(),
            limits:            Limits { max_steps: 100, max_cost_units: max_cost, timeout_ms: 30_000 },
            named_limits:      HashMap::new(),
            allowed_tools:     vec!["http_get".to_string()],
            per_tool_max_calls: HashMap::new(),
        }
    }

    /// BridgeComponents with a "researcher" named limit for enter/exit tests.
    fn test_components_with_named_limit() -> BridgeComponents {
        let researcher = Limits { max_steps: 50, max_cost_units: 500, timeout_ms: 60_000 };
        let mut named  = HashMap::new();
        named.insert("researcher".to_string(), researcher);
        BridgeComponents {
            registry:          ToolRegistry::new(),
            limits:            Limits { max_steps: 100, max_cost_units: 1000, timeout_ms: 30_000 },
            named_limits:      named,
            allowed_tools:     vec!["http_get".to_string()],
            per_tool_max_calls: HashMap::new(),
        }
    }

    // ── Day 8 tests ───────────────────────────────────────────────────────────

    #[test]
    fn rate_limit_fires_after_n_requests() {
        // Server allows 5 req/s per IP. Sending 7 rapid requests must produce
        // at least one 429 Too Many Requests.
        let dir = test_certs_dir();
        gen_certs_for_test(&dir);
        let port  = next_port();
        let token = format!("rl-{port}");

        let _h = start_server_with_handle(test_components(), port, token.clone(), &dir, 5);

        let client = make_mtls_client(&dir, port);
        let base   = format!("https://127.0.0.1:{port}");

        let mut saw_429 = false;
        for _ in 0..7 {
            let resp = client
                .get(format!("{base}/health"))
                .header("X-Nanny-Session-Token", &token)
                .send()
                .expect("request must complete");
            if resp.status() == 429 {
                saw_429 = true;
                break;
            }
        }
        assert!(saw_429, "rate limiter must fire within 7 rapid requests (limit 5 req/s)");

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn graceful_drain_stops_server_cleanly() {
        // Start server → confirm it responds → trigger graceful shutdown (2 s
        // drain) → wait for drain to finish → confirm new connections are refused.
        let dir = test_certs_dir();
        gen_certs_for_test(&dir);
        let port  = next_port();
        let token = format!("drain-{port}");

        let handle = start_server_with_handle(test_components(), port, token.clone(), &dir, 100);

        let client = make_mtls_client(&dir, port);
        let base   = format!("https://127.0.0.1:{port}");

        // Server must respond normally before drain.
        let pre = client
            .get(format!("{base}/health"))
            .header("X-Nanny-Session-Token", &token)
            .send()
            .expect("health must succeed before drain");
        assert_eq!(pre.status(), 200);

        // Trigger graceful shutdown with a 2 s drain window.
        handle.graceful_shutdown(Some(Duration::from_secs(2)));

        // Wait for the drain window plus a small buffer.
        std::thread::sleep(Duration::from_secs(3));

        // New connections must be refused (server is gone).
        let post = client
            .get(format!("{base}/health"))
            .header("X-Nanny-Session-Token", &token)
            .send();
        assert!(post.is_err(), "server must refuse connections after graceful shutdown");

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn execution_stopped_returns_410_in_network_server() {
        // POST /stop → subsequent action endpoints must return 410 Gone.
        let dir = test_certs_dir();
        gen_certs_for_test(&dir);
        let port  = next_port();
        let token = format!("stop410-{port}");

        let _h = start_server_with_handle(test_components(), port, token.clone(), &dir, 100);

        let client = make_mtls_client(&dir, port);
        let base   = format!("https://127.0.0.1:{port}");

        // Stop the execution.
        let stop = client
            .post(format!("{base}/stop"))
            .header("X-Nanny-Session-Token", &token)
            .body("{}")
            .send()
            .expect("stop must reach server");
        assert_eq!(stop.status(), 200);

        // Action endpoints must return 410.
        for path in &["/tool/call", "/step", "/agent/enter", "/rule/evaluate"] {
            let resp = client
                .post(format!("{base}{path}"))
                .header("X-Nanny-Session-Token", &token)
                .body("{}")
                .send()
                .expect("post-stop request must complete");
            assert_eq!(
                resp.status(), 410,
                "{path} must return 410 after execution stopped"
            );
        }

        std::fs::remove_dir_all(&dir).ok();
    }

    // ── Day 9 tests: shared budget + agent scope + cross-client enforcement ──

    #[test]
    fn shared_budget_across_clients() {
        // Two independent mTLS clients connect to ONE server (shared budget,
        // max_cost = 25).
        //
        // Call 1 (client 1, cost 10) → allowed (total 10)
        // Call 2 (client 2, cost 10) → allowed (total 20, still below 25)
        // Call 3 (client 1, cost 10) → LimitsPolicy pre-check: 20+10=30 > 25
        //                               → denied BudgetExhausted (returns 200 JSON "denied")
        // Call 4 (client 2)          → 410 (execution stopped after call 3 denial)
        //
        // Note: using max_cost=25 (not 20) is intentional.  With max_cost=20,
        // call 2 triggers the post-check boundary (20 >= 20 → mark_stopped, but
        // returns "allowed").  Call 3 then receives 410 instead of a "denied" JSON
        // body.  max_cost=25 ensures call 3 hits the LimitsPolicy pre-check
        // (30 > 25) which returns a proper denial response.
        let dir = test_certs_dir();
        gen_certs_for_test(&dir);
        let port  = next_port();
        let token = format!("budget-{port}");

        let _h = start_server_with_handle(
            test_components_with_cost(25),
            port,
            token.clone(),
            &dir,
            100,
        );

        let c1   = make_mtls_client(&dir, port);
        let c2   = make_mtls_client(&dir, port);
        let base = format!("https://127.0.0.1:{port}");

        macro_rules! tool_call {
            ($client:expr) => {
                $client
                    .post(format!("{}/tool/call", base))
                    .header("X-Nanny-Session-Token", &token)
                    .body(r#"{"tool":"http_get","cost":10}"#)
                    .send()
                    .expect("tool/call must reach server")
            };
        }

        // Call 1 — client 1 allowed
        let r1: serde_json::Value = tool_call!(c1).json().unwrap();
        assert_eq!(r1["status"], "allowed", "call 1 must be allowed");

        // Call 2 — client 2 allowed (shared ledger still within limit)
        let r2: serde_json::Value = tool_call!(c2).json().unwrap();
        assert_eq!(r2["status"], "allowed", "call 2 must be allowed");

        // Call 3 — client 1 denied (budget exhausted)
        let r3: serde_json::Value = tool_call!(c1).json().unwrap();
        assert_eq!(r3["status"], "denied",        "call 3 must be denied");
        assert_eq!(r3["reason"], "BudgetExhausted", "stop reason must be BudgetExhausted");

        // Call 4 — client 2 gets 410 (execution already stopped)
        let r4 = tool_call!(c2);
        assert_eq!(r4.status(), 410, "client 2 must get 410 after execution stopped");

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn agent_enter_exit_events_over_network() {
        // /agent/enter + /agent/exit round-trip over the network server.
        // Uses test_components_with_named_limit() so "researcher" is a valid scope.
        let dir = test_certs_dir();
        gen_certs_for_test(&dir);
        let port  = next_port();
        let token = format!("agentev-{port}");

        let _h = start_server_with_handle(
            test_components_with_named_limit(),
            port,
            token.clone(),
            &dir,
            100,
        );

        let client = make_mtls_client(&dir, port);
        let base   = format!("https://127.0.0.1:{port}");

        // Enter "researcher" scope.
        let enter = client
            .post(format!("{base}/agent/enter"))
            .header("X-Nanny-Session-Token", &token)
            .body(r#"{"name":"researcher"}"#)
            .send()
            .expect("agent/enter must reach server");
        assert_eq!(enter.status(), 200, "known scope must return 200");
        let eb: serde_json::Value = enter.json().unwrap();
        assert_eq!(eb["status"], "ok");

        // Exit scope.
        let exit = client
            .post(format!("{base}/agent/exit"))
            .header("X-Nanny-Session-Token", &token)
            .body("{}")
            .send()
            .expect("agent/exit must reach server");
        assert_eq!(exit.status(), 200);
        let xb: serde_json::Value = exit.json().unwrap();
        assert_eq!(xb["status"], "ok");

        // Events must contain AgentScopeEntered and AgentScopeExited.
        std::thread::sleep(Duration::from_millis(50));
        let events_text = client
            .get(format!("{base}/events"))
            .header("X-Nanny-Session-Token", &token)
            .send()
            .expect("events endpoint must respond")
            .text()
            .unwrap();

        let has_entered = events_text.lines().any(|l| {
            serde_json::from_str::<serde_json::Value>(l)
                .map(|v| v["event"] == "AgentScopeEntered")
                .unwrap_or(false)
        });
        let has_exited = events_text.lines().any(|l| {
            serde_json::from_str::<serde_json::Value>(l)
                .map(|v| v["event"] == "AgentScopeExited")
                .unwrap_or(false)
        });

        assert!(has_entered, "AgentScopeEntered must appear in event log\ngot: {events_text}");
        assert!(has_exited,  "AgentScopeExited must appear in event log\ngot: {events_text}");

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn unknown_agent_scope_returns_404_over_network() {
        // /agent/enter with an unknown name → 404 (not in named_limits).
        let (port, token, dir) = start_proxy_server(None);

        let client = make_mtls_client(&dir, port);
        let base   = format!("https://127.0.0.1:{port}");

        let resp = client
            .post(format!("{base}/agent/enter"))
            .header("X-Nanny-Session-Token", &token)
            .body(r#"{"name":"ghost"}"#)
            .send()
            .expect("request must complete");

        assert_eq!(resp.status(), 404, "unknown scope must return 404");

        std::fs::remove_dir_all(&dir).ok();
    }

    // ── Day 10 tests: security ────────────────────────────────────────────────

    #[test]
    fn client_cert_signed_by_wrong_ca_is_refused() {
        // mTLS defense: a client cert issued by a DIFFERENT CA must be rejected
        // at the TLS handshake — not at the session-token layer.
        //
        // Setup:
        //   dir_a → CA-A, server cert + client cert signed by CA-A
        //   dir_b → CA-B (independent), client cert signed by CA-B
        //
        // Server uses CA-A as the trusted CA for client cert verification.
        // The attacker builds a reqwest client with:
        //   - server CA root = CA-A (so they can verify the server cert)
        //   - client identity = cert/key from dir_b (signed by CA-B)
        //
        // The TLS handshake must fail because CA-A will not accept the CA-B cert.
        let dir_a = test_certs_dir();
        gen_certs_for_test(&dir_a);
        let dir_b = test_certs_dir(); // independent CA
        gen_certs_for_test(&dir_b);

        let port  = next_port();
        let addr: SocketAddr = format!("127.0.0.1:{port}").parse().unwrap();
        let token = format!("wrong-ca-{port}");

        // Start server with CA-A certs.
        let cert  = dir_a.join("server.crt");
        let key   = dir_a.join("server.key");
        let ca    = dir_a.join("ca.crt");
        let tok2  = token.clone();
        std::thread::spawn(move || {
            NetworkServer::start_blocking(addr, cert, key, ca, test_components(), None, Some(tok2), 100).ok();
        });
        wait_for_port(port);

        // Build a client that trusts CA-A but presents a cert signed by CA-B.
        let ca_pem_a    = std::fs::read(dir_a.join("ca.crt")).unwrap();
        let ca_cert_a   = reqwest::Certificate::from_pem(&ca_pem_a).unwrap();
        let cert_pem_b  = std::fs::read(dir_b.join("client.crt")).unwrap();
        let key_pem_b   = std::fs::read(dir_b.join("client.key")).unwrap();
        let bad_identity = reqwest::Identity::from_pem(&[cert_pem_b, key_pem_b].concat()).unwrap();

        let bad_client = reqwest::blocking::Client::builder()
            .add_root_certificate(ca_cert_a)
            .identity(bad_identity)
            .use_rustls_tls()
            .danger_accept_invalid_hostnames(true)
            .timeout(std::time::Duration::from_secs(5))
            .build()
            .unwrap();

        let result = bad_client
            .get(format!("https://127.0.0.1:{port}/health"))
            .header("X-Nanny-Session-Token", &token)
            .send();

        // TLS handshake must fail — server rejects the CA-B client cert.
        assert!(
            result.is_err(),
            "client cert from wrong CA must be rejected at TLS handshake"
        );

        std::fs::remove_dir_all(&dir_a).ok();
        std::fs::remove_dir_all(&dir_b).ok();
    }

    #[test]
    fn valid_cert_wrong_token_returns_401() {
        // Defense in depth: even with a valid mTLS client cert, a wrong or
        // missing session token must return 401 — not 200.
        //
        // This verifies that the token check is independent of mTLS: passing
        // the TLS layer does not bypass the session-token gate.
        let dir   = test_certs_dir();
        gen_certs_for_test(&dir);
        let port  = next_port();
        let addr: SocketAddr = format!("127.0.0.1:{port}").parse().unwrap();
        let correct_token = format!("correct-{port}");

        let cert = dir.join("server.crt");
        let key  = dir.join("server.key");
        let ca   = dir.join("ca.crt");
        let tok2 = correct_token.clone();
        std::thread::spawn(move || {
            NetworkServer::start_blocking(addr, cert, key, ca, test_components(), None, Some(tok2), 100).ok();
        });
        wait_for_port(port);

        // Valid client cert — TLS succeeds.
        let client = make_mtls_client(&dir, port);

        // Wrong token → must get 401 (token check fires after TLS).
        let resp = client
            .get(format!("https://127.0.0.1:{port}/health"))
            .header("X-Nanny-Session-Token", "not-the-right-token")
            .send()
            .expect("request must reach server (TLS succeeds)");

        assert_eq!(
            resp.status(), 401,
            "valid cert + wrong token must return 401"
        );

        std::fs::remove_dir_all(&dir).ok();
    }
}

// ── Test cert generator (used by network tests) ───────────────────────────────

/// Generate a minimal test cert bundle using rcgen — called only from tests.
#[cfg(test)]
fn gen_certs_for_test(dir: &Path) {
    use rcgen::{BasicConstraints, CertificateParams, DistinguishedName, DnType, IsCa, KeyPair};
    use time::OffsetDateTime;

    let not_before = OffsetDateTime::now_utc();
    let not_after  = not_before + time::Duration::days(30);

    // CA
    let mut ca_dn = DistinguishedName::new();
    ca_dn.push(DnType::CommonName, "Test CA");
    let mut ca_params = CertificateParams::new(vec![]).unwrap();
    ca_params.distinguished_name = ca_dn;
    ca_params.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
    ca_params.not_before = not_before;
    ca_params.not_after = not_after;
    let ca_key = KeyPair::generate().unwrap();
    let ca_cert = ca_params.self_signed(&ca_key).unwrap();

    // Server cert
    let mut srv_dn = DistinguishedName::new();
    srv_dn.push(DnType::CommonName, "Test Server");
    let mut srv_params = CertificateParams::new(vec!["localhost".to_string(), "127.0.0.1".to_string()]).unwrap();
    srv_params.distinguished_name = srv_dn;
    srv_params.not_before = not_before;
    srv_params.not_after = not_after;
    let srv_key = KeyPair::generate().unwrap();
    let srv_cert = srv_params.signed_by(&srv_key, &ca_cert, &ca_key).unwrap();

    // Client cert
    let mut cli_dn = DistinguishedName::new();
    cli_dn.push(DnType::CommonName, "Test Client");
    let mut cli_params = CertificateParams::new(vec!["nanny-client".to_string()]).unwrap();
    cli_params.distinguished_name = cli_dn;
    cli_params.not_before = not_before;
    cli_params.not_after = not_after;
    let cli_key = KeyPair::generate().unwrap();
    let cli_cert = cli_params.signed_by(&cli_key, &ca_cert, &ca_key).unwrap();

    std::fs::write(dir.join("ca.crt"),     ca_cert.pem()).unwrap();
    std::fs::write(dir.join("ca.key"),     ca_key.serialize_pem()).unwrap();
    std::fs::write(dir.join("server.crt"), srv_cert.pem()).unwrap();
    std::fs::write(dir.join("server.key"), srv_key.serialize_pem()).unwrap();
    std::fs::write(dir.join("client.crt"), cli_cert.pem()).unwrap();
    std::fs::write(dir.join("client.key"), cli_key.serialize_pem()).unwrap();
}
