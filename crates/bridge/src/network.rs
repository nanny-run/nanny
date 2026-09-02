// network.rs: TCP + mTLS governance server for cross-process enforcement.
//
// Started by `nanny run --serve`. Multiple agents on the same or different
// machines connect to it. All connections share one execution context:
// one tool call history, one set of call counts, one stop state. This is
// cross-process enforcement without a cloud dependency.
//
// Transport: axum (HTTP routing) + rustls (mTLS, both sides present certs).
// Auth:      session token (X-Nanny-Session-Token header) + mTLS client cert.
// Together:  mTLS ensures only certified clients connect; session token is
//            defense-in-depth and per-execution identity.
//
// Usage from CLI:
//     nanny run --serve [--addr 0.0.0.0:62669] [--cert ...] [--key ...] [--ca ...]
//
// Agents point to the server via:
//     NANNY_BRIDGE_ADDR=host:port
//     NANNY_SESSION_TOKEN=<token>
//     NANNY_BRIDGE_CERT=~/.nanny/certs/client.crt
//     NANNY_BRIDGE_KEY=~/.nanny/certs/client.key
//     NANNY_BRIDGE_CA=~/.nanny/certs/ca.crt

use std::collections::HashMap;
use std::future::Future;
use std::io::Write;
use std::net::{IpAddr, SocketAddr};
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use std::task::{Context as TaskContext, Poll};
use std::time::Instant;

use anyhow::{Context, Result};
use axum::{
    body::Bytes,
    extract::{ConnectInfo, State},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    routing::{get, post},
    Router,
};
use hyper::body::Incoming;
use tower::Service as TowerService;
use uuid::Uuid;

use nanny_runtime::ToolRegistry;

use super::{
    handle_agent_enter, handle_agent_exit, handle_app, handle_events, handle_harness,
    handle_health, handle_llm_usage, handle_rule_evaluate, handle_rules, handle_status,
    handle_stop, handle_tool_call, init_run_template, record_governor, stopped_reason,
    take_run_events, BridgeComponents, BridgeResp, BridgeState, ContentType, RunTemplate,
};
use std::sync::mpsc::Sender;

/// Run id used when a request carries no `X-Nanny-Run-Id` header.
///
/// All headerless clients share this one run: preserving the single-execution
/// behaviour (one team, one task). Distinct run ids get isolated counters and
/// stop independently, because Nanny stops the run, not the host.
const DEFAULT_RUN_ID: &str = "default";

/// The governor's default port. 62669 spells NANNY on a phone keypad.
pub const DEFAULT_GOVERNOR_PORT: u16 = 62669;

/// How many ports to try past the requested one before giving up. Generous
/// enough that a developer with a handful of governors never notices, bounded
/// so a pathological box errors instead of scanning 64k ports.
const PORT_FALLFORWARD_ATTEMPTS: u16 = 64;

/// The governor's default listen address: loopback on the well-known port.
///
/// Exported so the CLI's `--addr` default and the fall-forward check here can
/// never drift apart. Fall-forward fires only on an exact match with this
/// address, so it must be one definition, not two literals.
pub fn default_governor_addr() -> SocketAddr {
    SocketAddr::from(([127, 0, 0, 1], DEFAULT_GOVERNOR_PORT))
}

/// Bind `addr`, stepping to the next port if it is already taken.
///
/// Bind-then-retry, never check-then-bind: probing whether a port is free and
/// *then* binding it leaves a window where two governors starting together
/// both see it free and one dies. Only the kernel can answer "is this port
/// mine" without a race, so we let it.
///
/// Fall-forward fires **only** for the exact default address
/// (127.0.0.1:62669), where several governors on one dev machine is the normal
/// case and stepping to the next port is what the operator wants.
///
/// Every other address is exact-or-error, and the comparison is on the whole
/// address rather than just the port for a specific reason:
/// `--addr 0.0.0.0:62669` is a deliberate choice to be reachable from the
/// network, usually paired with a firewall rule or a reverse proxy pinned to
/// that port. Quietly moving it to 62670 would leave a governor running that
/// nothing can reach, which is worse than refusing to start.
///
/// Port 0 is honoured as "any free port", the standard meaning, and needs no
/// retry loop since the kernel picks.
fn bind_with_fallforward(requested: SocketAddr) -> Result<std::net::TcpListener> {
    let explicit = requested != default_governor_addr();
    let mut addr = requested;

    for attempt in 0..PORT_FALLFORWARD_ATTEMPTS {
        match std::net::TcpListener::bind(addr) {
            Ok(listener) => {
                // `std::net` hands back a blocking socket, and tokio refuses to
                // register one ("Registering a blocking socket with the tokio
                // runtime is unsupported"). axum_server::from_tcp panics rather
                // than erroring, so this must be set before it sees the socket.
                listener
                    .set_nonblocking(true)
                    .context("failed to set the listener non-blocking")?;
                if attempt > 0 {
                    println!(
                        "nanny: port {} was in use, listening on {} instead",
                        requested.port(),
                        addr.port()
                    );
                }
                return Ok(listener);
            }
            Err(e) if e.kind() == std::io::ErrorKind::AddrInUse && !explicit => {
                let Some(next) = addr.port().checked_add(1) else {
                    break;
                };
                addr.set_port(next);
            }
            Err(e) if e.kind() == std::io::ErrorKind::AddrInUse => {
                anyhow::bail!(
                    "address {addr} is already in use\n\n\
                     Another process is listening there. Either stop it, or pick a \
                     different address with --addr.\n\
                     If it's another nanny governor, `nanny status --app=<appId>` \
                     will tell you which app owns it."
                );
            }
            Err(e) => {
                return Err(anyhow::Error::new(e).context(format!("failed to bind {addr}")));
            }
        }
    }

    anyhow::bail!(
        "no free port found in {PORT_FALLFORWARD_ATTEMPTS} attempts from {}\n\n\
         That many consecutive ports being busy usually means something else is \
         wrong on this machine. Pick an explicit address with --addr.",
        requested.port()
    )
}

/// Write a shared secret to disk, owner-readable only, with no window in
/// which it is readable by anyone else.
///
/// The mode is set **as the file is created**, not applied afterwards. Writing
/// first and calling `set_permissions` second (what this replaced) leaves the
/// file at the process umask (commonly 0644) for the moment in between, which
/// on a multi-user box is long enough to read a token out of.
fn write_secret_file(path: &Path, contents: &str) -> Result<()> {
    #[cfg(unix)]
    {
        use std::io::Write as _;
        use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
        let mut f = std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .open(path)
            .with_context(|| format!("failed to create {}", path.display()))?;
        f.write_all(contents.as_bytes())
            .with_context(|| format!("failed to write {}", path.display()))?;
        // `.mode()` only applies when the file is created, so an existing file
        // from an earlier run keeps its old mode. Enforce it either way.
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))
            .with_context(|| format!("failed to restrict {}", path.display()))?;
    }
    #[cfg(not(unix))]
    {
        // Windows has no umask window; ACLs on the per-user state directory are
        // the protection there.
        std::fs::write(path, contents)
            .with_context(|| format!("failed to write {}", path.display()))?;
    }
    Ok(())
}

// ── Per-IP rate limiter ───────────────────────────────────────────────────────

/// Sliding-window per-IP rate limiter.  DoS protection only: never a
/// business-tier gate and never in nanny.toml.  Hardcoded safe default;
/// power users override with `--rate-limit` on `nanny run --serve`.
#[derive(Clone)]
struct RateLimiter {
    inner: Arc<Mutex<std::collections::HashMap<IpAddr, (u32, Instant)>>>,
    rps: u32,
}

impl RateLimiter {
    fn new(rps: u32) -> Self {
        Self {
            inner: Arc::new(Mutex::new(std::collections::HashMap::new())),
            rps,
        }
    }

    /// Returns `true` if the request is within the rate limit.
    fn check(&self, ip: IpAddr) -> bool {
        let mut map = self.inner.lock().unwrap();
        let now = Instant::now();
        let entry = map.entry(ip).or_insert((0u32, now));
        if now.duration_since(entry.1).as_secs() >= 1 {
            // New second window: reset counter.
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
    /// Per-run enforcement state, keyed by run id (from `X-Nanny-Run-Id`, or
    /// [`DEFAULT_RUN_ID`] when absent). Each run has its own counters and stop
    /// state: a stop ends that run, not the server. Runs are created
    /// lazily on first reference from [`AppState::template`].
    runs: Arc<Mutex<HashMap<String, Arc<Mutex<BridgeState>>>>>,
    /// Template for minting a fresh run on first reference.
    template: Arc<RunTemplate>,
    registry: Arc<ToolRegistry>,
    /// Session token stored separately for fast auth check without locking.
    /// Guards every request: tool calls, status, everything.
    session_tokens: Vec<String>,
    /// Per-IP rate limiter: DoS protection.
    rate_limiter: RateLimiter,
}

/// Constant-time byte comparison for the session token. Plain
/// `==` short-circuits on the first differing byte, which leaks a timing
/// signal proportional to how many leading bytes an attacker guessed
/// correctly. This is the standard XOR-accumulate technique, no crypto
/// library needed for a short, fixed-shape token compare.
fn secure_compare(a: &str, b: &str) -> bool {
    let (a, b) = (a.as_bytes(), b.as_bytes());
    if a.len() != b.len() {
        return false;
    }
    let mut diff = 0u8;
    for (x, y) in a.iter().zip(b.iter()) {
        diff |= x ^ y;
    }
    diff == 0
}

impl AppState {
    /// Resolve the enforcement state for the run named in `X-Nanny-Run-Id`,
    /// creating it on first reference. A missing or empty header maps to
    /// [`DEFAULT_RUN_ID`], so headerless clients keep the shared-run behaviour
    /// and every request for the same id shares one run.
    fn run_state(&self, headers: &HeaderMap) -> Arc<Mutex<BridgeState>> {
        let run_id = headers
            .get("x-nanny-run-id")
            .and_then(|v| v.to_str().ok())
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .unwrap_or(DEFAULT_RUN_ID)
            .to_string();

        let run_id_for_state = run_id.clone();
        self.runs
            .lock()
            .unwrap()
            .entry(run_id)
            .or_insert_with(|| self.template.build_state(&run_id_for_state))
            .clone()
    }
}

// ── Auth & rate-limit checks ───────────────────────────────────────────────────
//
// Plain functions, not axum middleware. They're called from exactly one
// place (`GovernorService::call`, below) which is the single dispatch
// point every request passes through before anything else happens, CONNECT
// included. This used to be two axum `.layer()` calls on the router, which
// worked for ordinary requests but silently never ran for CONNECT (CONNECT
// bypasses the router entirely, see `handle_connect`'s doc comment for why).
// Rather than duplicate these checks in two places that could drift apart,
// there is now exactly one place they can be added: here, called
// unconditionally from `GovernorService::call`. Any FUTURE check meant to
// apply to all traffic (an audit log, a body-size cap, whatever) belongs
// here too: a `Router.layer()` only ever sees non-CONNECT traffic, by
// construction, so it is structurally the wrong place for anything that
// must cover every request.

/// Checks the `X-Nanny-Session-Token` header; guards every ordinary request.
/// A session token, shortened so it can appear in a log.
///
/// The governor used to print the token in full at startup, in both transport
/// modes. That is a convenience on a laptop and a credential leak in a
/// deployment: a container writes it to stdout on every boot, straight into
/// whatever aggregates the logs, and the one thing it admits is a process to
/// this governor.
///
/// It is still printed, because "is it using the token I set?" is a real
/// question and the line above only says that a token was taken, not which.
/// Enough to recognise, not enough to use: the floor is 32 characters, so this
/// leaves at least 20 of them unseen, and the full value is on disk in
/// `server.token` for anything that actually needs it.
fn token_fingerprint(token: &str) -> String {
    let chars: Vec<char> = token.chars().collect();
    let head: String = chars.iter().take(8).collect();
    let tail: String = chars[chars.len().saturating_sub(4)..].iter().collect();
    format!("{head}…{tail} ({} chars)", chars.len())
}

/// Whether the request carries one of the tokens this governor accepts.
///
/// **Every candidate is compared, with no early exit.** Returning as soon as
/// one matches would leak, through timing, which entry matched and therefore
/// how far through a rotation the fleet is. The set holds two entries during a
/// rotation and one the rest of the time, so the cost of comparing all of them
/// is not worth an information leak.
fn session_token_ok(headers: &HeaderMap, expected: &[String]) -> bool {
    let Some(got) = headers
        .get("x-nanny-session-token")
        .and_then(|v| v.to_str().ok())
    else {
        return false;
    };
    let mut matched = false;
    for candidate in expected {
        matched |= secure_compare(got, candidate);
    }
    matched
}

fn rate_limit_ok(app: &AppState, peer: SocketAddr) -> bool {
    app.rate_limiter.check(peer.ip())
}

// ── Response conversion ───────────────────────────────────────────────────────

fn to_response(resp: BridgeResp) -> Response {
    let ct = match resp.content_type {
        ContentType::Json => "application/json",
        ContentType::Ndjson => "application/x-ndjson",
    };
    let status = StatusCode::from_u16(resp.status).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);
    (status, [(axum::http::header::CONTENT_TYPE, ct)], resp.body).into_response()
}

/// 410 Gone response for a stopped run, carrying the typed stop reason so the
/// client reports the true cause instead of a generic "execution stopped".
fn stopped_gone(reason: &str) -> Response {
    (
        StatusCode::GONE,
        [(axum::http::header::CONTENT_TYPE, "application/json")],
        format!(r#"{{"error":"execution stopped","reason":"{reason}"}}"#),
    )
        .into_response()
}

// ── Route handlers ────────────────────────────────────────────────────────────

async fn route_health(State(app): State<AppState>, headers: HeaderMap) -> Response {
    to_response(handle_health(&app.run_state(&headers)))
}

async fn route_status(State(app): State<AppState>, headers: HeaderMap) -> Response {
    to_response(handle_status(&app.run_state(&headers)))
}

async fn route_events(State(app): State<AppState>, headers: HeaderMap) -> Response {
    to_response(handle_events(&app.run_state(&headers)))
}

async fn route_stop(State(app): State<AppState>, headers: HeaderMap, body: Bytes) -> Response {
    to_response(handle_stop(&body, &app.run_state(&headers)))
}

async fn route_tool_call(State(app): State<AppState>, headers: HeaderMap, body: Bytes) -> Response {
    let shared = app.run_state(&headers);
    // Action endpoints return 410 after this run stops: other runs are unaffected.
    if let Some(reason) = stopped_reason(&shared) {
        return stopped_gone(&reason);
    }
    to_response(handle_tool_call(&body, &shared, &app.registry))
}

async fn route_rule_evaluate(
    State(app): State<AppState>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    let shared = app.run_state(&headers);
    if let Some(reason) = stopped_reason(&shared) {
        return stopped_gone(&reason);
    }
    to_response(handle_rule_evaluate(&body, &shared))
}

async fn route_agent_enter(
    State(app): State<AppState>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    let shared = app.run_state(&headers);
    if let Some(reason) = stopped_reason(&shared) {
        return stopped_gone(&reason);
    }
    to_response(handle_agent_enter(&body, &shared))
}

async fn route_agent_exit(State(app): State<AppState>, headers: HeaderMap) -> Response {
    to_response(handle_agent_exit(&app.run_state(&headers)))
}

async fn route_llm_usage(State(app): State<AppState>, headers: HeaderMap, body: Bytes) -> Response {
    let shared = app.run_state(&headers);
    if let Some(reason) = stopped_reason(&shared) {
        return stopped_gone(&reason);
    }
    to_response(handle_llm_usage(&body, &shared))
}

async fn route_harness(State(app): State<AppState>, headers: HeaderMap, body: Bytes) -> Response {
    let shared = app.run_state(&headers);
    if let Some(reason) = stopped_reason(&shared) {
        return stopped_gone(&reason);
    }
    to_response(handle_harness(&body, &shared))
}

async fn route_rules(State(app): State<AppState>, headers: HeaderMap, body: Bytes) -> Response {
    let shared = app.run_state(&headers);
    if let Some(reason) = stopped_reason(&shared) {
        return stopped_gone(&reason);
    }
    to_response(handle_rules(&body, &shared))
}

async fn route_app(State(app): State<AppState>, headers: HeaderMap, body: Bytes) -> Response {
    let shared = app.run_state(&headers);
    if let Some(reason) = stopped_reason(&shared) {
        return stopped_gone(&reason);
    }
    to_response(handle_app(&body, &shared))
}

/// Router fallback for genuinely unmatched, non-CONNECT requests. CONNECT
/// never reaches this (`GovernorService` intercepts it earlier) so this is
/// just a 404 for any other unrecognized method/path.
async fn route_not_found() -> Response {
    (StatusCode::NOT_FOUND, r#"{"error":"Not Found"}"#).into_response()
}

// ── GovernorService: the ONE checkpoint every request passes through ────────
//
// A hand-rolled `tower::MakeService`/`Service` pair standing in for
// `Router::into_make_service_with_connect_info`. This exists because CONNECT
// must never reach axum's `Router::call()` (see `handle_connect`'s doc
// comment for why), so something has to sit in front of the router and
// branch, and since that something already sees every request before
// anything else does, it is also the single, structurally-unbypassable place
// rate-limiting and auth happen. There is no axum `.layer()` for either
// anymore: a `Router.layer()` only ever sees non-CONNECT traffic (the router
// isn't even reached until after this checkpoint), so it was the wrong place
// for anything meant to apply universally: a future protection added there
// would silently never cover CONNECT. Any check that must apply to ALL
// traffic belongs in `GovernorService::call`, below, full stop; that's not a
// convention to remember, it's the only place wired to see everything.
#[derive(Clone)]
struct GovernorMakeService {
    router: Router,
    app: AppState,
}

impl TowerService<SocketAddr> for GovernorMakeService {
    type Response = GovernorService;
    type Error = std::convert::Infallible;
    type Future = std::future::Ready<Result<Self::Response, Self::Error>>;

    fn poll_ready(&mut self, _cx: &mut TaskContext<'_>) -> Poll<Result<(), Self::Error>> {
        Poll::Ready(Ok(()))
    }

    fn call(&mut self, peer: SocketAddr) -> Self::Future {
        std::future::ready(Ok(GovernorService {
            router: self.router.clone(),
            app: self.app.clone(),
            peer,
        }))
    }
}

#[derive(Clone)]
struct GovernorService {
    router: Router,
    app: AppState,
    peer: SocketAddr,
}

impl TowerService<hyper::Request<Incoming>> for GovernorService {
    type Response = Response;
    type Error = std::convert::Infallible;
    type Future = Pin<Box<dyn Future<Output = Result<Self::Response, Self::Error>> + Send>>;

    fn poll_ready(&mut self, _cx: &mut TaskContext<'_>) -> Poll<Result<(), Self::Error>> {
        Poll::Ready(Ok(()))
    }

    fn call(&mut self, req: hyper::Request<Incoming>) -> Self::Future {
        let app = self.app.clone();
        let peer = self.peer;
        let mut router = self.router.clone();

        Box::pin(async move {
            // ── Universal checkpoint: every request ──────────────────────
            if !rate_limit_ok(&app, peer) {
                return Ok((
                    StatusCode::TOO_MANY_REQUESTS,
                    r#"{"error":"rate limit exceeded"}"#,
                )
                    .into_response());
            }

            // Liveness is the one route served without the token, so a
            // container orchestrator can probe a governor without being given
            // the credential that admits a process to it. It reports whether
            // the run is up and nothing else; every route carrying operational
            // detail, `/status` included, stays behind the check below.
            //
            // The rate limit above still applies, so an unauthenticated route
            // cannot be used to flood the governor.
            if req.uri().path() != "/health"
                && !session_token_ok(req.headers(), &app.session_tokens)
            {
                return Ok(
                    (StatusCode::UNAUTHORIZED, r#"{"error":"Unauthorized"}"#).into_response()
                );
            }
            let mut req = req.map(axum::body::Body::new);
            req.extensions_mut().insert(ConnectInfo(peer));
            match TowerService::call(&mut router, req).await {
                Ok(resp) => Ok(resp),
                Err(never) => match never {},
            }
        })
    }
}

// ── Router ────────────────────────────────────────────────────────────────────
//
// No auth or rate-limit layers here anymore, GovernorService (above) is the
// single checkpoint both are enforced at, for every request, before the
// router is ever reached.

fn build_router(app: AppState) -> Router {
    Router::new()
        // Read-only: always available
        .route("/health", get(route_health))
        .route("/status", get(route_status))
        .route("/events", get(route_events))
        // /stop: always accepted (idempotent)
        .route("/stop", post(route_stop))
        // Action endpoints: return 410 when stopped
        .route("/tool/call", post(route_tool_call))
        .route("/rule/evaluate", post(route_rule_evaluate))
        .route("/agent/enter", post(route_agent_enter))
        .route("/agent/exit", post(route_agent_exit))
        .route("/llm/usage", post(route_llm_usage))
        .route("/harness", post(route_harness))
        .route("/rules", post(route_rules))
        .route("/app", post(route_app))
        // Fallback for genuinely unmatched requests. CONNECT never reaches this;
        // GovernorService (see above) intercepts it before the router at all.
        .fallback(route_not_found)
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

    // Load CA cert: used to verify client certificates.
    let ca_pem = std::fs::read(ca_path)
        .with_context(|| format!("failed to read CA cert: {}", ca_path.display()))?;
    let ca_certs: Vec<CertificateDer<'static>> = rustls_pemfile::certs(&mut ca_pem.as_ref())
        .collect::<std::result::Result<Vec<_>, _>>()
        .context("failed to parse CA cert PEM")?;

    let mut root_store = RootCertStore::empty();
    for cert in ca_certs {
        root_store
            .add(cert)
            .context("failed to add CA cert to root store")?;
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
        let mut sigterm =
            signal(SignalKind::terminate()).expect("failed to install SIGTERM handler");
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
/// `nanny stop` sends SIGTERM.
pub struct NetworkServer;

impl NetworkServer {
    /// Start the mTLS governance server and block until shutdown.
    ///
    /// `session_token`: if `Some`, use that token; if `None`, generate a fresh UUID.
    /// The token is printed to stdout and written to `<state_dir>/server.token` so
    /// `nanny run --join=<id>` can auto-inject it into child environments.
    /// `state_dir` is per-app (`~/.nanny/servers/<app_id>/`, resolved by the
    /// caller), never the shared `~/.nanny`, so two unrelated apps' governors
    /// on one machine can never collide or overwrite each other's state.
    /// Start the server with no cloud forwarding: the common case and every
    /// existing entry point. See [`Self::start_blocking_synced`] to attach a
    /// per-run event sink for cloud sync.
    #[allow(clippy::too_many_arguments)]
    pub fn start_blocking(
        addr: SocketAddr,
        cert_path: PathBuf,
        key_path: PathBuf,
        ca_path: PathBuf,
        components: BridgeComponents,
        session_tokens: Option<Vec<String>>,
        rate_limit_rps: u32,
        state_dir: PathBuf,
    ) -> Result<()> {
        Self::start_blocking_synced(
            addr,
            cert_path,
            key_path,
            ca_path,
            components,
            session_tokens,
            rate_limit_rps,
            None,
            state_dir,
            None,
        )
    }

    /// Same as [`Self::start_blocking`], plus an optional `event_sink`: when
    /// `Some`, a background thread drains each run's events and sends
    /// `(run_id, lines)` to it. This is the ONLY hook cloud sync uses; the engine
    /// stays auth free: it never talks to the cloud, it just hands off strings.
    ///
    /// `local_log_path`: when `Some`, the same drain thread also appends each
    /// drained line to this file, flushed per write. This is what makes
    /// `[observability] log = "file"` behave identically whether the process
    /// is local `nanny run` or `nanny run --serve`: before this, `nanny.toml`
    /// promised a log file and `--serve` silently never wrote one, the config
    /// was only ever honored by the local, single-process run path. The
    /// caller (`commands/server.rs`) resolves this from the server's own
    /// `nanny.toml`, the same `[observability]` table local `nanny run`
    /// already reads via `EventWriter::from_config`.
    #[allow(clippy::too_many_arguments)]
    pub fn start_blocking_synced(
        addr: SocketAddr,
        cert_path: PathBuf,
        key_path: PathBuf,
        ca_path: PathBuf,
        components: BridgeComponents,
        session_tokens: Option<Vec<String>>,
        rate_limit_rps: u32, // max req/s per client IP, DoS protection, default 100
        event_sink: Option<Sender<(String, Vec<String>)>>,
        state_dir: PathBuf, // ~/.nanny/servers/<app_id>/, keyed, per-app, never shared
        local_log_path: Option<PathBuf>,
    ) -> Result<()> {
        // Install ring crypto provider: safe to call multiple times.
        let _ = rustls::crypto::ring::default_provider().install_default();

        // Bind before writing any state, so the files record the port actually
        // in use rather than the one that was requested, since an occupied default
        // steps forward, and `--join`/`--app` must find the real one.
        let listener = bind_with_fallforward(addr)?;
        let addr = listener
            .local_addr()
            .context("bound listener has no local address")?;

        // Configured tokens are accepted as a set so a rotation can hold two at
        // once; with none configured a single one is minted, unchanged.
        let tokens =
            session_tokens.unwrap_or_else(|| vec![Uuid::new_v4().to_string()]);
        // The first is the one published to disk and printed: a joiner needs
        // one that works, not all of them, and during a rotation the operator
        // already holds the one being introduced.
        let token = tokens
            .first()
            .cloned()
            .expect("resolve_session_token never yields an empty set");
        let (template, registry) = init_run_template(components, token.clone());
        let template = Arc::new(template);

        // Pre-create the default run so /health and /status answer before any
        // action endpoint is hit. Distinct run ids are minted lazily on demand.
        let runs: Arc<Mutex<HashMap<String, Arc<Mutex<BridgeState>>>>> =
            Arc::new(Mutex::new(HashMap::new()));
        let default_state = template.build_state(DEFAULT_RUN_ID);
        runs.lock()
            .unwrap()
            .insert(DEFAULT_RUN_ID.to_string(), default_state.clone());

        // GovernorIdentified: declared once, into the default run's
        // own event stream, so it drains and forwards the same way every other
        // bridge event does: no separate server-level channel needed. Best
        // effort: a hostname lookup can fail (sandboxed/minimal containers),
        // and this is attribution, never a credential the run depends on.
        {
            let name = hostname::get()
                .ok()
                .and_then(|h| h.into_string().ok())
                .unwrap_or_else(|| "unknown".to_string());
            let mut guard = default_state.lock().unwrap();
            record_governor(
                &mut guard,
                name,
                addr.to_string(),
                env!("CARGO_PKG_VERSION").to_string(),
            );
        }

        // Draining hook: when either a cloud sink or a local log path is
        // attached, a background thread drains each run's events. Draining is
        // destructive (take_run_events removes what it returns), so this is
        // the one place that reads them; both destinations get their own
        // copy of the same drained lines, neither steals from the other.
        // Cloud sink: hands `(run_id, lines)` to the cli-layer forwarder, no
        // cloud code lives here. Local log: appends each line to
        // `local_log_path`, flushed per write, same guarantee
        // `EventWriter` (the local `nanny run` path) already gives: this is
        // what makes `[observability] log = "file"` behave the same whether
        // the process is local `nanny run` or `nanny run --serve`.
        if event_sink.is_some() || local_log_path.is_some() {
            let drain_runs = Arc::clone(&runs);
            let mut local_log_file = match &local_log_path {
                Some(path) => match std::fs::OpenOptions::new()
                    .create(true)
                    .append(true)
                    .open(path)
                {
                    Ok(f) => Some(f),
                    Err(e) => {
                        eprintln!(
                            "nanny: failed to open local log file '{}': {e}",
                            path.display()
                        );
                        None
                    }
                },
                None => None,
            };
            std::thread::spawn(move || loop {
                std::thread::sleep(std::time::Duration::from_millis(250));
                let ids: Vec<String> = {
                    let guard = drain_runs.lock().unwrap();
                    guard.keys().cloned().collect()
                };
                for id in ids {
                    let state = drain_runs.lock().unwrap().get(&id).cloned();
                    if let Some(state) = state {
                        let lines = take_run_events(&state);
                        if lines.is_empty() {
                            continue;
                        }
                        if let Some(file) = local_log_file.as_mut() {
                            for line in &lines {
                                let _ = writeln!(file, "{line}");
                            }
                            let _ = file.flush();
                        }
                        if let Some(sink) = &event_sink {
                            if sink.send((id, lines)).is_err() {
                                return; // forwarder gone → stop draining
                            }
                        }
                    }
                }
            });
        }

        let app = AppState {
            runs,
            template,
            registry,
            session_tokens: tokens.clone(),
            rate_limiter: RateLimiter::new(rate_limit_rps),
        };

        // Write token to <state_dir>/server.token for auto-injection by
        // `nanny run --join=<id>`. Keyed per-app, never the shared ~/.nanny.
        //
        // **None of this is fatal.** These files exist so another process on
        // this machine can discover a governor: `--join` reads the address and
        // token, `status` and `stop` find the pid. A process joining from
        // another host is given all of that as configuration and never reads
        // them, so a deployment on a read-only filesystem would otherwise be
        // refused a governor over bookkeeping it cannot use. When the address
        // was passed with `--addr` and the token supplied through the
        // environment, the governor is declining to start because it cannot
        // write down what it was just told.
        let state_dir_ok = match std::fs::create_dir_all(&state_dir) {
            Ok(()) => true,
            Err(e) => {
                eprintln!(
                    "nanny: cannot write {} ({e}); serving anyway. \
                     `nanny status`, `nanny stop` and `nanny run --join` cannot \
                     discover this governor on this machine, so a joining process \
                     needs NANNY_BRIDGE_ADDR and NANNY_SESSION_TOKEN set explicitly.",
                    state_dir.display()
                );
                false
            }
        };

        // The address actually bound, which is not necessarily the one
        // requested. `nanny status --app` and `nanny run --join` read this file
        // to find the real server, so it has to be written here, after the
        // bind, by the code that owns the socket. Writing the *requested*
        // address (as the CLI used to) would point every joiner at a port
        // nothing is listening on.
        let addr_file = state_dir.join("server.addr");
        if state_dir_ok {
            if let Err(e) = std::fs::write(&addr_file, addr.to_string()) {
                eprintln!("nanny: cannot write {} ({e}); continuing.", addr_file.display());
            }
        }

        // A shared secret: created owner-read-only, never merely chmod'd
        // after the fact.
        let token_file = state_dir.join("server.token");
        if state_dir_ok {
            if let Err(e) = write_secret_file(&token_file, &token) {
                eprintln!("nanny: cannot write {} ({e:#}); continuing.", token_file.display());
            }
        }

        // PID file so `nanny stop --app=<id>` can send SIGTERM.
        let pid_file = state_dir.join("server.pid");
        if state_dir_ok {
            if let Err(e) = std::fs::write(&pid_file, std::process::id().to_string()) {
                eprintln!("nanny: cannot write {} ({e}); continuing.", pid_file.display());
            }
        }

        // Instructions aimed at a person at a keyboard: how to join from this
        // machine, and which key stops it. In a container both are noise in a
        // log that nobody can type into, and "Press CTRL-C to stop" is simply
        // untrue there. A terminal check separates the two without needing a
        // flag or an environment variable to be set correctly.
        let interactive = std::io::IsTerminal::is_terminal(&std::io::stdout());

        // A fingerprint, never the token itself: see `token_fingerprint`. The
        // count matters during a rotation, when the governor is deliberately
        // holding two and an operator wants to see that it took both.
        let accepted = if tokens.len() == 1 {
            token_fingerprint(&token)
        } else {
            format!("{} (+{} more accepted)", token_fingerprint(&token), tokens.len() - 1)
        };
        if addr.ip().is_loopback() {
            println!("nanny: governance server started  (plain HTTP, loopback)");
            println!("  address      : {addr}");
            println!("  session token: {accepted}");
            if state_dir_ok {
                println!("  token file   : {}", token_file.display());
            }
            if interactive {
                println!();
                println!("Join with: nanny run --join=<this app's id>  (see .nanny/app.json)");
            }
        } else {
            println!("nanny: governance server started  (mTLS)");
            println!("  address      : {addr}");
            println!("  session token: {accepted}");
            println!("  token file   : {}", token_file.display());
            println!();
            println!("Join with: nanny run --join=<this app's id>  (see .nanny/app.json)");
            println!();
            println!("Cross-machine agents, set these in your deployment config:");
            println!("  NANNY_BRIDGE_ADDR={addr}");
            println!("  NANNY_SESSION_TOKEN=$(cat {})", token_file.display());
            println!(
                "  NANNY_BRIDGE_CERT, NANNY_BRIDGE_KEY, NANNY_BRIDGE_CA  (from ~/.nanny/certs/)"
            );
        }
        if interactive {
            println!();
            println!("Press CTRL-C to stop.");
        }

        let rt = tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()
            .context("failed to build tokio runtime")?;

        let result = rt.block_on(async {
            // ── Graceful SIGTERM drain ────────────────────────────────────────
            // `nanny stop` sends SIGTERM (Unix) / taskkill (Windows).
            // We give in-flight requests 10 s to complete before forcing exit.
            let server_handle = axum_server::Handle::new();
            {
                let drain = server_handle.clone();
                tokio::spawn(async move {
                    graceful_shutdown_signal().await;
                    eprintln!(
                        "nanny: governance server shutdown signal received, \
                         draining connections (10 s grace)…"
                    );
                    drain.graceful_shutdown(Some(std::time::Duration::from_secs(10)));
                });
            }

            let router = build_router(app.clone());
            let make_service = GovernorMakeService { router, app };

            if addr.ip().is_loopback() {
                // ── Plain HTTP (loopback) ─────────────────────────────────────
                // Loopback is OS-enforced: only processes on this machine can
                // connect. No TLS needed; session token is the auth layer.
                // `from_tcp` rather than `bind`: the socket is already bound
                // (see bind_with_fallforward), so re-binding here would fail and
                // discard the port we resolved.
                axum_server::from_tcp(listener)
                    .context("failed to adopt the bound listener")?
                    .handle(server_handle)
                    .serve(make_service)
                    .await
                    .context("server error")
            } else {
                // ── mTLS (non-loopback) ───────────────────────────────────────
                // Mandatory for any address reachable from the network.
                // build_tls_config reads and validates the cert files.
                let tls_config = build_tls_config(&cert_path, &key_path, &ca_path)
                    .context("failed to build TLS config")?;
                let rustls_config =
                    axum_server::tls_rustls::RustlsConfig::from_config(Arc::new(tls_config));

                // ── Cert hot-reload ───────────────────────────────────────────
                // Watch the directory containing the server cert. When any file
                // changes (from `nanny certs rotate` or `nanny certs import`),
                // rebuild the TLS config and swap it in without restarting.
                // New connections use the new cert; in-flight connections finish
                // on the old one. If the new files fail to parse, log and keep
                // the old config: the server never goes down on a bad write.
                if let Some(cert_dir) = cert_path.parent().map(|p| p.to_path_buf()) {
                    use notify::{RecommendedWatcher, RecursiveMode, Watcher};
                    let (tx, rx) = std::sync::mpsc::channel();
                    match RecommendedWatcher::new(tx, notify::Config::default()) {
                        Ok(mut watcher) => {
                            if watcher
                                .watch(&cert_dir, RecursiveMode::NonRecursive)
                                .is_ok()
                            {
                                // Leak the watcher: it must stay alive for the
                                // lifetime of the process to keep delivering events.
                                std::mem::forget(watcher);

                                let rc = rustls_config.clone();
                                let cp = cert_path.clone();
                                let kp = key_path.clone();
                                let cap = ca_path.clone();

                                std::thread::spawn(move || {
                                    while rx.recv().is_ok() {
                                        // Drain burst events: a single rotate/import
                                        // writes multiple files and fires many events.
                                        while rx.try_recv().is_ok() {}
                                        // Brief settle delay so all files are flushed
                                        // to disk before we re-read them.
                                        std::thread::sleep(std::time::Duration::from_millis(150));

                                        match build_tls_config(&cp, &kp, &cap) {
                                            Ok(new_cfg) => {
                                                rc.reload_from_config(Arc::new(new_cfg));
                                                eprintln!(
                                                    "nanny: governance server certs hot-reloaded"
                                                );
                                            }
                                            Err(e) => {
                                                eprintln!(
                                                    "nanny: governance server cert reload failed, \
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
                                "nanny: governance server cert watcher failed to start \
                                 (hot-reload disabled): {e}"
                            );
                        }
                    }
                }

                axum_server::from_tcp_rustls(listener, rustls_config)
                    .context("failed to adopt the bound listener")?
                    .handle(server_handle)
                    .serve(make_service)
                    .await
                    .context("server error")
            }
        });

        // Clean up PID and token files on shutdown.
        let _ = std::fs::remove_file(&pid_file);
        let _ = std::fs::remove_file(&token_file);

        result
    }
}

// ── Test cert generator (used by network tests) ───────────────────────────────

/// Generate a minimal test cert bundle using rcgen: called only from tests.
#[cfg(test)]
fn gen_certs_for_test(dir: &Path) {
    use rcgen::{
        BasicConstraints, CertificateParams, DistinguishedName, DnType, IsCa, Issuer, KeyPair,
    };
    use time::OffsetDateTime;

    let not_before = OffsetDateTime::now_utc();
    let not_after = not_before + time::Duration::days(30);

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
    let ca_issuer = Issuer::from_params(&ca_params, &ca_key);

    // Server cert
    let mut srv_dn = DistinguishedName::new();
    srv_dn.push(DnType::CommonName, "Test Server");
    let mut srv_params =
        CertificateParams::new(vec!["localhost".to_string(), "127.0.0.1".to_string()]).unwrap();
    srv_params.distinguished_name = srv_dn;
    srv_params.not_before = not_before;
    srv_params.not_after = not_after;
    let srv_key = KeyPair::generate().unwrap();
    let srv_cert = srv_params.signed_by(&srv_key, &ca_issuer).unwrap();

    // Client cert
    let mut cli_dn = DistinguishedName::new();
    cli_dn.push(DnType::CommonName, "Test Client");
    let mut cli_params = CertificateParams::new(vec!["nanny-client".to_string()]).unwrap();
    cli_params.distinguished_name = cli_dn;
    cli_params.not_before = not_before;
    cli_params.not_after = not_after;
    let cli_key = KeyPair::generate().unwrap();
    let cli_cert = cli_params.signed_by(&cli_key, &ca_issuer).unwrap();

    std::fs::write(dir.join("ca.crt"), ca_cert.pem()).unwrap();
    std::fs::write(dir.join("ca.key"), ca_key.serialize_pem()).unwrap();
    std::fs::write(dir.join("server.crt"), srv_cert.pem()).unwrap();
    std::fs::write(dir.join("server.key"), srv_key.serialize_pem()).unwrap();
    std::fs::write(dir.join("client.crt"), cli_cert.pem()).unwrap();
    std::fs::write(dir.join("client.key"), cli_key.serialize_pem()).unwrap();
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use nanny_runtime::ToolRegistry;
    use std::collections::{BTreeSet, HashMap};
    use std::sync::atomic::Ordering;
    use std::time::Duration;

    // Path parity ─────────────────────────────────────────────────────────────

    /// Every POST path the socket dispatch in `lib.rs` answers.
    ///
    /// Read out of the source rather than maintained by hand, so the test
    /// cannot pass against a list that has drifted from the code it describes.
    ///
    /// Two forms, because the dispatch has two. Most paths are match arms
    /// (`("POST", "/x") => …`), but `/stop` is an early guard above the match
    /// (`method == "POST" && path == "/stop"`) since it stays accepted after a
    /// run has stopped. Reading only the arms would have reported `/stop` as
    /// network-only, which is exactly the kind of false positive that gets a
    /// parity test deleted.
    fn dispatch_post_paths() -> BTreeSet<String> {
        let src = include_str!("lib.rs");
        src.lines()
            .filter_map(|line| {
                let line = line.trim();
                if let Some(rest) = line.strip_prefix(r#"("POST", ""#) {
                    return rest.split('"').next().map(str::to_string);
                }
                if line.starts_with(r#"if method == "POST" && path == ""#) {
                    return line
                        .split(r#"path == ""#)
                        .nth(1)?
                        .split('"')
                        .next()
                        .map(str::to_string);
                }
                None
            })
            .collect()
    }

    /// Every POST path registered on the axum router below.
    fn router_post_paths() -> BTreeSet<String> {
        let src = include_str!("network.rs");
        src.lines()
            .filter_map(|line| {
                let trimmed = line.trim();
                // `.route("/x", post(...))`: only POST; GET routes are the
                // read surface and have no dispatch counterpart.
                if !trimmed.starts_with(".route(\"") || !trimmed.contains("post(") {
                    return None;
                }
                let path = trimmed.strip_prefix(".route(\"")?.split('"').next()?;
                Some(path.to_string())
            })
            .collect()
    }

    /// The two transports must answer the same set of POST paths.
    ///
    /// This is the defect that produced the test. `POST /rules` was added to
    /// the socket dispatch and never to the router, so under `--serve` it fell
    /// through to `route_not_found` and 404'd: and both SDKs post it
    /// fire-and-forget (`let _ = http_post("/rules", …)`), so nothing surfaced.
    /// The rules half of declared authority silently never arrived for exactly
    /// the fleet deployments most likely to be paying for it.
    ///
    /// Asserting set *equality*, not "rules is present", is the point: the
    /// class of bug is a path added to one transport and not the other, in
    /// either direction, and only equality catches the next one.
    #[test]
    fn socket_and_network_answer_the_same_post_paths() {
        let dispatch = dispatch_post_paths();
        let router = router_post_paths();

        assert!(
            !dispatch.is_empty() && !router.is_empty(),
            "path extraction found nothing, the parser has drifted from the source, \
             which would make this test vacuous"
        );
        assert_eq!(
            dispatch,
            router,
            "socket dispatch and axum router disagree on POST paths.\n\
             only in dispatch: {:?}\n\
             only in router:   {:?}",
            dispatch.difference(&router).collect::<Vec<_>>(),
            router.difference(&dispatch).collect::<Vec<_>>(),
        );
    }

    // secure_compare ──────────────────────────────────────────────────────────

    #[test]
    fn secure_compare_matches_equal_strings() {
        assert!(secure_compare("same-token-value", "same-token-value"));
    }

    #[test]
    fn secure_compare_rejects_different_strings_same_length() {
        assert!(!secure_compare("token-aaaaaaaaaa", "token-bbbbbbbbbb"));
    }

    #[test]
    fn secure_compare_rejects_different_lengths() {
        assert!(!secure_compare("short", "a-lot-longer-value"));
        assert!(!secure_compare("a-lot-longer-value", "short"));
    }

    #[test]
    fn secure_compare_rejects_empty_against_nonempty() {
        assert!(!secure_compare("", "nonempty"));
        assert!(secure_compare("", ""));
    }

    // ── Day 6/7 unit tests ────────────────────────────────────────────────────

    // host_is_allowed ─────────────────────────────────────────────────────────

    // is_blocked_host ─────────────────────────────────────────────────────────

    // ── Test fixtures ─────────────────────────────────────────────────────────

    fn test_components() -> BridgeComponents {
        BridgeComponents {
            registry: ToolRegistry::new(),
            allowed_tools: vec!["echo".to_string()],
            per_tool_max_calls: HashMap::new(),
            tool_labels: Default::default(),
        }
    }

    /// Ask the OS for a port that is genuinely free right now.
    ///
    /// Binding to port 0 makes the kernel pick one, then we release it and
    /// hand the number to the server under test.
    ///
    /// This replaces a fixed base (15200) plus an incrementing counter, which
    /// was unique only *within a single process*. Across back-to-back
    /// `cargo test` runs the previous run's sockets are still in TIME_WAIT on
    /// those exact ports, and `% 200` meant the 201st test wrapped onto the
    /// first one's port. That is how a cross-test port collision became
    /// flaky: it failed four runs out of four when the suite was run
    /// repeatedly, while passing every time in isolation.
    ///
    /// There is a small window between releasing the listener and the server
    /// binding, but the kernel does not hand out the same ephemeral port twice
    /// in quick succession, so this is dramatically better than a fixed range.
    fn next_port() -> u16 {
        std::net::TcpListener::bind("127.0.0.1:0")
            .expect("the OS must be able to hand out an ephemeral port")
            .local_addr()
            .expect("a bound listener always has a local address")
            .port()
    }

    fn test_certs_dir() -> PathBuf {
        use std::sync::atomic::AtomicU64;
        static CNT: AtomicU64 = AtomicU64::new(0);
        let id = CNT.fetch_add(1, Ordering::SeqCst);
        let dir =
            std::env::temp_dir().join(format!("nanny-net-test-{}-{}", std::process::id(), id));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// A scratch `state_dir` for `start_blocking`/`start_blocking_synced` in
    /// tests, real usage keys this by app id under `~/.nanny/servers/`; tests
    /// use an isolated temp dir per call so parallel tests never collide.
    fn test_state_dir() -> PathBuf {
        use std::sync::atomic::AtomicU64;
        static CNT: AtomicU64 = AtomicU64::new(0);
        let id = CNT.fetch_add(1, Ordering::SeqCst);
        std::env::temp_dir().join(format!(
            "nanny-net-test-state-{}-{}",
            std::process::id(),
            id
        ))
    }

    // ── Tests ─────────────────────────────────────────────────────────────────

    #[test]
    fn raw_tcp_tls_handshake_works() {
        // Diagnostic: verifies that a real TCP mTLS handshake works WITHOUT
        // axum-server: pure tokio-rustls. If this passes but the axum-server
        // tests fail, axum-server is causing the issue.
        use rustls::pki_types::{CertificateDer, PrivateKeyDer, ServerName};

        let _ = rustls::crypto::ring::default_provider().install_default();

        let dir = test_certs_dir();
        gen_certs_for_test(&dir);

        let port = next_port();
        let addr_str = format!("127.0.0.1:{port}");

        // ── Build server TLS config (pure rustls, ring explicit) ──────────────
        let ca_pem = std::fs::read(dir.join("ca.crt")).unwrap();
        let srv_pem = std::fs::read(dir.join("server.crt")).unwrap();
        let srv_key = std::fs::read(dir.join("server.key")).unwrap();
        let cli_pem = std::fs::read(dir.join("client.crt")).unwrap();
        let cli_key = std::fs::read(dir.join("client.key")).unwrap();

        let provider = Arc::new(rustls::crypto::ring::default_provider());

        // Build server config
        let ca_der: Vec<CertificateDer<'static>> = rustls_pemfile::certs(&mut ca_pem.as_ref())
            .collect::<Result<Vec<_>, _>>()
            .unwrap();
        let mut srv_root = rustls::RootCertStore::empty();
        for c in ca_der.clone() {
            srv_root.add(c).unwrap();
        }

        let srv_verifier = rustls::server::WebPkiClientVerifier::builder_with_provider(
            Arc::new(srv_root),
            provider.clone(),
        )
        .build()
        .unwrap();

        let srv_certs: Vec<CertificateDer<'static>> = rustls_pemfile::certs(&mut srv_pem.as_ref())
            .collect::<Result<Vec<_>, _>>()
            .unwrap();
        let srv_private: PrivateKeyDer<'static> =
            rustls_pemfile::private_key(&mut srv_key.as_ref())
                .unwrap()
                .unwrap();

        let mut server_config = rustls::ServerConfig::builder_with_provider(provider.clone())
            .with_safe_default_protocol_versions()
            .unwrap()
            .with_client_cert_verifier(srv_verifier)
            .with_single_cert(srv_certs, srv_private)
            .unwrap();
        server_config.alpn_protocols = vec![b"http/1.1".to_vec()];

        // ── Build client TLS config ────────────────────────────────────────────
        let mut cli_root = rustls::RootCertStore::empty();
        for c in ca_der {
            cli_root.add(c).unwrap();
        }

        let cli_certs: Vec<CertificateDer<'static>> = rustls_pemfile::certs(&mut cli_pem.as_ref())
            .collect::<Result<Vec<_>, _>>()
            .unwrap();
        let cli_private: PrivateKeyDer<'static> =
            rustls_pemfile::private_key(&mut cli_key.as_ref())
                .unwrap()
                .unwrap();

        let client_config = rustls::ClientConfig::builder_with_provider(provider)
            .with_safe_default_protocol_versions()
            .unwrap()
            .with_root_certificates(cli_root)
            .with_client_auth_cert(cli_certs, cli_private)
            .unwrap();

        // ── Start a simple TCP+TLS echo server in background ──────────────────
        let srv_cfg = Arc::new(server_config);
        let addr_clone = addr_str.clone();
        std::thread::spawn(move || {
            let rt = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .unwrap();
            rt.block_on(async move {
                use tokio::net::TcpListener;
                let listener = TcpListener::bind(&addr_clone).await.unwrap();
                let acceptor = tokio_rustls::TlsAcceptor::from(srv_cfg);
                if let Ok((stream, _)) = listener.accept().await {
                    // Accept and immediately close: we just want the handshake
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
        assert!(
            result.is_ok(),
            "raw tokio-rustls mTLS handshake must succeed: {result:?}"
        );

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

        let ca_pem = std::fs::read(dir.join("ca.crt")).unwrap();
        let srv_pem = std::fs::read(dir.join("server.crt")).unwrap();
        let key_pem = std::fs::read(dir.join("server.key")).unwrap();

        use rustls::pki_types::{CertificateDer, PrivateKeyDer};
        let ca_der: Vec<CertificateDer<'static>> = rustls_pemfile::certs(&mut ca_pem.as_ref())
            .collect::<Result<Vec<_>, _>>()
            .unwrap();
        let srv_der: Vec<CertificateDer<'static>> = rustls_pemfile::certs(&mut srv_pem.as_ref())
            .collect::<Result<Vec<_>, _>>()
            .unwrap();
        let key: PrivateKeyDer<'static> = rustls_pemfile::private_key(&mut key_pem.as_ref())
            .unwrap()
            .unwrap();

        assert!(!ca_der.is_empty(), "CA cert must parse");
        assert!(!srv_der.is_empty(), "server cert must parse");

        let mut root_store = rustls::RootCertStore::empty();
        for cert in ca_der {
            root_store.add(cert).unwrap();
        }

        // Build a ServerConfig: proves cert+key are a valid pair.
        let server_cfg = rustls::ServerConfig::builder()
            .with_no_client_auth()
            .with_single_cert(srv_der.clone(), key)
            .expect("server cert+key must form a valid pair");

        // Build a ClientConfig that trusts the CA: proves the CA cert is usable.
        let client_cfg = rustls::ClientConfig::builder()
            .with_root_certificates(root_store)
            .with_no_client_auth();

        // Verify the server cert is trusted by the client's CA store.
        // We do this by doing a TLS handshake in memory using rustls directly.
        use rustls::pki_types::ServerName;
        let server_name = ServerName::try_from("localhost").unwrap().to_owned();
        let mut client_conn =
            rustls::ClientConnection::new(Arc::new(client_cfg), server_name).unwrap();
        let mut server_conn = rustls::ServerConnection::new(Arc::new(server_cfg)).unwrap();

        // Run the handshake in memory.
        let mut handshake_done = false;
        for _ in 0..20 {
            if !client_conn.wants_write()
                && !server_conn.wants_write()
                && !client_conn.is_handshaking()
                && !server_conn.is_handshaking()
            {
                handshake_done = true;
                break;
            }
            let mut buf = Vec::new();
            if client_conn.wants_write() {
                client_conn.write_tls(&mut buf).unwrap();
                server_conn
                    .read_tls(&mut std::io::Cursor::new(&buf))
                    .unwrap();
                server_conn.process_new_packets().unwrap();
            }
            let mut buf = Vec::new();
            if server_conn.wants_write() {
                server_conn.write_tls(&mut buf).unwrap();
                client_conn
                    .read_tls(&mut std::io::Cursor::new(&buf))
                    .unwrap();
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
        let state_dir = test_state_dir();
        let addr: SocketAddr = format!("0.0.0.0:{port}").parse().unwrap();
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
        let server_state_dir = state_dir.clone();
        std::thread::spawn(move || {
            NetworkServer::start_blocking(
                addr,
                cert2,
                key2,
                ca2,
                test_components(),
                Some(vec![token2]),
                100,
                server_state_dir,
            )
            .ok();
        });

        // Wait for the server to bind (poll instead of fixed sleep).
        let port = wait_for_bound_port(&state_dir);

        // Connect with valid client cert
        let ca_pem = std::fs::read(&ca).unwrap();
        let ca_cert = reqwest::Certificate::from_pem(&ca_pem).unwrap();
        let cert_pem = std::fs::read(&client_cert).unwrap();
        let key_pem = std::fs::read(&client_key).unwrap();
        let identity = reqwest::Identity::from_pem(&[cert_pem, key_pem].concat()).unwrap();

        let client = reqwest::blocking::Client::builder()
            .add_root_certificate(ca_cert)
            .identity(identity)
            .use_rustls_tls() // Identity::from_pem = rustls identity
            .danger_accept_invalid_hostnames(true) // test certs use "localhost"
            .timeout(CLIENT_TIMEOUT)
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

    /// end to end: a real `NetworkServer::start_blocking` run
    /// declares its own identity before anything else happens, so the cloud
    /// has a stable name/address/version even for a governor nobody ever
    /// POSTs an app or a tool call to.
    #[test]
    fn server_declares_its_own_identity_on_start() {
        let dir = test_certs_dir();
        gen_certs_for_test(&dir);

        let port = next_port();
        let state_dir = test_state_dir();
        // Non-loopback: loopback serves plain HTTP, and this test needs mTLS
        // to exercise the real `start_blocking` path.
        let addr: SocketAddr = format!("0.0.0.0:{port}").parse().unwrap();
        let cert = dir.join("server.crt");
        let key = dir.join("server.key");
        let ca = dir.join("ca.crt");
        let client_cert = dir.join("client.crt");
        let client_key = dir.join("client.key");
        let token = "test-server-token-governor".to_string();

        let cert2 = cert.clone();
        let key2 = key.clone();
        let ca2 = ca.clone();
        let token2 = token.clone();
        let server_state_dir = state_dir.clone();
        std::thread::spawn(move || {
            NetworkServer::start_blocking(
                addr,
                cert2,
                key2,
                ca2,
                test_components(),
                Some(vec![token2]),
                100,
                server_state_dir,
            )
            .ok();
        });
        let port = wait_for_bound_port(&state_dir);

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
            .timeout(CLIENT_TIMEOUT)
            .build()
            .unwrap();

        let events_text = client
            .get(format!("https://127.0.0.1:{port}/events"))
            .header("X-Nanny-Session-Token", &token)
            .send()
            .expect("events endpoint must respond")
            .text()
            .unwrap();

        let identified: Vec<serde_json::Value> = events_text
            .lines()
            .filter(|l| !l.trim().is_empty())
            .map(|l| serde_json::from_str(l).unwrap())
            .filter(|v: &serde_json::Value| v["event"] == "GovernorIdentified")
            .collect();
        assert_eq!(identified.len(), 1, "declared exactly once, at startup");
        assert_eq!(
            identified[0]["address"],
            format!("{addr}"),
            "must be the address it actually bound, not the one requested"
        );
        assert_eq!(identified[0]["version"], env!("CARGO_PKG_VERSION"));
        assert!(
            identified[0]["name"]
                .as_str()
                .is_some_and(|n| !n.is_empty()),
            "name must be present (falls back to \"unknown\", never blank)"
        );

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn missing_token_returns_401() {
        let dir = test_certs_dir();
        gen_certs_for_test(&dir);

        let port = next_port();
        let state_dir = test_state_dir();
        let addr: SocketAddr = format!("0.0.0.0:{port}").parse().unwrap();
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
        let server_state_dir = state_dir.clone();
        std::thread::spawn(move || {
            NetworkServer::start_blocking(
                addr,
                cert2,
                key2,
                ca2,
                test_components(),
                Some(vec![tok2]),
                100,
                server_state_dir,
            )
            .ok();
        });
        let port = wait_for_bound_port(&state_dir);

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
            .timeout(CLIENT_TIMEOUT)
            .build()
            .unwrap();

        // /status, not /health: liveness is served without the token on
        // purpose, so probing it here would assert the opposite of the design.
        let resp = client
            .get(format!("https://127.0.0.1:{port}/status"))
            // No token header → 401
            .send()
            .expect("request must complete");

        assert_eq!(resp.status(), 401);

        // And the exemption itself: a valid client certificate with no token
        // still gets liveness, which is what a container health check has.
        let health = client
            .get(format!("https://127.0.0.1:{port}/health"))
            .send()
            .expect("request must complete");
        assert_eq!(health.status(), 200, "/health is served without a token");
        assert!(
            !health.text().unwrap().contains("reason"),
            "/health must not disclose the stop reason"
        );

        std::fs::remove_dir_all(&dir).ok();
    }

    /// Poll TCP connect until the port is accepting connections (up to 10 s).
    /// Replaces fixed sleep(350ms): under heavy parallel test load, the fixed
    /// sleep is not enough.  Polling is both faster on a quiet machine and
    /// robust on a loaded one.
    ///
    /// The deadline is deliberately generous. It costs nothing on a healthy run
    /// (this returns as soon as the port answers) and only spends real time
    /// when something is actually wrong, so a slow CI box is not reported as a
    /// broken server. The old 3 s deadline was tight enough to lose a race
    /// against TLS setup on a loaded machine.
    /// The port the server actually bound, read from the address it recorded.
    ///
    /// Not the port the test asked for. `bind_with_fallforward` moves to the
    /// next free port when the requested one is taken, and under a parallel
    /// suite that is routine: `next_port` releases its listener before the
    /// server binds, so two tests can be handed the same port microseconds
    /// apart. Probing the requested port then succeeds against *another*
    /// test's server, and every assertion after it is made against the wrong
    /// one, which is why these failed only when run together.
    /// How long a test client waits for a response.
    ///
    /// Generous on purpose. Each of these tests stands up a real TLS server,
    /// and the suite runs them in parallel on every core, so a handshake
    /// competes with a hundred others for CPU. Five seconds was enough alone
    /// and not enough together, which is the whole reason three of these
    /// looked flaky: the failure was `TimedOut`, never a refused connection.
    const CLIENT_TIMEOUT: Duration = Duration::from_secs(30);

    fn wait_for_bound_port(state_dir: &Path) -> u16 {
        let addr_file = state_dir.join("server.addr");
        wait_for_file(&addr_file);
        let addr = std::fs::read_to_string(&addr_file).expect("server.addr must be readable");
        addr.trim()
            .rsplit(':')
            .next()
            .and_then(|p| p.parse().ok())
            .unwrap_or_else(|| panic!("server.addr must end in a port, got {addr:?}"))
    }

    fn wait_for_port(port: u16) {
        for _ in 0..200 {
            if std::net::TcpStream::connect(format!("127.0.0.1:{port}")).is_ok() {
                return;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        panic!("server on port {port} never became ready within 10 s");
    }

    /// Poll until `path` exists (up to 10 s).
    ///
    /// The server binds its listener *before* writing state files, so it can
    /// fall forward off a busy port and record the address it actually got.
    /// That means a TCP probe no longer implies the files are on disk, and a
    /// test that reads one has to wait for the file itself.
    ///
    /// Harmless in production: a joiner discovers a server by reading
    /// server.addr, so "not written yet" surfaces as a clean "no server address
    /// found" rather than a half-initialised connection.
    fn wait_for_file(path: &Path) {
        for _ in 0..200 {
            if path.exists() {
                return;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        panic!("{} never appeared within 10 s", path.display());
    }

    // ── Port fall-forward ─────────────────────────────────────────────────────

    #[test]
    fn fallforward_steps_past_a_busy_default_port() {
        // Occupy the default, then prove a second governor lands next door
        // rather than failing. Several governors on one dev box is normal.
        let Ok(held) = std::net::TcpListener::bind(default_governor_addr()) else {
            // A real governor already owns the default on this machine; the
            // behaviour under test is still covered by the cases below.
            return;
        };

        let listener = bind_with_fallforward(default_governor_addr())
            .expect("a busy default must fall forward, not fail");
        let got = listener.local_addr().unwrap();

        assert_ne!(
            got.port(),
            DEFAULT_GOVERNOR_PORT,
            "must not claim the held port"
        );
        assert!(
            got.port() > DEFAULT_GOVERNOR_PORT,
            "must step forward, not backward"
        );
        drop(held);
    }

    #[test]
    fn an_explicit_busy_address_is_an_error_not_a_silent_move() {
        // The important half. An explicitly named port is usually paired with a
        // firewall rule or reverse proxy pinned to it, so moving would leave a
        // governor running that nothing can reach.
        let port = next_port();
        let addr: SocketAddr = format!("127.0.0.1:{port}").parse().unwrap();
        let _held = std::net::TcpListener::bind(addr).expect("must hold the port");

        let err = bind_with_fallforward(addr).expect_err("an explicit busy address must fail");
        let msg = format!("{err:#}");
        assert!(msg.contains("already in use"), "must say why: {msg}");
        assert!(msg.contains("--addr"), "must point at the fix: {msg}");
    }

    #[test]
    fn a_free_address_binds_exactly_where_asked() {
        let port = next_port();
        let addr: SocketAddr = format!("127.0.0.1:{port}").parse().unwrap();
        let listener = bind_with_fallforward(addr).expect("a free port must bind");
        assert_eq!(
            listener.local_addr().unwrap(),
            addr,
            "no drift when the port is free"
        );
    }

    #[test]
    fn port_zero_lets_the_kernel_choose() {
        let addr: SocketAddr = "127.0.0.1:0".parse().unwrap();
        let listener = bind_with_fallforward(addr).expect("port 0 must bind");
        assert_ne!(
            listener.local_addr().unwrap().port(),
            0,
            "kernel must assign a real port"
        );
    }

    #[test]
    fn the_bound_listener_is_non_blocking() {
        // tokio refuses to register a blocking socket, and axum_server::from_tcp
        // *panics* rather than returning an error, so this is worth pinning.
        let addr: SocketAddr = "127.0.0.1:0".parse().unwrap();
        let listener = bind_with_fallforward(addr).unwrap();
        match listener.accept() {
            Err(e) => assert_eq!(
                e.kind(),
                std::io::ErrorKind::WouldBlock,
                "a non-blocking listener must not park the thread"
            ),
            Ok(_) => panic!("nothing should have connected to a just-bound test port"),
        }
    }

    #[test]
    fn no_client_cert_is_rejected_at_tls() {
        let dir = test_certs_dir();
        gen_certs_for_test(&dir);

        let port = next_port();
        let state_dir = test_state_dir();
        let addr: SocketAddr = format!("0.0.0.0:{port}").parse().unwrap();
        let token = "test-server-token-nocert".to_string();

        let cert = dir.join("server.crt");
        let key = dir.join("server.key");
        let ca = dir.join("ca.crt");

        let cert2 = cert.clone();
        let key2 = key.clone();
        let ca2 = ca.clone();
        let tok2 = token.clone();
        let server_state_dir = state_dir.clone();
        std::thread::spawn(move || {
            NetworkServer::start_blocking(
                addr,
                cert2,
                key2,
                ca2,
                test_components(),
                Some(vec![tok2]),
                100,
                server_state_dir,
            )
            .ok();
        });
        let port = wait_for_bound_port(&state_dir);

        // Connect WITHOUT a client cert: TLS handshake must fail
        let ca_pem = std::fs::read(&ca).unwrap();
        let ca_cert = reqwest::Certificate::from_pem(&ca_pem).unwrap();

        let client = reqwest::blocking::Client::builder()
            .add_root_certificate(ca_cert)
            .use_rustls_tls()
            .danger_accept_invalid_hostnames(true)
            .timeout(CLIENT_TIMEOUT)
            // No .identity(...): no client cert
            .build()
            .unwrap();

        let result = client
            .get(format!("https://127.0.0.1:{port}/health"))
            .header("X-Nanny-Session-Token", &token)
            .send();

        // Must fail: server requires client cert
        assert!(
            result.is_err(),
            "connection without client cert must be rejected"
        );

        std::fs::remove_dir_all(&dir).ok();
    }

    // ── Shared helpers (Day 8 + Day 9) ───────────────────────────────────────

    /// Build a blocking mTLS reqwest client that trusts the test CA and
    /// presents the test client cert.
    fn make_mtls_client(dir: &Path, _port: u16) -> reqwest::blocking::Client {
        let ca_pem = std::fs::read(dir.join("ca.crt")).unwrap();
        let ca_cert = reqwest::Certificate::from_pem(&ca_pem).unwrap();
        let cert_pem = std::fs::read(dir.join("client.crt")).unwrap();
        let key_pem = std::fs::read(dir.join("client.key")).unwrap();
        let identity = reqwest::Identity::from_pem(&[cert_pem, key_pem].concat()).unwrap();
        reqwest::blocking::Client::builder()
            .add_root_certificate(ca_cert)
            .identity(identity)
            .use_rustls_tls()
            .danger_accept_invalid_hostnames(true)
            .timeout(CLIENT_TIMEOUT)
            .build()
            .unwrap()
    }

    /// Start a server with full control over components, rps limit, and a
    /// returned `axum_server::Handle` so tests can trigger graceful shutdown.
    fn start_server_with_handle(
        components: BridgeComponents,
        port: u16,
        token: String,
        dir: &Path,
        rps: u32,
    ) -> axum_server::Handle<SocketAddr> {
        let addr: SocketAddr = format!("127.0.0.1:{port}").parse().unwrap();
        let cert = dir.join("server.crt");
        let key = dir.join("server.key");
        let ca = dir.join("ca.crt");

        let handle = axum_server::Handle::new();
        let handle_inner = handle.clone();
        let tok = token.clone();

        std::thread::spawn(move || {
            let _ = rustls::crypto::ring::default_provider().install_default();
            let tls_config = build_tls_config(&cert, &key, &ca).unwrap();
            let (template, registry) = init_run_template(components, tok.clone());
            let template = Arc::new(template);
            let runs: Arc<Mutex<HashMap<String, Arc<Mutex<BridgeState>>>>> =
                Arc::new(Mutex::new(HashMap::new()));
            runs.lock().unwrap().insert(
                DEFAULT_RUN_ID.to_string(),
                template.build_state(DEFAULT_RUN_ID),
            );
            let app = AppState {
                runs,
                template,
                registry,
                session_tokens: vec![tok],
                rate_limiter: RateLimiter::new(rps),
            };
            let rt = tokio::runtime::Builder::new_multi_thread()
                .enable_all()
                .build()
                .unwrap();
            let _ = rt.block_on(async move {
                let rc = axum_server::tls_rustls::RustlsConfig::from_config(Arc::new(tls_config));
                // Route through GovernorMakeService, not the router directly;
                // rate limiting and auth are enforced there now, not as router
                // layers (see GovernorService's doc comment). Using the router
                // alone here would silently skip both.
                let router = build_router(app.clone());
                axum_server::bind_rustls(addr, rc)
                    .handle(handle_inner)
                    .serve(GovernorMakeService { router, app })
                    .await
            });
        });
        wait_for_port(port);
        handle
    }

    /// BridgeComponents for cost-reporting tests. The parameter is vestigial:
    /// tokens are measured, never enforced.
    fn test_components_with_cost(_max_tokens: u64) -> BridgeComponents {
        BridgeComponents {
            registry: ToolRegistry::new(),
            allowed_tools: vec!["http_get".to_string()],
            per_tool_max_calls: HashMap::new(),
            tool_labels: Default::default(),
        }
    }

    /// BridgeComponents for the agent enter/exit tests.
    fn test_components_with_named_limit() -> BridgeComponents {
        BridgeComponents {
            registry: ToolRegistry::new(),
            allowed_tools: vec!["http_get".to_string()],
            per_tool_max_calls: HashMap::new(),
            tool_labels: Default::default(),
        }
    }

    // ── Day 8 tests ───────────────────────────────────────────────────────────

    #[test]
    fn rate_limit_fires_after_n_requests() {
        // Server allows 5 req/s per IP. Sending 7 rapid requests must produce
        // at least one 429 Too Many Requests.
        let dir = test_certs_dir();
        gen_certs_for_test(&dir);
        let port = next_port();
        let token = format!("rl-{port}");

        let _h = start_server_with_handle(test_components(), port, token.clone(), &dir, 5);

        let client = make_mtls_client(&dir, port);
        let base = format!("https://127.0.0.1:{port}");

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
        assert!(
            saw_429,
            "rate limiter must fire within 7 rapid requests (limit 5 req/s)"
        );

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn graceful_drain_stops_server_cleanly() {
        // Start server → confirm it responds → trigger graceful shutdown (2 s
        // drain) → wait for drain to finish → confirm new connections are refused.
        let dir = test_certs_dir();
        gen_certs_for_test(&dir);
        let port = next_port();
        let token = format!("drain-{port}");

        let handle = start_server_with_handle(test_components(), port, token.clone(), &dir, 100);

        let client = make_mtls_client(&dir, port);
        let base = format!("https://127.0.0.1:{port}");

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
        assert!(
            post.is_err(),
            "server must refuse connections after graceful shutdown"
        );

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn execution_stopped_returns_410_in_network_server() {
        // POST /stop → subsequent action endpoints must return 410 Gone.
        let dir = test_certs_dir();
        gen_certs_for_test(&dir);
        let port = next_port();
        let token = format!("stop410-{port}");

        let _h = start_server_with_handle(test_components(), port, token.clone(), &dir, 100);

        let client = make_mtls_client(&dir, port);
        let base = format!("https://127.0.0.1:{port}");

        // Stop the execution.
        let stop = client
            .post(format!("{base}/stop"))
            .header("X-Nanny-Session-Token", &token)
            .body("{}")
            .send()
            .expect("stop must reach server");
        assert_eq!(stop.status(), 200);

        // Action endpoints must return 410.
        for path in &["/tool/call", "/llm/usage", "/agent/enter", "/rule/evaluate"] {
            let resp = client
                .post(format!("{base}{path}"))
                .header("X-Nanny-Session-Token", &token)
                .body("{}")
                .send()
                .expect("post-stop request must complete");
            assert_eq!(
                resp.status(),
                410,
                "{path} must return 410 after execution stopped"
            );
        }

        std::fs::remove_dir_all(&dir).ok();
    }

    /// "Nanny stops the run, not the host": stopping one run must return
    /// 410 for that run only; other runs (and the server) keep working.
    #[test]
    fn stopping_one_run_does_not_affect_other_runs() {
        let dir = test_certs_dir();
        gen_certs_for_test(&dir);
        let port = next_port();
        let token = format!("runs-{port}");

        let _h = start_server_with_handle(test_components(), port, token.clone(), &dir, 100);

        let client = make_mtls_client(&dir, port);
        let base = format!("https://127.0.0.1:{port}");

        // Stop run "alpha" specifically.
        let stop = client
            .post(format!("{base}/stop"))
            .header("X-Nanny-Session-Token", &token)
            .header("X-Nanny-Run-Id", "alpha")
            .body(r#"{"reason":"ManualStop"}"#)
            .send()
            .expect("stop must reach server");
        assert_eq!(stop.status(), 200);

        // Run "alpha" is stopped → 410 carrying the typed reason.
        let a = client
            .post(format!("{base}/tool/call"))
            .header("X-Nanny-Session-Token", &token)
            .header("X-Nanny-Run-Id", "alpha")
            .body(r#"{"tool":"echo","args":{"message":"hi"}}"#)
            .send()
            .expect("alpha tool call must complete");
        assert_eq!(a.status(), 410, "the stopped run must return 410");
        let a_body: serde_json::Value = a.json().unwrap();
        assert_eq!(
            a_body["reason"], "ManualStop",
            "410 must carry the typed reason"
        );

        // Run "beta" is a different run → unaffected, tool call still allowed.
        let b = client
            .post(format!("{base}/tool/call"))
            .header("X-Nanny-Session-Token", &token)
            .header("X-Nanny-Run-Id", "beta")
            .body(r#"{"tool":"echo","args":{"message":"hi"}}"#)
            .send()
            .expect("beta tool call must complete");
        assert_eq!(
            b.status(),
            200,
            "a different run must keep working after another stops"
        );
        let b_body: serde_json::Value = b.json().unwrap();
        assert_eq!(b_body["status"], "allowed");

        // The default run (no run-id header) is also its own run → unaffected.
        let d = client
            .post(format!("{base}/tool/call"))
            .header("X-Nanny-Session-Token", &token)
            .body(r#"{"tool":"echo","args":{"message":"hi"}}"#)
            .send()
            .expect("default tool call must complete");
        assert_eq!(
            d.status(),
            200,
            "the default run must be unaffected by another run's stop"
        );

        std::fs::remove_dir_all(&dir).ok();
    }

    // ── Day 9 tests: shared state + agent scope + cross-client enforcement ──

    #[test]
    fn shared_state_across_clients() {
        // Two independent mTLS clients connect to ONE server. Every call they
        // make lands in the same execution state, so the token counter and the
        // call history accumulate across both of them rather than per-client.
        let dir = test_certs_dir();
        gen_certs_for_test(&dir);
        let port = next_port();
        let token = format!("shared-{port}");

        let _h = start_server_with_handle(
            test_components_with_cost(25),
            port,
            token.clone(),
            &dir,
            100,
        );

        let c1 = make_mtls_client(&dir, port);
        let c2 = make_mtls_client(&dir, port);
        let base = format!("https://127.0.0.1:{port}");

        macro_rules! tool_call {
            ($client:expr) => {
                $client
                    .post(format!("{}/tool/call", base))
                    .header("X-Nanny-Session-Token", &token)
                    .body(r#"{"tool":"http_get"}"#)
                    .send()
                    .expect("tool/call must reach server")
            };
        }

        // Alternate clients; every call is allowed and every call is counted.
        for (n, c) in [(1, &c1), (2, &c2), (3, &c1), (4, &c2)] {
            let r: serde_json::Value = tool_call!(c).json().unwrap();
            assert_eq!(r["status"], "allowed", "call {n} must be allowed");
        }

        let status: serde_json::Value = c1
            .get(format!("{base}/status"))
            .header("X-Nanny-Session-Token", &token)
            .send()
            .expect("status must reach server")
            .json()
            .unwrap();
        assert_eq!(
            status["tool_call_counts"]["http_get"], 4,
            "both clients must accumulate into one shared call count"
        );

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn agent_enter_exit_events_over_network() {
        // /agent/enter + /agent/exit round-trip over the network server.
        let dir = test_certs_dir();
        gen_certs_for_test(&dir);
        let port = next_port();
        let token = format!("agentev-{port}");

        let _h = start_server_with_handle(
            test_components_with_named_limit(),
            port,
            token.clone(),
            &dir,
            100,
        );

        let client = make_mtls_client(&dir, port);
        let base = format!("https://127.0.0.1:{port}");

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

        assert!(
            has_entered,
            "AgentScopeEntered must appear in event log\ngot: {events_text}"
        );
        assert!(
            has_exited,
            "AgentScopeExited must appear in event log\ngot: {events_text}"
        );

        std::fs::remove_dir_all(&dir).ok();
    }

    // ── Day 10 tests: security ────────────────────────────────────────────────

    #[test]
    fn client_cert_signed_by_wrong_ca_is_refused() {
        // mTLS defense: a client cert issued by a DIFFERENT CA must be rejected
        // at the TLS handshake: not at the session-token layer.
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

        let port = next_port();
        let state_dir = test_state_dir();
        let addr: SocketAddr = format!("0.0.0.0:{port}").parse().unwrap();
        let token = format!("wrong-ca-{port}");

        // Start server with CA-A certs.
        let cert = dir_a.join("server.crt");
        let key = dir_a.join("server.key");
        let ca = dir_a.join("ca.crt");
        let tok2 = token.clone();
        let server_state_dir = state_dir.clone();
        std::thread::spawn(move || {
            NetworkServer::start_blocking(
                addr,
                cert,
                key,
                ca,
                test_components(),
                Some(vec![tok2]),
                100,
                server_state_dir,
            )
            .ok();
        });
        let port = wait_for_bound_port(&state_dir);

        // Build a client that trusts CA-A but presents a cert signed by CA-B.
        let ca_pem_a = std::fs::read(dir_a.join("ca.crt")).unwrap();
        let ca_cert_a = reqwest::Certificate::from_pem(&ca_pem_a).unwrap();
        let cert_pem_b = std::fs::read(dir_b.join("client.crt")).unwrap();
        let key_pem_b = std::fs::read(dir_b.join("client.key")).unwrap();
        let bad_identity = reqwest::Identity::from_pem(&[cert_pem_b, key_pem_b].concat()).unwrap();

        let bad_client = reqwest::blocking::Client::builder()
            .add_root_certificate(ca_cert_a)
            .identity(bad_identity)
            .use_rustls_tls()
            .danger_accept_invalid_hostnames(true)
            .timeout(CLIENT_TIMEOUT)
            .build()
            .unwrap();

        let result = bad_client
            .get(format!("https://127.0.0.1:{port}/health"))
            .header("X-Nanny-Session-Token", &token)
            .send();

        // TLS handshake must fail: server rejects the CA-B client cert.
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
        // missing session token must return 401: not 200.
        //
        // This verifies that the token check is independent of mTLS: passing
        // the TLS layer does not bypass the session-token gate.
        let dir = test_certs_dir();
        gen_certs_for_test(&dir);
        let port = next_port();
        let state_dir = test_state_dir();
        let addr: SocketAddr = format!("0.0.0.0:{port}").parse().unwrap();
        let correct_token = format!("correct-{port}");

        let cert = dir.join("server.crt");
        let key = dir.join("server.key");
        let ca = dir.join("ca.crt");
        let tok2 = correct_token.clone();
        let server_state_dir = state_dir.clone();
        std::thread::spawn(move || {
            NetworkServer::start_blocking(
                addr,
                cert,
                key,
                ca,
                test_components(),
                Some(vec![tok2]),
                100,
                server_state_dir,
            )
            .ok();
        });
        let port = wait_for_bound_port(&state_dir);

        // Valid client cert: TLS succeeds.
        let client = make_mtls_client(&dir, port);

        // Wrong token → must get 401 (token check fires after TLS). Probed on
        // /status: /health is deliberately exempt from the token check.
        let resp = client
            .get(format!("https://127.0.0.1:{port}/status"))
            .header("X-Nanny-Session-Token", "not-the-right-token")
            .send()
            .expect("request must reach server (TLS succeeds)");

        assert_eq!(
            resp.status(),
            401,
            "valid cert + wrong token must return 401"
        );

        std::fs::remove_dir_all(&dir).ok();
    }

    // ── Loopback plain HTTP path ──────────────────────────────────────
    //
    // These tests cover the branch introduced in this session:
    // `addr.ip().is_loopback()` → `axum_server::bind` (plain HTTP, no certs).
    //
    // All existing integration tests use mTLS even on loopback. This group is
    // the only coverage for the new path.
    //
    // Cert paths are dummies: they are never read on the loopback branch.

    /// Start a plain-HTTP (loopback) server in a background thread.
    /// Returns the bound port and the (test-scratch) state dir it wrote its
    /// token/pid files into. Token is the caller-supplied string.
    fn start_plain_http_server(token: &str) -> (u16, PathBuf) {
        start_plain_http_server_with(vec![token.to_string()])
    }

    fn start_plain_http_server_with(tokens: Vec<String>) -> (u16, PathBuf) {
        let port = next_port();
        let addr: SocketAddr = format!("127.0.0.1:{port}").parse().unwrap();
        let toks = tokens;
        let state_dir = test_state_dir();
        let thread_state_dir = state_dir.clone();
        std::thread::spawn(move || {
            NetworkServer::start_blocking(
                addr,
                // Cert paths are never read for loopback: pass nonexistent paths.
                PathBuf::from("/dev/null/nanny-test-dummy.crt"),
                PathBuf::from("/dev/null/nanny-test-dummy.key"),
                PathBuf::from("/dev/null/nanny-test-dummy-ca.crt"),
                test_components(),
                Some(toks),
                100,
                thread_state_dir,
            )
            .ok();
        });
        wait_for_port(port);
        // The listener binds before the state files are written (so the server
        // can fall forward and record the port it actually got), so a TCP probe
        // alone races them. Wait for the file callers actually read.
        wait_for_file(&state_dir.join("server.token"));
        (port, state_dir)
    }

    /// Plain HTTP client: no TLS, no certs.
    fn plain_http_client() -> reqwest::blocking::Client {
        reqwest::blocking::Client::builder()
            .timeout(CLIENT_TIMEOUT)
            .build()
            .unwrap()
    }

    // GET /health returns {"state":"running"} over plain HTTP.
    // Proves the loopback branch binds and serves correctly without any TLS setup.
    #[test]
    fn loopback_plain_http_health_returns_running() {
        let token = format!("plain-health-{}", next_port());
        let (port, _state_dir) = start_plain_http_server(&token);
        let client = plain_http_client();

        let resp = client
            .get(format!("http://127.0.0.1:{port}/health"))
            .header("X-Nanny-Session-Token", &token)
            .send()
            .expect("plain HTTP GET /health must succeed");

        assert_eq!(
            resp.status(),
            200,
            "loopback plain HTTP must return 200 on /health"
        );
        let body: serde_json::Value = resp.json().unwrap();
        assert_eq!(
            body["state"], "running",
            "health response must carry state=running; got: {body}"
        );
    }

    #[test]
    fn the_startup_line_shows_a_fingerprint_not_the_token() {
        // A container writes this to stdout on every boot. The token is the
        // one thing that admits a process to the governor, so the log gets
        // enough to recognise it and not enough to use it.
        let token = "34e6beeed95fe4e954bc23b9193b9fda538483aca53caec3d9003ae3d69e8fad";
        let shown = token_fingerprint(token);

        assert!(!shown.contains(token), "the full token must never be printed");
        assert!(shown.starts_with("34e6beee"), "recognisable from the start: {shown}");
        assert!(shown.contains("8fad"), "and from the end: {shown}");
        assert!(shown.contains("(64 chars)"), "length is part of the check: {shown}");

        // At the 32-character floor, at least 20 stay unseen. Counted off the
        // token part alone: the trailing "(32 chars)" is full of characters
        // that are also hex digits, which is what made the first version of
        // this assertion measure the label rather than the secret.
        let shortest = "0123456789abcdef0123456789abcdef";
        let shown = token_fingerprint(shortest);
        let token_part = shown.split(" (").next().unwrap();
        let revealed = token_part.chars().filter(|c| *c != '…').count();
        assert_eq!(revealed, 12, "8 from the head and 4 from the tail: {shown}");
        assert!(
            shortest.len() - revealed >= 20,
            "at the floor, at least 20 characters stay unseen: {shown}"
        );
    }

    #[test]
    fn either_token_in_the_set_authenticates() {
        // The overlap a rotation lives in: the governor holds the outgoing and
        // incoming tokens at once, so joiners can be rolled one at a time
        // instead of everything restarting at the same instant.
        let old = format!("rotate-old-{}", next_port());
        let new = format!("rotate-new-{}", next_port());
        let (port, _state_dir) = start_plain_http_server_with(vec![old.clone(), new.clone()]);
        let client = plain_http_client();

        for token in [&old, &new] {
            let resp = client
                .get(format!("http://127.0.0.1:{port}/status"))
                .header("X-Nanny-Session-Token", token)
                .send()
                .expect("request must reach server");
            assert_eq!(resp.status(), 200, "token {token} must authenticate");
        }

        // And a token that was never in the set still does not.
        let resp = client
            .get(format!("http://127.0.0.1:{port}/status"))
            .header("X-Nanny-Session-Token", "never-issued")
            .send()
            .expect("request must reach server");
        assert_eq!(resp.status(), 401);
    }

    #[test]
    fn an_unwritable_state_dir_serves_anyway() {
        // Those files exist for same-machine discovery, which a joiner on
        // another host never uses. A governor that was given its address and
        // token refusing to start because it cannot write them down would make
        // a read-only root filesystem, a standard hardening baseline,
        // incompatible with running one at all.
        let token = format!("readonly-{}", next_port());
        let unwritable = std::env::temp_dir()
            .join(format!("nanny-ro-{}", next_port()))
            .join("locked");
        std::fs::create_dir_all(unwritable.parent().unwrap()).unwrap();
        // A file where the state directory should be: create_dir_all fails.
        std::fs::write(unwritable.parent().unwrap().join("locked"), b"not a dir").unwrap();

        let port = next_port();
        let addr: SocketAddr = format!("127.0.0.1:{port}").parse().unwrap();
        let tok = token.clone();
        let dir = unwritable.clone();
        std::thread::spawn(move || {
            NetworkServer::start_blocking(
                addr,
                PathBuf::from("/dev/null/nanny-test-dummy.crt"),
                PathBuf::from("/dev/null/nanny-test-dummy.key"),
                PathBuf::from("/dev/null/nanny-test-dummy-ca.crt"),
                test_components(),
                Some(vec![tok]),
                100,
                dir,
            )
            .ok();
        });

        let client = plain_http_client();
        let deadline = Instant::now() + Duration::from_secs(10);
        let mut served = false;
        while Instant::now() < deadline {
            if let Ok(resp) = client.get(format!("http://127.0.0.1:{port}/health")).send() {
                if resp.status() == 200 {
                    served = true;
                    break;
                }
            }
            std::thread::sleep(Duration::from_millis(100));
        }
        assert!(served, "the governor must serve despite an unwritable state dir");

        std::fs::remove_dir_all(unwritable.parent().unwrap()).ok();
    }

    // Wrong token returns 401 over plain HTTP.
    // Auth is the session token, not TLS: must still enforce on plain HTTP path.
    #[test]
    fn loopback_plain_http_wrong_token_returns_401() {
        let token = format!("plain-auth-{}", next_port());
        let (port, _state_dir) = start_plain_http_server(&token);
        let client = plain_http_client();

        let resp = client
            .get(format!("http://127.0.0.1:{port}/status"))
            .header("X-Nanny-Session-Token", "not-the-right-token")
            .send()
            .expect("request must reach server even with wrong token");

        assert_eq!(
            resp.status(),
            401,
            "wrong token on plain HTTP must return 401"
        );
    }

    // POST /tool/call allows an allowlisted tool over plain HTTP.
    // Proves enforcement is active, not just that the socket binds.
    #[test]
    fn loopback_plain_http_tool_call_allowed() {
        let token = format!("plain-tool-{}", next_port());
        let (port, _state_dir) = start_plain_http_server(&token);
        let client = plain_http_client();

        let resp = client
            .post(format!("http://127.0.0.1:{port}/tool/call"))
            .header("X-Nanny-Session-Token", &token)
            .body(r#"{"tool":"echo"}"#)
            .send()
            .expect("POST /tool/call must reach plain HTTP server");

        assert_eq!(
            resp.status(),
            200,
            "allowed tool on plain HTTP must return 200"
        );
        let body: serde_json::Value = resp.json().unwrap();
        assert_eq!(
            body["status"], "allowed",
            "an allowlisted tool must be allowed; got: {body}"
        );
    }

    // POST /stop → action endpoints return 410 over plain HTTP.
    // Proves the execution-stopped gate works on the plain HTTP code path.
    #[test]
    fn loopback_plain_http_410_after_stop() {
        let token = format!("plain-stop-{}", next_port());
        let (port, _state_dir) = start_plain_http_server(&token);
        let client = plain_http_client();
        let base = format!("http://127.0.0.1:{port}");

        // Stop the execution.
        let stop = client
            .post(format!("{base}/stop"))
            .header("X-Nanny-Session-Token", &token)
            .body(r#"{"reason":"ManualStop"}"#)
            .send()
            .expect("/stop must reach plain HTTP server");
        assert_eq!(stop.status(), 200, "/stop must return 200");

        // All action endpoints must now return 410.
        for path in &["/tool/call", "/llm/usage", "/agent/enter", "/rule/evaluate"] {
            let resp = client
                .post(format!("{base}{path}"))
                .header("X-Nanny-Session-Token", &token)
                .body("{}")
                .send()
                .expect("post-stop request must reach server");
            assert_eq!(
                resp.status(),
                410,
                "{path} must return 410 after execution stopped on plain HTTP path"
            );
        }
    }

    // Shared state across two plain-HTTP clients.
    // Two independent clients hit the same enforcement state, so their calls
    // accumulate into one token count rather than one per client.
    #[test]
    fn loopback_plain_http_shared_state_across_clients() {
        let token = format!("plain-shared-{}", next_port());
        let port = {
            let p = next_port();
            let addr: SocketAddr = format!("127.0.0.1:{p}").parse().unwrap();
            let tok = token.clone();
            std::thread::spawn(move || {
                NetworkServer::start_blocking(
                    addr,
                    PathBuf::from("/dev/null/dummy.crt"),
                    PathBuf::from("/dev/null/dummy.key"),
                    PathBuf::from("/dev/null/dummy-ca.crt"),
                    test_components_with_cost(25),
                    Some(vec![tok]),
                    100,
                    test_state_dir(),
                )
                .ok();
            });
            wait_for_port(p);
            p
        };

        let c1 = plain_http_client();
        let c2 = plain_http_client();
        let base = format!("http://127.0.0.1:{port}");

        macro_rules! tool_call {
            ($client:expr) => {
                $client
                    .post(format!("{}/tool/call", base))
                    .header("X-Nanny-Session-Token", &token)
                    .body(r#"{"tool":"http_get"}"#)
                    .send()
                    .expect("tool call must reach server")
            };
        }

        for (n, c) in [(1, &c1), (2, &c2), (3, &c1), (4, &c2)] {
            let r = tool_call!(c);
            assert_eq!(r.status(), 200, "call {n} must succeed");
            let b: serde_json::Value = r.json().unwrap();
            assert_eq!(b["status"], "allowed", "call {n} must be allowed");
        }

        let status: serde_json::Value = c1
            .get(format!("{base}/status"))
            .header("X-Nanny-Session-Token", &token)
            .send()
            .expect("status must reach server")
            .json()
            .unwrap();
        assert_eq!(
            status["tool_call_counts"]["http_get"], 4,
            "both clients must accumulate into one shared call count"
        );
    }

    // ── /status field contract ───────────────────────────────────────────
    //
    // GET /status is what the Python SDK reads to populate PolicyContext.
    // The bridge-to-SDK field mapping is a documented contract:
    //   bridge "tokens_spent"  → SDK "tokens_spent"
    // A regression here breaks @rule evaluation silently.

    #[test]
    fn status_returns_correct_fields_after_tool_call() {
        let token = format!("status-fields-{}", next_port());
        let (port, _state_dir) = start_plain_http_server(&token);
        let client = plain_http_client();
        let base = format!("http://127.0.0.1:{port}");

        // Make one tool call so the counters are non-zero.
        client
            .post(format!("{base}/tool/call"))
            .header("X-Nanny-Session-Token", &token)
            .body(r#"{"tool":"echo","args":{"x":"y"}}"#)
            .send()
            .expect("tool call must succeed");

        let resp = client
            .get(format!("{base}/status"))
            .header("X-Nanny-Session-Token", &token)
            .send()
            .expect("GET /status must succeed");

        assert_eq!(resp.status(), 200);
        let body: serde_json::Value = resp.json().unwrap();

        // These are the exact field names the Python SDK reads.
        assert!(
            body["tokens_spent"].is_number(),
            "/status must have numeric 'tokens_spent' field; got: {body}"
        );
        assert!(
            body["elapsed_ms"].is_number(),
            "/status must have numeric 'elapsed_ms' field; got: {body}"
        );
        assert!(
            body["tool_call_counts"].is_object(),
            "/status must have object 'tool_call_counts' field; got: {body}"
        );
        assert!(
            body["tool_call_history"].is_array(),
            "/status must have array 'tool_call_history' field; got: {body}"
        );
        assert!(
            body["tool_labels"].is_object(),
            "/status must have object 'tool_labels' field; got: {body}"
        );

        // Verify the values reflect the call we just made.
        assert_eq!(
            body["tokens_spent"], 0,
            "a tool call measures no tokens; got: {body}"
        );
        assert!(
            body["tool_call_counts"]["echo"].as_u64().unwrap_or(0) >= 1,
            "tool_call_counts must count the echo call; got: {body}"
        );
        let history = body["tool_call_history"].as_array().unwrap();
        assert!(
            history.iter().any(|v| v == "echo"),
            "tool_call_history must include 'echo'; got: {body}"
        );
    }

    // ── repeated tool calls accumulate in /status ───────────────────────

    #[test]
    fn repeated_tool_calls_accumulate_in_status() {
        let token = format!("call-count-{}", next_port());
        let (port, _state_dir) = start_plain_http_server(&token);
        let client = plain_http_client();
        let base = format!("http://127.0.0.1:{port}");

        // POST /tool/call twice.
        for _ in 0..2 {
            let resp = client
                .post(format!("{base}/tool/call"))
                .header("X-Nanny-Session-Token", &token)
                .body(r#"{"tool":"echo","args":{}}"#)
                .send()
                .expect("POST /tool/call must succeed");
            assert_eq!(resp.status(), 200, "POST /tool/call must return 200");
        }

        let status: serde_json::Value = client
            .get(format!("{base}/status"))
            .header("X-Nanny-Session-Token", &token)
            .send()
            .expect("GET /status must succeed")
            .json()
            .unwrap();

        assert_eq!(
            status["tool_call_counts"]["echo"], 2,
            "both calls must be counted; got: {status}"
        );
        assert_eq!(
            status["tool_call_history"].as_array().unwrap().len(),
            2,
            "both calls must be in history; got: {status}"
        );
    }

    // ── Tool call events in network server ───────────────────────────

    // POST /tool/call emits ToolAllowed in /events.
    #[test]
    fn tool_call_emits_tool_allowed_event_in_network_server() {
        let token = format!("ev-allowed-{}", next_port());
        let (port, _state_dir) = start_plain_http_server(&token);
        let client = plain_http_client();
        let base = format!("http://127.0.0.1:{port}");

        client
            .post(format!("{base}/tool/call"))
            .header("X-Nanny-Session-Token", &token)
            .body(r#"{"tool":"echo"}"#)
            .send()
            .expect("tool call must reach server");

        let events = client
            .get(format!("{base}/events"))
            .header("X-Nanny-Session-Token", &token)
            .send()
            .expect("GET /events must succeed")
            .text()
            .unwrap();

        let has_allowed = events.lines().any(|l| {
            serde_json::from_str::<serde_json::Value>(l)
                .map(|v| v["event"] == "ToolAllowed")
                .unwrap_or(false)
        });
        assert!(
            has_allowed,
            "ToolAllowed event must appear after a successful tool call\ngot: {events}"
        );
    }

    // POST /tool/call for a denied tool emits ToolDenied in /events.
    #[test]
    fn tool_call_denied_emits_tool_denied_event_in_network_server() {
        let token = format!("ev-denied-{}", next_port());
        let (port, _state_dir) = start_plain_http_server(&token);
        let client = plain_http_client();
        let base = format!("http://127.0.0.1:{port}");

        // "not_allowed_tool" is not in the allowed_tools list ("echo" only in test_components).
        client
            .post(format!("{base}/tool/call"))
            .header("X-Nanny-Session-Token", &token)
            .body(r#"{"tool":"not_allowed_tool"}"#)
            .send()
            .expect("tool call must reach server");

        let events = client
            .get(format!("{base}/events"))
            .header("X-Nanny-Session-Token", &token)
            .send()
            .expect("GET /events must succeed")
            .text()
            .unwrap();

        let has_denied = events.lines().any(|l| {
            serde_json::from_str::<serde_json::Value>(l)
                .map(|v| v["event"] == "ToolDenied")
                .unwrap_or(false)
        });
        assert!(
            has_denied,
            "ToolDenied event must appear after a denied tool call\ngot: {events}"
        );
    }

    // ── Token file permissions ───────────────────────────────────────────
    // Verifies <state_dir>/server.token is written with mode 0o600 (Unix only).

    #[cfg(unix)]
    #[test]
    fn server_token_file_has_restrictive_permissions() {
        use std::os::unix::fs::PermissionsExt;

        let token = format!("tokenperm-{}", next_port());
        let (port, state_dir) = start_plain_http_server(&token);
        let _ = port; // server is running, token file has been written

        let token_file = state_dir.join("server.token");

        assert!(
            token_file.exists(),
            "server.token must exist in the state dir after server start"
        );

        let mode = std::fs::metadata(&token_file)
            .expect("must read token file metadata")
            .permissions()
            .mode();

        // 0o600 = owner read+write only. No group or world bits.
        let group_world_bits = mode & 0o077;
        assert_eq!(
            group_world_bits, 0,
            "server.token must not be group- or world-readable; mode was 0o{mode:o}"
        );
    }

    // ── Rate limiter window reset ────────────────────────────────────────

    #[test]
    fn rate_limiter_recovers_after_window_reset() {
        // Server allows 3 req/s per IP. Send 5 requests fast (must see ≥1 429).
        // Then wait 1.1s for the window to reset.
        // Then send 3 more requests: all must succeed (window reset, fresh count).
        let dir = test_certs_dir();
        gen_certs_for_test(&dir);
        let port = next_port();
        let token = format!("rl-reset-{port}");

        let _h = start_server_with_handle(test_components(), port, token.clone(), &dir, 3);

        let client = make_mtls_client(&dir, port);
        let base = format!("https://127.0.0.1:{port}");

        // Exhaust the window: at least one 429 expected.
        let mut saw_429 = false;
        for _ in 0..5 {
            let resp = client
                .get(format!("{base}/health"))
                .header("X-Nanny-Session-Token", &token)
                .send()
                .expect("health request must reach server");
            if resp.status() == 429 {
                saw_429 = true;
            }
        }
        assert!(saw_429, "rate limiter must fire 429 when limit is exceeded");

        // Wait for a new window (1.1 s > 1 s window).
        std::thread::sleep(Duration::from_millis(1100));

        // All three requests must now succeed (fresh window).
        for i in 0..3 {
            let resp = client
                .get(format!("{base}/health"))
                .header("X-Nanny-Session-Token", &token)
                .send()
                .expect("health must succeed after window reset");
            assert_eq!(
                resp.status(),
                200,
                "request {i} after window reset must return 200, rate limiter must have reset"
            );
        }

        std::fs::remove_dir_all(&dir).ok();
    }

    //
    // is_blocked_host unit tests prove the logic. These integration tests prove
    // the handler actually calls is_blocked_host before forwarding.

    // ── In-flight request completes before graceful drain ────────────────
    //
    // graceful_drain_stops_server_cleanly proves new connections are refused
    // after drain. This test proves the drain window actually works: a request
    // received during the drain window must return 200, not a connection error.
    //
    // Invariant: if a response arrives, it must be well-formed (200). If we
    // lose the race to the shutdown, a connection error is also acceptable.
    // A corrupt or non-200 response during the drain window is the failure mode.

    #[test]
    fn in_flight_request_completes_before_drain_deadline() {
        let dir = test_certs_dir();
        gen_certs_for_test(&dir);
        let port = next_port();
        let token = format!("inflight-{port}");

        let handle = start_server_with_handle(test_components(), port, token.clone(), &dir, 100);
        let client = make_mtls_client(&dir, port);
        let base = format!("https://127.0.0.1:{port}");

        // Confirm server is up before the test begins.
        let pre = client
            .get(format!("{base}/health"))
            .header("X-Nanny-Session-Token", &token)
            .send()
            .expect("pre-drain health must succeed");
        assert_eq!(pre.status(), 200, "server must be healthy before drain");

        // Trigger graceful shutdown with a 2s drain window.
        handle.graceful_shutdown(Some(Duration::from_secs(2)));

        // Send a request immediately after triggering drain. The race:
        //   Win → request received during drain → server returns 200 (not error)
        //   Lose → connection refused (server already closed) → Err(_) is fine
        // The assertion is: IF we get a response, it must be 200.
        let during_drain = client
            .get(format!("{base}/health"))
            .header("X-Nanny-Session-Token", &token)
            .send();
        match during_drain {
            Ok(resp) => assert_eq!(
                resp.status(),
                200,
                "a response received during the drain window must be 200, not an error"
            ),
            Err(_) => { /* connection refused, we lost the race, acceptable */ }
        }

        // Wait for the drain window to expire.
        std::thread::sleep(Duration::from_secs(3));

        // All new connections must now be refused.
        let post = client
            .get(format!("{base}/health"))
            .header("X-Nanny-Session-Token", &token)
            .send();
        assert!(
            post.is_err(),
            "server must refuse all connections after graceful drain expires"
        );

        std::fs::remove_dir_all(&dir).ok();
    }
}
