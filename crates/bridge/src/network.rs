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

use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use anyhow::{Context, Result};
use axum::{
    body::Bytes,
    extract::{Request, State},
    http::StatusCode,
    middleware::{self, Next},
    response::{IntoResponse, Response},
    routing::{get, post},
    Router,
};
use uuid::Uuid;

use nanny_runtime::ToolRegistry;

use super::{
    BridgeComponents, BridgeResp, BridgeState, ContentType,
    handle_agent_enter, handle_agent_exit, handle_events, handle_health,
    handle_rule_evaluate, handle_status, handle_step, handle_stop,
    handle_tool_call, init_shared_state, is_stopped,
};

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

// ── Proxy (HTTP CONNECT) ─────────────────────────────────────────────────────

fn host_is_allowed(host: &str, patterns: &[String]) -> bool {
    patterns.iter().any(|p| {
        if p == host {
            return true;
        }
        // Support a minimal glob pattern: "*.example.com" suffix match.
        if let Some(suffix) = p.strip_prefix("*.") {
            return host.ends_with(&format!(".{suffix}"));
        }
        false
    })
}

async fn route_proxy(State(app): State<AppState>, req: Request) -> Response {
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

    // CONNECT uses authority-form: "host:port". Axum exposes it via uri().authority().
    let authority = req
        .uri()
        .authority()
        .map(|a| a.as_str())
        .unwrap_or("");
    let host = authority.split(':').next().unwrap_or("");

    if host.is_empty() {
        return (
            StatusCode::BAD_REQUEST,
            r#"{"error":"invalid CONNECT target"}"#,
        )
            .into_response();
    }

    if !host_is_allowed(host, allowed) {
        eprintln!("nanny proxy: denied host {host}");
        return (
            StatusCode::FORBIDDEN,
            format!(r#"{{"error":"proxy destination denied","host":"{host}"}}"#),
        )
            .into_response();
    }

    // Proxying (tunneling) is not yet implemented on the network server.
    (
        StatusCode::NOT_IMPLEMENTED,
        r#"{"error":"proxy not implemented"}"#,
    )
        .into_response()
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
        // Token auth on every route
        .layer(middleware::from_fn_with_state(app.clone(), require_token))
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
    ) -> Result<()> {
        // Install ring crypto provider — safe to call multiple times.
        let _ = rustls::crypto::ring::default_provider().install_default();

        let token = session_token.unwrap_or_else(|| Uuid::new_v4().to_string());
        let (shared, registry) = init_shared_state(components, token.clone());

        let tls_config = build_tls_config(&cert_path, &key_path, &ca_path)?;
        let app = AppState {
            shared,
            registry,
            session_token: token.clone(),
            proxy_allowed_hosts,
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
            axum_server::bind_rustls(addr, rustls_config)
                .serve(build_router(app).into_make_service())
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
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::time::Duration;

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
            NetworkServer::start_blocking(addr, cert2, key2, ca2, test_components(), None, Some(token2))
                .ok();
        });

        // Give the server time to bind
        std::thread::sleep(Duration::from_millis(300));

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
            NetworkServer::start_blocking(addr, cert2, key2, ca2, test_components(), None, Some(tok2)).ok();
        });
        std::thread::sleep(Duration::from_millis(300));

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
            NetworkServer::start_blocking(addr, cert2, key2, ca2, test_components(), None, Some(tok2)).ok();
        });
        std::thread::sleep(Duration::from_millis(300));

        // Connect WITHOUT a client cert — TLS handshake must fail
        let ca_pem = std::fs::read(&ca).unwrap();
        let ca_cert = reqwest::Certificate::from_pem(&ca_pem).unwrap();

        let client = reqwest::blocking::Client::builder()
            .add_root_certificate(ca_cert)
            .use_rustls_tls()
            .danger_accept_invalid_hostnames(true)
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
