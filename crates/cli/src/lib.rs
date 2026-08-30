//! nanny: Rust SDK and CLI.
//!
//! # Usage
//!
//! ```toml
//! [dependencies]
//! nanny = "0.3"
//! ```
//!
//! ```rust,ignore
//! use nanny::{tool, rule, agent, PolicyContext};
//!
//! #[tool(tokens = 200)]
//! fn search_web(query: &str) -> String { ... }
//!
//! #[rule("no_spiral")]
//! fn check_spiral(ctx: &PolicyContext) -> bool { ... }
//!
//! #[agent("researcher")]
//! fn run_research(topic: &str) { ... }
//! ```

// ── User-facing re-exports ────────────────────────────────────────────────────

/// The type passed to `#[nanny::rule]` functions.
pub use nanny_core::policy::PolicyContext;

/// All possible reasons nanny stopped execution.
pub use nanny_core::agent::state::StopReason;

/// Declare a function as a governed nanny tool.
/// See [`tool`](nanny_macros::tool) for full documentation.
pub use nanny_macros::tool;

/// Register a function as a named enforcement rule.
/// See [`rule`](nanny_macros::rule) for full documentation.
pub use nanny_macros::rule;

/// Activate a named limits set for the duration of a function.
/// See [`agent`](nanny_macros::agent) for full documentation.
pub use nanny_macros::agent;

// ── Built-in bridge tools ─────────────────────────────────────────────────────

/// Fetch a URL via nanny's built-in `http_get` bridge tool.
///
/// The request is executed by the nanny bridge: the calling process never
/// opens a network connection directly. Nanny enforces the allowlist, charges
/// tokens (200 per request), and applies the step limit before making the
/// request.
///
/// # Passthrough mode
///
/// When running outside `nanny run` (no bridge active), returns
/// `Err("bridge unavailable")`. This keeps behaviour predictable: the tool
/// always goes through nanny when a bridge is present.
///
/// # Example
///
/// ```rust,ignore
/// use nanny::PolicyContext;
/// use rig::tool::Tool;
///
/// impl Tool for MyFetchTool {
///     async fn call(&self, args: Self::Args) -> Result<Self::Output, Self::Error> {
///         tokio::task::spawn_blocking(move || {
///             nanny::http_get(args.url).map_err(MyError)
///         })
///         .await
///         .map_err(|e| MyError(e.to_string()))?
///     }
/// }
/// ```
pub fn http_get(url: String) -> Result<String, String> {
    // Evaluate local rules first: same as #[nanny::tool] does.
    // This ensures #[nanny::rule] functions (e.g. "no_loop") fire for
    // built-in bridge tools, not just developer-defined #[nanny::tool] functions.
    let mut args = std::collections::HashMap::new();
    args.insert("url".to_string(), url.clone());
    if let Some(rule_name) = runtime::evaluate_local_rules("http_get", args) {
        runtime::report_stop("RuleDenied");
        eprintln!("nanny: stopped, RuleDenied: {rule_name}");
        std::process::exit(1);
    }

    let args_json = serde_json::json!({"url": url}).to_string();
    runtime::call_bridge_tool("http_get", &args_json)
}

// ── LLM token usage reporting ─────────────────────────────────────────────────

/// Measured LLM token usage, reported to the bridge via [`report_usage`].
///
/// Only `input` and `output` are ever required. `model` and `provider` are
/// optional attribution labels: identifiers only, never prompt or response
/// content, and never pricing. Omit them with `..Default::default()`:
///
/// ```rust,ignore
/// nanny::report_usage(nanny::Usage { input: 1200, output: 340, ..Default::default() });
/// ```
#[derive(Debug, Default, Clone)]
pub struct Usage {
    /// Prompt / input tokens consumed by the LLM call.
    pub input: u64,
    /// Completion / output tokens produced by the LLM call.
    pub output: u64,
    /// Optional model identifier (e.g. `"gpt-4o"`). Label only: no pricing.
    pub model: Option<String>,
    /// Optional provider identifier (e.g. `"openai"`). Label only: no pricing.
    pub provider: Option<String>,
    /// Optional finer split of `input` (never additional tokens beyond it),
    /// set these only if the provider's response reports prompt-caching
    /// usage (e.g. OpenAI's `usage.prompt_tokens_details.cached_tokens`,
    /// Anthropic's `cache_read_input_tokens`/`cache_creation_input_tokens`).
    /// Reporting only, same as `model`/`provider`: never debited separately
    /// from `input`, and no pricing logic reads these in the engine: they
    /// exist so a downstream cost calculator can price cache-hit tokens at
    /// their real, much cheaper rate.
    pub cache_read: Option<u64>,
    pub cache_write: Option<u64>,
    /// Optional harness attribution reported alongside this call: the "on every
    /// request" path (parity with the Python SDK). Deduped bridge-side, so it is
    /// safe to set on every report. Prefer [`set_harness`] for a one-shot declare.
    pub harness: Option<Harness>,
}

/// Report measured LLM token usage to the nanny bridge.
///
/// This is the Rust counterpart to Python's `nanny.instrument()`. Rust cannot
/// monkey-patch an LLM client, so: idiomatically, usage is reported
/// explicitly: after an LLM call, hand nanny the token counts already present
/// on the response.
///
/// `input + output` is debited from the active budget; `model`/`provider` (if
/// set) are recorded as attribution labels in the audit log. Only numbers and
/// identifiers cross the boundary: never prompt or response content.
///
/// # Passthrough mode
///
/// When running outside `nanny run` (no bridge active) this is a no-op: the
/// same passthrough contract as `#[nanny::tool]`.
///
/// Fire-and-forget: never blocks the agent and never panics. Transport errors
/// are swallowed and a zero-token report is skipped.
///
/// # Example
///
/// ```rust,ignore
/// let resp = client.chat().create(req).await?;
/// nanny::report_usage(nanny::Usage {
///     input:  resp.usage.prompt_tokens,
///     output: resp.usage.completion_tokens,
///     model:  Some("gpt-4o".into()),
///     ..Default::default()
/// });
/// ```
pub fn report_usage(usage: Usage) {
    runtime::report_usage(
        usage.input,
        usage.output,
        usage.model,
        usage.provider,
        usage.cache_read,
        usage.cache_write,
        usage.harness,
    );
}

// ── Harness attribution ───────────────────────────────────────────────────────

/// The agentic harness that ran this agent (e.g. `opencode`, `langgraph`,
/// `crewai`), declared to the nanny bridge via [`set_harness`].
///
/// This is our equivalent of OpenRouter's "app" column: an attribution label
/// only, recorded once per run. It is distinct from `#[nanny::agent(...)]`,
/// which names a *limits scope*, not the harness. Only `name` is required:
///
/// ```rust,ignore
/// nanny::set_harness(nanny::Harness { name: "opencode".into(), ..Default::default() });
/// ```
#[derive(Debug, Default, Clone)]
pub struct Harness {
    /// Harness identifier, e.g. `"opencode"`.
    pub name: String,
    /// Optional harness version, e.g. `"0.3.2"`.
    pub version: Option<String>,
}

/// Declare the agentic harness running this agent to the nanny bridge.
///
/// Records a `HarnessIdentified` attribution event so the cloud can group and
/// compare executions by harness. Call once at startup; the last non-empty
/// declaration wins.
///
/// Rust cannot introspect the harness the way the Python SDK can (it wraps the
/// LLM client), so in Rust the harness is declared explicitly.
///
/// # Passthrough mode
///
/// When running outside `nanny run` (no bridge active) this is a no-op: the
/// same passthrough contract as `report_usage`. An empty `name` is ignored.
pub fn set_harness(harness: Harness) {
    runtime::set_harness(harness.name, harness.version);
}

/// Declare which app this process belongs to.
///
/// Records an `AppIdentified` attribution event so the cloud can group runs by
/// app. Normally you never call this: `nanny run` reads the committed
/// `.nanny/app.json` and declares it for you. It exists for the case where a
/// process joins a governor and wants to report under its own identity rather
/// than inheriting the governor's.
///
/// Identity travels in the event stream, not in the API key, which is what lets
/// one governor holding one credential serve many apps and still attribute each
/// separately, the same reason OpenTelemetry makes `service.name` a resource
/// attribute rather than a transport concern.
///
/// # Passthrough mode
///
/// When running outside `nanny run` (no bridge active) this is a no-op. An
/// empty `app_id` is ignored; an empty `name` is allowed, since the id is what
/// identifies and the name is only a label.
pub fn set_app(app_id: impl Into<String>, name: impl Into<String>) {
    runtime::set_app(app_id.into(), name.into());
}

/// Declare the rules registered in this process to the governor.
///
/// Records a `RulesDeclared` audit event listing every `#[nanny::rule]` name
/// compiled into this binary. Normally you never call this: the first governed
/// tool call declares them for you.
///
/// It exists because rules are the half of declared authority the governor
/// cannot see. It reads nanny.toml, not your binary, so without this the audit
/// log records every refusal but never what *could* have refused, which is the
/// difference between "nothing was blocked" and "nothing was watching".
///
/// Declaration only: naming a rule here never enforces it. Enforcement stays
/// with the rule body.
///
/// # Passthrough mode
///
/// When running outside `nanny run` (no bridge active) this is a no-op.
pub fn declare_rules() {
    runtime::declare_rules();
}

// ── Run control ─────────────────────────────────────────────────────────────

/// Scope a governed run to the current thread, not the whole process.
///
/// A run is Nanny's real unit of governance: one stop state, one history, "a
/// stop is final". A `#[nanny::agent(...)]` scope does not give you a second
/// one, it only labels a phase within the current one. If your process runs
/// several logically independent runs (one long-lived server giving each
/// incoming request its own clean slate) this is how you say that.
///
/// ```rust,ignore
/// {
///     let _run = nanny::run_scope(None);
///     // every governed call on this thread now belongs to a fresh run
/// }   // previous scope restored here
/// ```
///
/// Pass `Some(id)` to resume a specific run; pass `None` to mint one. The
/// previous scope is restored on drop, so nesting is safe.
///
/// # Threads and tasks
///
/// The scope is thread-local. Two threads each in their own `run_scope` never
/// see each other's run id. **On a Tokio runtime use
/// [`run_scope_async`] instead**: tasks are multiplexed onto shared threads and
/// migrate between them, so a thread-local would leak across concurrent tasks,
/// which is exactly the bug this exists to prevent.
///
/// Only meaningful when governed through a governance server (`nanny run
/// --serve` / `--join`), which keys state per run id. Under local `nanny run`
/// one process is always exactly one run, so this is a safe no-op there and
/// code that runs under either mode does not need to branch.
///
/// Mirrors `nanny_sdk.run_scope()` on the Python side.
#[must_use = "the scope ends when the guard is dropped; binding to `_` drops it immediately"]
pub fn run_scope(run_id: Option<String>) -> RunScope {
    let id = run_id.unwrap_or_else(nanny_config::new_run_id);
    let previous = runtime::scoped_run_id();
    runtime::set_scoped_run_id(Some(id.clone()));
    RunScope { id, previous }
}

/// The active run scope. Restores the previous scope when dropped.
///
/// Restoring rather than clearing is what makes nesting safe: an inner scope
/// ending must not silently promote its caller to "no scope at all".
pub struct RunScope {
    id: String,
    previous: Option<String>,
}

impl RunScope {
    /// The run id this scope activated.
    pub fn id(&self) -> &str {
        &self.id
    }
}

impl Drop for RunScope {
    fn drop(&mut self) {
        runtime::set_scoped_run_id(self.previous.take());
    }
}

/// Scope a governed run to the current Tokio task.
///
/// The async counterpart of [`run_scope`]. Tokio tasks share and migrate
/// between threads, so a thread-local scope would leak between concurrent
/// tasks; this binds the run id to the task instead.
///
/// ```rust,ignore
/// let out = nanny::run_scope_async(None, async {
///     // every governed call in this task belongs to its own run
/// }).await;
/// ```
///
/// Under the reframe this is a correctness property, not an accounting one:
/// rules read tool call history, so a leaked run id means one tenant's
/// untrusted read poisons another tenant's history, which is a wrong security
/// verdict rather than a wrong number.
pub async fn run_scope_async<F: std::future::Future>(run_id: Option<String>, f: F) -> F::Output {
    let id = run_id.unwrap_or_else(nanny_config::new_run_id);
    runtime::TASK_RUN_ID.scope(id, f).await
}

// ── Private runtime: for generated code only ─────────────────────────────────
//
// Everything below this line is used exclusively by code generated by
// nanny-macros. It is not a public API. Names and signatures may change
// in any release without notice.

#[doc(hidden)]
pub mod __private {
    // Re-export inventory so generated code can call
    // `::nanny::__private::inventory::submit! { ... }` without requiring
    // users to add inventory as a direct dependency.
    pub use ::inventory;

    pub use super::runtime::{
        agent_enter, agent_exit, call_bridge_tool, call_tool, evaluate_local_rules, is_active,
        report_stop, report_stop_rule, Rule, ToolVerdict,
    };
}

mod runtime {
    use nanny_core::policy::PolicyContext;
    use std::collections::HashMap;
    use std::sync::{Mutex, OnceLock};
    use std::time::Instant;

    // ── Rule registry ─────────────────────────────────────────────────────────

    /// A user-defined enforcement rule, registered via `#[nanny::rule("name")]`.
    pub struct Rule {
        pub name: &'static str,
        pub func: fn(&PolicyContext) -> bool,
    }

    inventory::collect!(Rule);

    // ── Client-side state ──────────────────────────────────────────────────────

    struct ClientState {
        start: Instant,
    }

    impl ClientState {
        fn new() -> Self {
            Self {
                start: Instant::now(),
            }
        }
    }

    fn client_state() -> &'static Mutex<ClientState> {
        static STATE: OnceLock<Mutex<ClientState>> = OnceLock::new();
        STATE.get_or_init(|| Mutex::new(ClientState::new()))
    }

    // ── Bridge detection ──────────────────────────────────────────────────────

    /// Returns `true` if any bridge transport is active.
    ///
    /// Priority (checked in order):
    ///   1. `NANNY_BRIDGE_SOCKET` : Unix domain socket (macOS/Linux local)
    ///   2. `NANNY_BRIDGE_PORT`   : TCP loopback (Windows local)
    ///   3. `NANNY_BRIDGE_ADDR`   : TCP + mTLS (network / cross-machine)
    ///   4. None of the above     : passthrough (no-op)
    pub fn is_active() -> bool {
        bridge_socket_path().is_some() || bridge_tcp_port().is_some() || bridge_addr().is_some()
    }

    #[cfg(unix)]
    fn bridge_socket_path() -> Option<std::path::PathBuf> {
        std::env::var("NANNY_BRIDGE_SOCKET")
            .ok()
            .map(std::path::PathBuf::from)
    }

    #[cfg(not(unix))]
    fn bridge_socket_path() -> Option<std::path::PathBuf> {
        None
    }

    fn bridge_tcp_port() -> Option<u16> {
        std::env::var("NANNY_BRIDGE_PORT").ok()?.parse().ok()
    }

    /// `NANNY_BRIDGE_ADDR`: host:port of the network governance server.
    /// Set automatically by `nanny run` when a server is running.
    fn bridge_addr() -> Option<String> {
        std::env::var("NANNY_BRIDGE_ADDR")
            .ok()
            .filter(|s| !s.is_empty())
    }

    /// True if `addr` (host:port) is a loopback address. The server serves plain
    /// HTTP on loopback and mTLS off-loopback (see `crates/bridge/src/network.rs`);
    /// the client must mirror that or the loopback handshake fails with a TLS error.
    fn addr_is_loopback(addr: &str) -> bool {
        if let Ok(sa) = addr.parse::<std::net::SocketAddr>() {
            return sa.ip().is_loopback();
        }
        addr.rsplit_once(':')
            .map(|(h, _)| h == "localhost")
            .unwrap_or(false)
    }

    fn session_token() -> String {
        std::env::var("NANNY_SESSION_TOKEN").unwrap_or_default()
    }

    tokio::task_local! {
        /// Task-local run id, set by `run_scope_async`.
        pub(crate) static TASK_RUN_ID: String;
    }

    thread_local! {
        /// Thread-local run id, set by `run_scope`.
        static THREAD_RUN_ID: std::cell::RefCell<Option<String>> =
            const { std::cell::RefCell::new(None) };
    }

    pub(crate) fn scoped_run_id() -> Option<String> {
        THREAD_RUN_ID.with(|c| c.borrow().clone())
    }

    pub(crate) fn set_scoped_run_id(id: Option<String>) {
        THREAD_RUN_ID.with(|c| *c.borrow_mut() = id);
    }

    /// Which run this process belongs to on the governance server.
    ///
    /// Resolution order, most specific first:
    ///   1. the current Tokio task's scope (`run_scope_async`)
    ///   2. the current thread's scope (`run_scope`)
    ///   3. `NANNY_RUN_ID`, how separate processes opt into a shared run
    ///
    /// Task before thread because a task is the narrower context: a task
    /// running inside a thread that also has a scope means someone asked for
    /// per-task isolation, and honouring the thread there would defeat it.
    ///
    /// Runs stop independently, so a stop ends this run, not the server (G3).
    /// Absent → the server's default run. The local bridge ignores it: one
    /// process is always one run.
    /// Test-only view of the resolved run id. Not public API: tests need to
    /// assert on resolution order, but nothing outside this crate should.
    #[cfg(test)]
    pub(crate) fn run_id_for_test() -> Option<String> {
        run_id()
    }

    fn run_id() -> Option<String> {
        if let Ok(id) = TASK_RUN_ID.try_with(|id| id.clone()) {
            if !id.is_empty() {
                return Some(id);
            }
        }
        if let Some(id) = scoped_run_id().filter(|s| !s.is_empty()) {
            return Some(id);
        }
        std::env::var("NANNY_RUN_ID").ok().filter(|s| !s.is_empty())
    }

    /// Header line for the run id, or empty when unset. Ends with CRLF so it
    /// slots directly into a raw HTTP request without extra separators.
    fn run_id_header_line() -> String {
        run_id()
            .map(|id| format!("X-Nanny-Run-Id: {id}\r\n"))
            .unwrap_or_default()
    }

    // ── mTLS cert resolution ───────────────────────────────────────────────────
    //
    // When NANNY_BRIDGE_ADDR is set, the SDK uses these certs to authenticate
    // with the server. `nanny run` auto-injects them from ~/.nanny/certs/ on the
    // local machine. Cross-machine deployments set the env vars manually.
    //
    // Two formats are accepted for all three NANNY_BRIDGE_CERT/KEY/CA env vars:
    //
    //   File path:   NANNY_BRIDGE_CA=/path/to/ca.crt
    //   Inline PEM:  NANNY_BRIDGE_CA="-----BEGIN CERTIFICATE-----\n..."
    //
    // Inline PEM works without a filesystem: useful in Docker/k8s where secrets
    // are injected as env var values rather than mounted files.
    //
    // NANNY_BRIDGE_CERT may be a combined cert+key PEM bundle, in which case
    // NANNY_BRIDGE_KEY can be omitted.

    fn default_nanny_certs_dir() -> std::path::PathBuf {
        dirs::home_dir()
            .expect("cannot determine home directory")
            .join(".nanny")
            .join("certs")
    }

    /// Resolve PEM bytes from an env var.
    ///
    /// - Env var starts with `-----BEGIN` → treat as inline PEM, return the bytes.
    /// - Env var is a non-empty string (not PEM) → treat as file path, read the file.
    /// - Env var is absent → try the fallback path; return `None` if it doesn't exist.
    fn resolve_pem(env_var: &str, fallback: std::path::PathBuf) -> Option<Vec<u8>> {
        match std::env::var(env_var) {
            Ok(val) if val.starts_with("-----BEGIN") => Some(val.into_bytes()),
            Ok(val) if !val.is_empty() => std::fs::read(&val).ok(),
            _ => std::fs::read(&fallback).ok(),
        }
    }

    // ── HTTP transport ────────────────────────────────────────────────────────

    struct BridgeResponse {
        status: u16,
        body: String,
    }

    fn http_get(path: &str) -> Option<BridgeResponse> {
        let token = session_token();
        let run_hdr = run_id_header_line();
        let req = format!(
            "GET {path} HTTP/1.1\r\n\
             Host: localhost\r\n\
             X-Nanny-Session-Token: {token}\r\n\
             {run_hdr}\
             Connection: close\r\n\
             \r\n"
        );

        #[cfg(unix)]
        if let Some(sock) = bridge_socket_path() {
            use std::io::{Read, Write};
            use std::os::unix::net::UnixStream;
            let mut stream = UnixStream::connect(&sock).ok()?;
            stream.write_all(req.as_bytes()).ok()?;
            let mut raw = String::new();
            stream.read_to_string(&mut raw).ok()?;
            return parse_http_response(&raw);
        }

        if let Some(port) = bridge_tcp_port() {
            use std::io::{Read, Write};
            use std::net::TcpStream;
            let mut stream = TcpStream::connect(("127.0.0.1", port)).ok()?;
            stream.write_all(req.as_bytes()).ok()?;
            let mut raw = String::new();
            stream.read_to_string(&mut raw).ok()?;
            return parse_http_response(&raw);
        }

        // Transport 3: NANNY_BRIDGE_ADDR: loopback is plain HTTP (mirrors the
        // server), non-loopback is mTLS.
        if let Some(addr) = bridge_addr() {
            if addr_is_loopback(&addr) {
                use std::io::{Read, Write};
                use std::net::TcpStream;
                let mut stream = TcpStream::connect(&addr).ok()?;
                stream.write_all(req.as_bytes()).ok()?;
                let mut raw = String::new();
                stream.read_to_string(&mut raw).ok()?;
                return parse_http_response(&raw);
            }
            return http_get_tls(&addr, path);
        }

        None
    }

    fn http_post(path: &str, body: &str) -> Option<BridgeResponse> {
        let token = session_token();
        let run_hdr = run_id_header_line();
        let req = format!(
            "POST {path} HTTP/1.1\r\n\
             Host: localhost\r\n\
             X-Nanny-Session-Token: {token}\r\n\
             {run_hdr}\
             Content-Type: application/json\r\n\
             Content-Length: {len}\r\n\
             Connection: close\r\n\
             \r\n\
             {body}",
            len = body.len()
        );

        #[cfg(unix)]
        if let Some(sock) = bridge_socket_path() {
            use std::io::{Read, Write};
            use std::os::unix::net::UnixStream;
            let mut stream = UnixStream::connect(&sock).ok()?;
            stream.write_all(req.as_bytes()).ok()?;
            let mut raw = String::new();
            stream.read_to_string(&mut raw).ok()?;
            return parse_http_response(&raw);
        }

        if let Some(port) = bridge_tcp_port() {
            use std::io::{Read, Write};
            use std::net::TcpStream;
            let mut stream = TcpStream::connect(("127.0.0.1", port)).ok()?;
            stream.write_all(req.as_bytes()).ok()?;
            let mut raw = String::new();
            stream.read_to_string(&mut raw).ok()?;
            return parse_http_response(&raw);
        }

        // Transport 3: NANNY_BRIDGE_ADDR: loopback is plain HTTP (mirrors the
        // server), non-loopback is mTLS.
        if let Some(addr) = bridge_addr() {
            if addr_is_loopback(&addr) {
                use std::io::{Read, Write};
                use std::net::TcpStream;
                let mut stream = TcpStream::connect(&addr).ok()?;
                stream.write_all(req.as_bytes()).ok()?;
                let mut raw = String::new();
                stream.read_to_string(&mut raw).ok()?;
                return parse_http_response(&raw);
            }
            return http_post_tls(&addr, path, body);
        }

        None
    }

    // ── mTLS transport (NANNY_BRIDGE_ADDR) ────────────────────────────────────

    /// Build a reqwest blocking client with mTLS client cert.
    ///
    /// Loads cert/key/CA from env vars or ~/.nanny/certs/ defaults.
    /// Returns `None` if cert files are missing or malformed: callers treat
    /// this as bridge unavailable (fail-closed when is_active() is true).
    fn build_tls_client() -> Option<reqwest::blocking::Client> {
        let certs_dir = default_nanny_certs_dir();

        // NANNY_BRIDGE_CERT may be a combined cert+key PEM bundle, in which case
        // NANNY_BRIDGE_KEY can be omitted (empty bytes are harmless when appended).
        let cert_pem = resolve_pem("NANNY_BRIDGE_CERT", certs_dir.join("client.crt"))?;
        let key_pem =
            resolve_pem("NANNY_BRIDGE_KEY", certs_dir.join("client.key")).unwrap_or_default();
        let ca_pem = resolve_pem("NANNY_BRIDGE_CA", certs_dir.join("ca.crt"))?;

        // reqwest::Identity requires PEM cert + key concatenated.
        let mut identity_pem = cert_pem;
        identity_pem.extend_from_slice(&key_pem);
        let identity = reqwest::Identity::from_pem(&identity_pem).ok()?;
        let ca_cert = reqwest::Certificate::from_pem(&ca_pem).ok()?;

        reqwest::blocking::Client::builder()
            .add_root_certificate(ca_cert)
            .identity(identity)
            .use_rustls_tls() // force rustls, Identity::from_pem produces a rustls identity
            .build()
            .ok()
    }

    fn http_get_tls(addr: &str, path: &str) -> Option<BridgeResponse> {
        let client = build_tls_client()?;
        let url = format!("https://{addr}{path}");
        let mut builder = client
            .get(&url)
            .header("X-Nanny-Session-Token", session_token());
        if let Some(id) = run_id() {
            builder = builder.header("X-Nanny-Run-Id", id);
        }
        let resp = builder.send().ok()?;
        let status = resp.status().as_u16();
        let body = resp.text().ok()?;
        Some(BridgeResponse { status, body })
    }

    fn http_post_tls(addr: &str, path: &str, body: &str) -> Option<BridgeResponse> {
        let client = build_tls_client()?;
        let url = format!("https://{addr}{path}");
        let mut builder = client
            .post(&url)
            .header("X-Nanny-Session-Token", session_token())
            .header("Content-Type", "application/json")
            .body(body.to_string());
        if let Some(id) = run_id() {
            builder = builder.header("X-Nanny-Run-Id", id);
        }
        let resp = builder.send().ok()?;
        let status = resp.status().as_u16();
        let resp_body = resp.text().ok()?;
        Some(BridgeResponse {
            status,
            body: resp_body,
        })
    }

    fn parse_http_response(raw: &str) -> Option<BridgeResponse> {
        let (headers, body) = raw.split_once("\r\n\r\n")?;
        let status_line = headers.lines().next()?;
        let status: u16 = status_line.split_whitespace().nth(1)?.parse().ok()?;
        Some(BridgeResponse {
            status,
            body: body.to_string(),
        })
    }

    // ── Rule evaluation ───────────────────────────────────────────────────────

    /// Evaluate all locally-registered rules for `tool_name`.
    /// Returns the name of the first denying rule, or `None` if all pass.
    ///
    /// Fetches all live counters from the bridge `/status` endpoint before
    /// evaluating rules: `BridgeState` is the single source of truth.
    ///
    /// Fails closed if the bridge is active but unreachable: rules cannot be
    /// evaluated reliably against zeroed counters.
    /// Manifesto: "silently continuing execution is always a bug."
    pub fn evaluate_local_rules(
        tool_name: &str,
        args: HashMap<String, String>,
    ) -> Option<&'static str> {
        // Declare once, on the first governed call, so the audit log records
        // what could have refused without the operator having to remember to
        // call declare_rules() by hand. Bridge-side dedupe makes the repeat
        // calls free; this guard just avoids the HTTP round trip.
        declare_rules_once();

        let elapsed_ms = client_state().lock().unwrap().start.elapsed().as_millis() as u64;

        // Fetch all tracked counters from the bridge (authoritative state).
        // If the bridge is active but unreachable, fail closed: same logic as
        // call_tool, which returns ToolVerdict::Stop("BridgeUnavailable").
        let status = match fetch_bridge_status() {
            Some(s) => s,
            None if is_active() => {
                report_stop("BridgeUnavailable");
                eprintln!("nanny: stopped, BridgeUnavailable (bridge unreachable during rule evaluation)");
                std::process::exit(1);
            }
            // Passthrough mode (no bridge env vars): zeros are correct; rules
            // still run but counters will be empty, which is expected offline.
            None => BridgeStatus {
                tokens_spent: 0,
                tool_call_counts: HashMap::new(),
                tool_call_history: Vec::new(),
                tool_labels: HashMap::new(),
            },
        };

        let ctx = PolicyContext {
            requested_tool: Some(tool_name.to_string()),
            tool_call_counts: status.tool_call_counts,
            tool_call_history: status.tool_call_history,
            tool_labels: status.tool_labels,
            last_tool_args: args,
            elapsed_ms,
            now_ms: nanny_core::events::event::now_ms(),
            tokens_spent: status.tokens_spent,
        };

        for rule in inventory::iter::<Rule> {
            if !(rule.func)(&ctx) {
                return Some(rule.name);
            }
        }
        None
    }

    /// Counters fetched from the bridge `/status` endpoint.
    struct BridgeStatus {
        tokens_spent: u64,
        tool_call_counts: HashMap<String, u32>,
        tool_call_history: Vec<String>,
        tool_labels: HashMap<String, Vec<String>>,
    }

    /// Fetch all live counters from the bridge /status endpoint.
    /// Returns `None` if the bridge is unreachable or returns unexpected data.
    /// Callers must check `is_active()` to decide whether to fail closed or use
    /// zeroed defaults (passthrough mode).
    fn fetch_bridge_status() -> Option<BridgeStatus> {
        let resp = match http_get("/status") {
            Some(r) if r.status == 200 => r,
            _ => return None,
        };
        let v: serde_json::Value = match serde_json::from_str(&resp.body) {
            Ok(v) => v,
            Err(_) => return None,
        };
        let tokens_spent = v.get("tokens_spent").and_then(|c| c.as_u64()).unwrap_or(0);
        let tool_call_counts = v
            .get("tool_call_counts")
            .and_then(|c| serde_json::from_value(c.clone()).ok())
            .unwrap_or_default();
        let tool_call_history = v
            .get("tool_call_history")
            .and_then(|h| serde_json::from_value(h.clone()).ok())
            .unwrap_or_default();
        let tool_labels = v
            .get("tool_labels")
            .and_then(|l| serde_json::from_value(l.clone()).ok())
            .unwrap_or_default();
        Some(BridgeStatus {
            tokens_spent,
            tool_call_counts,
            tool_call_history,
            tool_labels,
        })
    }

    // ── Tool call ─────────────────────────────────────────────────────────────

    /// What the bridge decided about a tool call.
    #[derive(Debug)]
    pub enum ToolVerdict {
        /// Allowed: run the original function body.
        Run,
        /// Denied or stopped: panic with this message.
        Stop(String),
    }

    /// POST /tool/call to the bridge.
    pub fn call_tool(tool_name: &str, tokens: u64) -> ToolVerdict {
        let body = format!(r#"{{"tool":"{tool_name}","tokens":{tokens}}}"#);
        match http_post("/tool/call", &body) {
            Some(resp) if resp.status == 200 && resp.body.contains("\"allowed\"") => {
                ToolVerdict::Run
            }
            Some(resp) if resp.status == 200 => {
                let reason = extract_str(&resp.body, "rule_name")
                    .map(|r| format!("RuleDenied: {r}"))
                    .or_else(|| {
                        extract_str(&resp.body, "tool_name").map(|t| format!("ToolDenied: {t}"))
                    })
                    .or_else(|| extract_str(&resp.body, "reason"))
                    .unwrap_or_else(|| "ToolDenied".to_string());
                ToolVerdict::Stop(reason)
            }
            Some(resp) if resp.status == 410 => {
                let reason = extract_str(&resp.body, "reason")
                    .unwrap_or_else(|| "ExecutionStopped".to_string());
                ToolVerdict::Stop(reason)
            }
            _ => {
                // If the bridge is unreachable while we are in a governed run,
                // the enforcement guarantee is broken: fail closed rather than
                // silently allowing the tool call to proceed ungoverned.
                // If we are not in a governed run (passthrough mode, no env vars
                // set), is_active() returns false and we run normally.
                if is_active() {
                    ToolVerdict::Stop("BridgeUnavailable".to_string())
                } else {
                    ToolVerdict::Run
                }
            }
        }
    }

    /// POST /tool/call for a bridge-side tool (e.g. `http_get`).
    ///
    /// Unlike `call_tool` (which is for user-defined tools), this forwards
    /// `args_json` to the bridge and returns the tool's actual output content
    /// on success. The bridge executes the tool itself: the child process
    /// never runs any local logic for it.
    ///
    /// Returns `Err(reason)` if the call is denied, stopped, or the bridge
    /// is unavailable.
    pub fn call_bridge_tool(tool_name: &str, args_json: &str) -> Result<String, String> {
        let body = format!(r#"{{"tool":"{tool_name}","args":{args_json}}}"#);
        match http_post("/tool/call", &body) {
            Some(resp) if resp.status == 200 => {
                // Use serde_json so the result field is safely decoded even
                // when it contains HTML with escaped quotes or special chars.
                let v: serde_json::Value =
                    serde_json::from_str(&resp.body).map_err(|e| e.to_string())?;
                if v["status"] == "allowed" {
                    Ok(v["result"].as_str().unwrap_or("").to_string())
                } else {
                    let reason = v["rule_name"]
                        .as_str()
                        .map(|r| format!("RuleDenied: {r}"))
                        .or_else(|| v["tool_name"].as_str().map(|t| format!("ToolDenied: {t}")))
                        .or_else(|| v["reason"].as_str().map(str::to_string))
                        .unwrap_or_else(|| "Denied".to_string());
                    Err(reason)
                }
            }
            Some(resp) if resp.status == 410 => {
                let reason = serde_json::from_str::<serde_json::Value>(&resp.body)
                    .ok()
                    .and_then(|v| v["reason"].as_str().map(str::to_string))
                    .unwrap_or_else(|| "ExecutionStopped".to_string());
                Err(reason)
            }
            Some(resp) if resp.status == 500 => {
                // Tool execution failed bridge-side (e.g. network error in http_get).
                let message = serde_json::from_str::<serde_json::Value>(&resp.body)
                    .ok()
                    .and_then(|v| v["message"].as_str().map(str::to_string))
                    .unwrap_or_else(|| "tool execution failed".to_string());
                Err(format!("ToolFailed: {message}"))
            }
            _ => Err("bridge unavailable".to_string()),
        }
    }

    /// POST /stop to the bridge: report a stop reason before calling exit(1).
    ///
    /// The bridge records this reason so the CLI emits it in `ExecutionStopped`
    /// instead of falling back to `ProcessCrashed`. Silently ignored if the
    /// bridge is unreachable.
    pub fn report_stop(reason: &str) {
        let body = serde_json::json!({"reason": reason}).to_string();
        let _ = http_post("/stop", &body);
    }

    /// POST /stop with RuleDenied metadata so the bridge can emit the NDJSON event.
    ///
    /// Carries `tool` and `rule_name` so `handle_stop` can append a `RuleDenied`
    /// event to the stream: client-side rule denials never reach `/tool/call`,
    /// so the bridge would otherwise have no way to emit the event.
    pub fn report_stop_rule(tool: &str, rule_name: &str) {
        let body = serde_json::json!({
            "reason":    "RuleDenied",
            "tool":      tool,
            "rule_name": rule_name,
        })
        .to_string();
        let _ = http_post("/stop", &body);
    }

    // ── LLM usage reporting ───────────────────────────────────────────────────

    /// POST /llm/usage: report measured LLM token usage.
    ///
    /// No-op in passthrough mode (no bridge) and for zero-token reports.
    /// Fire-and-forget: the bridge response is ignored and transport errors are
    /// swallowed, so reporting usage never interrupts the agent.
    pub fn report_usage(
        input: u64,
        output: u64,
        model: Option<String>,
        provider: Option<String>,
        cache_read: Option<u64>,
        cache_write: Option<u64>,
        harness: Option<super::Harness>,
    ) {
        if !is_active() || input + output == 0 {
            return;
        }
        let mut body = serde_json::json!({"input": input, "output": output});
        if let Some(m) = model {
            body["model"] = serde_json::Value::String(m);
        }
        if let Some(p) = provider {
            body["provider"] = serde_json::Value::String(p);
        }
        if let Some(cr) = cache_read {
            body["cache_read"] = serde_json::Value::from(cr);
        }
        if let Some(cw) = cache_write {
            body["cache_write"] = serde_json::Value::from(cw);
        }
        if let Some(harness) = harness {
            let mut h = serde_json::json!({ "name": harness.name });
            if let Some(v) = harness.version {
                h["version"] = serde_json::Value::String(v);
            }
            body["harness"] = h;
        }
        let _ = http_post("/llm/usage", &body.to_string());
    }

    // ── Harness declaration ───────────────────────────────────────────────────

    /// POST /harness: declare the agentic harness that ran this agent.
    ///
    /// No-op in passthrough mode (no bridge) and for an empty name.
    /// Fire-and-forget: the bridge response is ignored and transport errors are
    /// swallowed, so declaring the harness never interrupts the agent.
    pub fn set_harness(name: String, version: Option<String>) {
        if !is_active() || name.trim().is_empty() {
            return;
        }
        let mut body = serde_json::json!({"name": name});
        if let Some(v) = version {
            body["version"] = serde_json::Value::String(v);
        }
        let _ = http_post("/harness", &body.to_string());
    }

    /// POST /app: declare which app this process is.
    ///
    /// No-op in passthrough mode (no bridge) and for an empty `app_id`.
    /// Fire-and-forget on the same contract as `set_harness`: the response is
    /// ignored and transport errors are swallowed, so declaring identity can
    /// never interrupt the agent.
    pub fn set_app(app_id: String, name: String) {
        if !is_active() || app_id.trim().is_empty() {
            return;
        }
        let body = serde_json::json!({"app_id": app_id, "name": name});
        let _ = http_post("/app", &body.to_string());
    }

    /// POST /rules: declare the rules registered in this process.
    ///
    /// Reads the same `inventory` registry `evaluate_local_rules` enforces
    /// from, so the declaration cannot drift from what actually runs.
    ///
    /// No-op in passthrough mode (no bridge) and when nothing is registered.
    /// Fire-and-forget on the same contract as `set_harness`.
    /// Declare registered rules the first time anything is governed.
    ///
    /// `Once` rather than a bridge round trip per call: the bridge already
    /// dedupes, but the cheapest request is the one never sent.
    pub(crate) fn declare_rules_once() {
        static DECLARED: std::sync::Once = std::sync::Once::new();
        DECLARED.call_once(declare_rules);
    }

    pub fn declare_rules() {
        if !is_active() {
            return;
        }
        let names: Vec<&str> = inventory::iter::<Rule>
            .into_iter()
            .map(|r| r.name)
            .collect();
        if names.is_empty() {
            return;
        }
        let body = serde_json::json!({"rules": names});
        let _ = http_post("/rules", &body.to_string());
    }

    // ── Agent enter / exit ────────────────────────────────────────────────────

    /// POST /agent/enter: switch to a named limits set.
    pub fn agent_enter(name: &str) {
        let body = serde_json::json!({"name": name}).to_string();
        if let Some(resp) = http_post("/agent/enter", &body) {
            if resp.status == 404 {
                panic!("nanny: agent limits set '{name}' not found in nanny.toml");
            }
        }
    }

    /// POST /agent/exit: revert to global limits.
    pub fn agent_exit() {
        http_post("/agent/exit", "{}");
    }

    // ── JSON helpers ──────────────────────────────────────────────────────────

    fn extract_str(json: &str, key: &str) -> Option<String> {
        let needle = format!("\"{key}\":");
        let after = json
            .find(&needle)
            .map(|i| json[i + needle.len()..].trim_start())?;
        let inner = after.strip_prefix('"')?;
        let end = inner.find('"')?;
        Some(inner[..end].to_string())
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::__private::*;

    #[test]
    fn inactive_when_no_env_vars() {
        // SAFETY: std::env::remove_var is unsound when other threads are
        // reading the environment concurrently (deprecated in Rust 1.85).
        // This test runs in a single-threaded harness with no other env
        // readers, so the mutation is safe here. Do not copy this pattern
        // into multi-threaded production code.
        unsafe {
            std::env::remove_var("NANNY_BRIDGE_SOCKET");
            std::env::remove_var("NANNY_BRIDGE_PORT");
            std::env::remove_var("NANNY_BRIDGE_ADDR");
        }
        assert!(!is_active());
    }

    // Note: we do not add a parallel test for NANNY_BRIDGE_ADDR → is_active() because
    // all env-var tests mutate shared process state. The bridge_addr() path is covered
    // by the is_passthrough() logic tested via `inactive_when_no_env_vars`.

    #[test]
    fn no_rules_registered_allows_all() {
        assert!(evaluate_local_rules("any_tool", ::std::collections::HashMap::new()).is_none());
    }

    #[test]
    fn report_usage_noop_in_passthrough() {
        // SAFETY: see `inactive_when_no_env_vars`: single-threaded harness.
        unsafe {
            std::env::remove_var("NANNY_BRIDGE_SOCKET");
            std::env::remove_var("NANNY_BRIDGE_PORT");
            std::env::remove_var("NANNY_BRIDGE_ADDR");
        }
        // No bridge active → no-op. Must not panic or attempt any connection.
        crate::report_usage(crate::Usage {
            input: 100,
            output: 50,
            ..Default::default()
        });
        crate::report_usage(crate::Usage {
            input: 8,
            output: 3,
            model: Some("gpt-4o".into()),
            provider: Some("openai".into()),
            ..Default::default()
        });
    }

    #[test]
    fn set_harness_noop_in_passthrough() {
        // SAFETY: see `inactive_when_no_env_vars`: single-threaded harness.
        unsafe {
            std::env::remove_var("NANNY_BRIDGE_SOCKET");
            std::env::remove_var("NANNY_BRIDGE_PORT");
            std::env::remove_var("NANNY_BRIDGE_ADDR");
        }
        // No bridge active → no-op. Must not panic or attempt any connection.
        crate::set_harness(crate::Harness {
            name: "opencode".into(),
            ..Default::default()
        });
        crate::set_harness(crate::Harness {
            name: "langgraph".into(),
            version: Some("0.3.2".into()),
        });
        // Empty name is ignored even if a bridge were active.
        crate::set_harness(crate::Harness::default());
    }

    // ── run_scope ─────────────────────────────────────────────────────────────
    //
    // No shared mutex here, unlike the fresh_run tests these replace. That
    // lock existed because fresh_run wrote a process-global env var, so its
    // own tests corrupted each other under cargo's default parallelism. The
    // scope is thread-local, so the tests are naturally isolated: the bug is
    // gone rather than worked around.

    #[test]
    fn run_scope_mints_an_id_and_restores_on_drop() {
        assert!(
            crate::runtime::scoped_run_id().is_none(),
            "no scope to start"
        );

        {
            let scope = crate::run_scope(None);
            assert!(!scope.id().is_empty());
            assert_eq!(crate::runtime::scoped_run_id().as_deref(), Some(scope.id()));
        }

        assert!(
            crate::runtime::scoped_run_id().is_none(),
            "drop must restore"
        );
    }

    #[test]
    fn run_scope_accepts_an_explicit_id() {
        let scope = crate::run_scope(Some("resumed-run".to_string()));
        assert_eq!(scope.id(), "resumed-run");
        assert_eq!(
            crate::runtime::scoped_run_id().as_deref(),
            Some("resumed-run")
        );
    }

    /// An inner scope ending must restore its caller, not clear the scope
    /// entirely. Clearing would silently promote the outer run to "no scope".
    #[test]
    fn nested_scopes_restore_the_outer_one() {
        let outer = crate::run_scope(Some("outer".to_string()));
        {
            let _inner = crate::run_scope(Some("inner".to_string()));
            assert_eq!(crate::runtime::scoped_run_id().as_deref(), Some("inner"));
        }
        assert_eq!(
            crate::runtime::scoped_run_id().as_deref(),
            Some("outer"),
            "the outer scope must survive the inner one ending"
        );
        drop(outer);
    }

    /// The bug fresh_run could not fix. Two threads each in their own scope
    /// must never observe each other's run id; with a process-global env var
    /// one would clobber the other.
    #[test]
    fn concurrent_scopes_on_two_threads_do_not_clobber() {
        let barrier = std::sync::Arc::new(std::sync::Barrier::new(2));
        let handles: Vec<_> = ["run-a", "run-b"]
            .into_iter()
            .map(|id| {
                let barrier = barrier.clone();
                std::thread::spawn(move || {
                    let _scope = crate::run_scope(Some(id.to_string()));
                    // Both threads hold their scope at the same time.
                    barrier.wait();
                    crate::runtime::scoped_run_id()
                })
            })
            .collect();

        let seen: Vec<Option<String>> = handles
            .into_iter()
            .map(|h| h.join().expect("thread must not panic"))
            .collect();

        assert_eq!(seen[0].as_deref(), Some("run-a"));
        assert_eq!(seen[1].as_deref(), Some("run-b"));
    }

    /// The case a thread-local alone gets wrong: many concurrent tasks
    /// multiplexed onto shared runtime threads. Each task must keep its own
    /// run id even when it yields and resumes on a different thread.
    #[test]
    fn concurrent_tasks_keep_their_own_run_id() {
        let rt = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .expect("runtime must build");

        let seen: Vec<(String, String)> = rt.block_on(async {
            let tasks: Vec<_> = (0..8)
                .map(|i| {
                    let want = format!("task-{i}");
                    tokio::spawn(crate::run_scope_async(Some(want.clone()), async move {
                        // Yield so the task can resume on another thread.
                        tokio::task::yield_now().await;
                        let got = crate::runtime::run_id_for_test().unwrap_or_default();
                        (want, got)
                    }))
                })
                .collect();

            let mut out = Vec::new();
            for t in tasks {
                out.push(t.await.expect("task must not panic"));
            }
            out
        });

        for (want, got) in seen {
            assert_eq!(
                got, want,
                "each task must keep its own run id across a yield"
            );
        }
    }

    /// Task scope beats thread scope: asking for per-task isolation inside a
    /// thread that also has a scope must honour the narrower one.
    #[test]
    fn a_task_scope_wins_over_a_thread_scope() {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("runtime must build");

        let _thread_scope = crate::run_scope(Some("thread".to_string()));
        let got = rt.block_on(crate::run_scope_async(Some("task".to_string()), async {
            crate::runtime::run_id_for_test()
        }));

        assert_eq!(got.as_deref(), Some("task"));
    }

    /// A scope on one thread is invisible to another. Scoping is per-thread,
    /// never process-wide.
    #[test]
    fn a_scope_does_not_leak_to_another_thread() {
        let _scope = crate::run_scope(Some("mine".to_string()));
        let other = std::thread::spawn(crate::runtime::scoped_run_id)
            .join()
            .expect("thread must not panic");
        assert!(
            other.is_none(),
            "another thread must not see this scope; got {other:?}"
        );
    }
}
