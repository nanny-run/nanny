// nanny-bridge — local enforcement server + network bridge server.
pub mod network;

// nanny-bridge — local enforcement server.
//
// Runs as a background thread inside the `nanny run` process.
// The child process communicates with it over a Unix domain socket (macOS/Linux)
// or TCP loopback (Windows).
//
// Unix:    /tmp/nanny-<session-token>.sock — no port, no conflicts, ever
// Windows: 127.0.0.1:<dynamic-port>       — OS-assigned, loopback only
//
// Every request must carry the session token in `X-Nanny-Session-Token`.
// The token is a UUID v4 generated fresh for each execution.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use uuid::Uuid;

use nanny_core::events::event::ExecutionEvent;
use nanny_core::agent::state::StopReason;
use nanny_core::policy::{Policy, PolicyContext, PolicyDecision};
use nanny_core::tool::{ToolArgs, ToolCallError, ToolExecutor};
use nanny_runtime::{RuleEvaluator, ToolPermissionPolicy, ToolRegistry};

// ── Error ─────────────────────────────────────────────────────────────────────

#[derive(Debug, thiserror::Error)]
pub enum BridgeError {
    #[error("failed to start bridge: {0}")]
    Start(String),
}

// ── Execution state ───────────────────────────────────────────────────────────

/// The runtime state of the current execution.
///
/// `Running` until a limit fires or the child exits cleanly.
/// Once `Stopped`, action endpoints return 410 Gone.
#[derive(Debug, Clone, PartialEq)]
pub enum ExecutionState {
    Running,
    Stopped { reason: String },
}

/// Final accounting snapshot read by the CLI when writing `ExecutionStopped`.
#[derive(Debug, Clone, Default)]
pub struct BridgeMetrics {
    pub tokens_spent:  u64,
    /// Total number of tool calls made during execution.
    pub tool_call_count:  usize,
    /// Number of distinct tools that were allowed (configured in `[tools]`).
    pub allowed_tool_count: usize,
}

// ── BridgeAddress ─────────────────────────────────────────────────────────────

/// How the child process reaches the bridge.
///
/// On Unix (macOS / Linux): a Unix domain socket. No port, no conflicts.
///   Inject `NANNY_BRIDGE_SOCKET` into the child environment.
///   The socket path embeds the session token UUID, and Unix filesystem
///   permissions restrict access to the creating user — no extra auth needed
///   to connect, but the `X-Nanny-Session-Token` header is still required.
///
/// On Windows: TCP loopback on an OS-assigned port.
///   Inject `NANNY_BRIDGE_PORT` into the child environment.
///   **Security note:** any local process on the machine can attempt a TCP
///   connection to 127.0.0.1:<port>. The session token (a random UUID passed
///   via `NANNY_SESSION_TOKEN` and required as `X-Nanny-Session-Token` on
///   every request) is the sole authentication mechanism. The token is
///   visible to child processes spawned by the agent — do not spawn untrusted
///   sub-processes from within a governed agent on Windows.
///
/// In both cases inject `NANNY_SESSION_TOKEN`.
#[derive(Debug, Clone)]
pub enum BridgeAddress {
    /// Unix domain socket — macOS and Linux only.
    #[cfg(unix)]
    Unix(std::path::PathBuf),
    /// TCP port on 127.0.0.1 — Windows fallback.
    Tcp(u16),
}

// ── BridgeComponents ──────────────────────────────────────────────────────────

/// Configuration the CLI passes to `Bridge::start`.
pub struct BridgeComponents {
    pub registry: ToolRegistry,
    pub allowed_tools: Vec<String>,
    /// Per-tool max call counts from `[tools.<name>] max_calls`.
    pub per_tool_max_calls: HashMap<String, u32>,
    /// Operator-declared labels per tool, from `[tools.<name>]`.
    /// Carried into every `PolicyContext` so rules can reason about what a
    /// tool is rather than what it is called.
    pub tool_labels: HashMap<String, Vec<String>>,
}

// ── Internal state ────────────────────────────────────────────────────────────

pub(crate) struct BridgeState {
    session_token: String,
    execution: ExecutionState,

    // Enforcement — stored separately so /rule/evaluate can access
    // rule_evaluator directly without evaluating the full policy chain.
    tool_permission_policy: ToolPermissionPolicy,
    rule_evaluator: RuleEvaluator,

    // Agent context switching ─────────────────────────────────────────────────
    agent_name_stack: Vec<String>,
    allowed_tools: Vec<String>,
    tool_labels: HashMap<String, Vec<String>>,

    // Execution tracking ──────────────────────────────────────────────────────
    tokens_spent: u64,
    tool_call_counts: HashMap<String, u32>,
    tool_call_history: Vec<String>,
    start_time: std::time::Instant,

    // Append-only event log ───────────────────────────────────────────────────
    events: Vec<String>,

    // Last-recorded harness attribution `(name, version)`. Used to dedup
    // `HarnessIdentified`: the SDK may resend the harness on every LLM call, so
    // we only append an event when it actually changes.
    last_harness: Option<(String, Option<String>)>,
    last_rules: Option<Vec<String>>,

    // Last-recorded app attribution `(app_id, name)`. Dedups `AppIdentified`
    // the same way, so a caller may safely (re)declare its identity on every
    // request. Under `--serve` this changes as different apps join.
    last_app: Option<(String, String)>,
}

// ── Bridge ────────────────────────────────────────────────────────────────────

/// A running bridge instance.
///
/// Inject `address` and `session_token` into the child process environment
/// before spawning it. On Unix set `NANNY_BRIDGE_SOCKET`; on Windows set
/// `NANNY_BRIDGE_PORT`. Always set `NANNY_SESSION_TOKEN`.
pub struct Bridge {
    shared: Arc<Mutex<BridgeState>>,
    /// How the child process connects to the bridge.
    pub address: BridgeAddress,
    /// Session token the child process must present on every request.
    pub session_token: String,
}

impl Bridge {
    /// Start the bridge.
    ///
    /// On Unix, binds a Unix domain socket before returning — ready immediately.
    /// On Windows, binds a TCP loopback socket on an OS-assigned port.
    pub fn start(components: BridgeComponents) -> Result<Self, BridgeError> {
        let token = Uuid::new_v4().to_string();

        let tool_permission_policy =
            ToolPermissionPolicy::new(components.allowed_tools.clone());
        let rule_evaluator = RuleEvaluator::new(components.per_tool_max_calls);

        let shared = Arc::new(Mutex::new(BridgeState {
            session_token: token.clone(),
            execution: ExecutionState::Running,
            tool_permission_policy,
            rule_evaluator,
            agent_name_stack: Vec::new(),
            allowed_tools: components.allowed_tools,
            tool_labels: components.tool_labels,
            tokens_spent: 0,
            tool_call_counts: HashMap::new(),
            tool_call_history: Vec::new(),
            start_time: std::time::Instant::now(),
            events: Vec::new(),
            last_harness: None,
            last_rules: None,
            last_app: None,
        }));

        let registry = Arc::new(components.registry);

        start_transport(token, shared, registry)
    }
}

// ── Transport startup ─────────────────────────────────────────────────────────

#[cfg(unix)]
fn start_transport(
    token: String,
    shared: Arc<Mutex<BridgeState>>,
    registry: Arc<ToolRegistry>,
) -> Result<Bridge, BridgeError> {
    let socket_path = std::path::PathBuf::from(format!("/tmp/nanny-{}.sock", token));

    // Remove stale socket if present (shouldn't happen with UUID names).
    let _ = std::fs::remove_file(&socket_path);

    // Bind in the main thread — socket is ready before start() returns.
    let listener = std::os::unix::net::UnixListener::bind(&socket_path)
        .map_err(|e| BridgeError::Start(format!("socket bind failed: {e}")))?;

    {
        let shared = shared.clone();
        let registry = registry.clone();
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(mut s) = stream else { continue };
                let Some(req) = parse_http_request(&mut s) else { continue };
                let resp = dispatch(req, &shared, &registry);
                write_http_response(&mut s, &resp);
            }
        });
    }

    Ok(Bridge {
        shared,
        address: BridgeAddress::Unix(socket_path),
        session_token: token,
    })
}

#[cfg(not(unix))]
fn start_transport(
    token: String,
    shared: Arc<Mutex<BridgeState>>,
    registry: Arc<ToolRegistry>,
) -> Result<Bridge, BridgeError> {
    // Bind to port 0 — the OS assigns a free ephemeral port per bridge instance.
    // This supports concurrent `nanny run` processes on the same machine without
    // conflict. The actual bound port is read back via server_addr() and injected
    // into the child process environment as NANNY_BRIDGE_PORT — child processes
    // never hardcode the port themselves.
    let server = tiny_http::Server::http("127.0.0.1:0")
        .map_err(|e| BridgeError::Start(e.to_string()))?;

    let port = server
        .server_addr()
        .to_ip()
        .ok_or_else(|| BridgeError::Start("could not read bound TCP port".to_string()))?
        .port();

    {
        let shared = shared.clone();
        let registry = registry.clone();
        std::thread::spawn(move || serve_tcp(server, shared, registry));
    }

    Ok(Bridge {
        shared,
        address: BridgeAddress::Tcp(port),
        session_token: token,
    })
}

impl Bridge {

    /// Read the current execution state.
    pub fn execution_state(&self) -> ExecutionState {
        self.shared.lock().unwrap().execution.clone()
    }

    /// Return the tokens measured and tool calls made so far.
    ///
    /// Called by the CLI just before emitting `ExecutionStopped` so the event
    /// carries accurate accounting rather than hardcoded zeros.
    pub fn metrics(&self) -> BridgeMetrics {
        let guard = self.shared.lock().unwrap();
        BridgeMetrics {
            tokens_spent:       guard.tokens_spent,
            tool_call_count:    guard.tool_call_history.len(),
            allowed_tool_count: guard.allowed_tools.len(),
        }
    }

    /// Mark the execution as stopped with the given reason.
    ///
    /// Idempotent — calling twice does nothing after the first stop.
    pub fn stop(&self, reason: impl Into<String>) {
        let reason: String = reason.into();
        let mut guard = self.shared.lock().unwrap();
        mark_stopped(&mut guard, &reason);
    }

    /// Declare which app this execution belongs to, emitting `AppIdentified`.
    ///
    /// The in-process equivalent of `POST /app`: an inline `nanny run` owns its
    /// bridge directly, so it declares identity through this rather than making
    /// an HTTP call to itself. Deduped the same way, so calling it repeatedly is
    /// harmless. Attribution only, never affects enforcement.
    pub fn declare_app(&self, app_id: impl Into<String>, name: impl Into<String>) {
        let mut guard = self.shared.lock().unwrap();
        record_app(&mut guard, app_id.into(), name.into());
    }

    /// Drain all accumulated event lines from the bridge.
    ///
    /// Returns the raw NDJSON lines (one serialised JSON object each) in the
    /// order they were appended and clears the internal buffer.  The CLI calls
    /// this after execution ends and writes the lines to the event log before
    /// emitting `ExecutionStopped`, preserving the invariant that
    /// `ExecutionStopped` is always the final event.
    pub fn drain_events(&self) -> Vec<String> {
        let mut guard = self.shared.lock().unwrap();
        std::mem::take(&mut guard.events)
    }
}

impl Drop for Bridge {
    fn drop(&mut self) {
        // Clean up the socket file so it doesn't linger between runs.
        #[cfg(unix)]
        if let BridgeAddress::Unix(ref path) = self.address {
            let _ = std::fs::remove_file(path);
        }
    }
}

// ── Transport-agnostic request / response ─────────────────────────────────────

/// A parsed incoming request — transport-independent.
struct BridgeReq {
    method: String,
    path: String,
    /// The value of the `X-Nanny-Session-Token` header, if present.
    token: Option<String>,
    /// Raw request body bytes.
    body: Vec<u8>,
}

pub(crate) enum ContentType {
    Json,
    Ndjson,
}

pub(crate) struct BridgeResp {
    pub(crate) status: u16,
    pub(crate) body: String,
    pub(crate) content_type: ContentType,
}

impl BridgeResp {
    fn json(status: u16, body: impl Into<String>) -> Self {
        Self { status, body: body.into(), content_type: ContentType::Json }
    }

    fn ndjson(body: impl Into<String>) -> Self {
        Self { status: 200, body: body.into(), content_type: ContentType::Ndjson }
    }
}

// ── Dispatch ──────────────────────────────────────────────────────────────────

fn dispatch(
    req: BridgeReq,
    shared: &Arc<Mutex<BridgeState>>,
    registry: &Arc<ToolRegistry>,
) -> BridgeResp {
    // Token check — required on every request.
    let token_ok = {
        let guard = shared.lock().unwrap();
        req.token.as_deref() == Some(guard.session_token.as_str())
    };
    if !token_ok {
        return BridgeResp::json(401, r#"{"error":"Unauthorized"}"#);
    }

    let method = req.method.as_str();
    let path = req.path.as_str();

    // Read-only endpoints — always available, even after execution stops.
    match (method, path) {
        ("GET", "/health") => return handle_health(shared),
        ("GET", "/status") => return handle_status(shared),
        ("GET", "/events") => return handle_events(shared),
        _ => {}
    }

    // /stop — child reports its own stop reason before calling exit(1).
    // Always accepted, even after execution has already stopped (idempotent).
    if method == "POST" && path == "/stop" {
        return handle_stop(&req.body, shared);
    }

    // All other action endpoints return 410 Gone once execution has stopped.
    // The 410 carries the typed stop reason so the client reports the true cause.
    if let Some(reason) = stopped_reason(shared) {
        return stopped_response(&reason);
    }

    match (method, path) {
        ("POST", "/tool/call")     => handle_tool_call(&req.body, shared, registry),
        ("POST", "/rule/evaluate") => handle_rule_evaluate(&req.body, shared),
        ("POST", "/agent/enter")   => handle_agent_enter(&req.body, shared),
        ("POST", "/agent/exit")    => handle_agent_exit(shared),
        ("POST", "/llm/usage")     => handle_llm_usage(&req.body, shared),
        ("POST", "/harness")       => handle_harness(&req.body, shared),
        ("POST", "/rules")         => handle_rules(&req.body, shared),
        ("POST", "/app")           => handle_app(&req.body, shared),
        _                          => BridgeResp::json(404, r#"{"error":"Not Found"}"#),
    }
}

// ── Handlers (transport-agnostic) ─────────────────────────────────────────────

pub(crate) fn handle_health(shared: &Arc<Mutex<BridgeState>>) -> BridgeResp {
    let guard = shared.lock().unwrap();
    let body = match &guard.execution {
        ExecutionState::Running =>
            r#"{"state":"running"}"#.to_string(),
        ExecutionState::Stopped { reason } =>
            format!(r#"{{"state":"stopped","reason":"{}"}}"#, reason),
    };
    BridgeResp::json(200, body)
}

pub(crate) fn handle_status(shared: &Arc<Mutex<BridgeState>>) -> BridgeResp {
    let guard = shared.lock().unwrap();
    let elapsed_ms = guard.start_time.elapsed().as_millis() as u64;
    let counts_json = serde_json::to_string(&guard.tool_call_counts).unwrap_or_else(|_| "{}".to_string());
    let history_json = serde_json::to_string(&guard.tool_call_history).unwrap_or_else(|_| "[]".to_string());
    // Labels ride on /status because an out-of-process SDK has no other way to
    // learn them: it never reads nanny.toml, only the governor does.
    let labels_json = serde_json::to_string(&guard.tool_labels).unwrap_or_else(|_| "{}".to_string());
    let body = match &guard.execution {
        ExecutionState::Running => format!(
            r#"{{"state":"running","tokens_spent":{},"elapsed_ms":{},"tool_call_counts":{},"tool_call_history":{},"tool_labels":{}}}"#,
            guard.tokens_spent, elapsed_ms, counts_json, history_json, labels_json
        ),
        ExecutionState::Stopped { reason } => format!(
            r#"{{"state":"stopped","reason":"{}","tokens_spent":{},"elapsed_ms":{},"tool_call_counts":{},"tool_call_history":{},"tool_labels":{}}}"#,
            reason, guard.tokens_spent, elapsed_ms, counts_json, history_json, labels_json
        ),
    };
    BridgeResp::json(200, body)
}

pub(crate) fn handle_events(shared: &Arc<Mutex<BridgeState>>) -> BridgeResp {
    let guard = shared.lock().unwrap();
    BridgeResp::ndjson(guard.events.join("\n"))
}

/// Take and clear a run's buffered events, for cloud forwarding by the
/// governance server. Auth-free: the engine only hands the strings off; who (if
/// anyone) forwards them is decided above the engine, in `crates/cli`.
pub(crate) fn take_run_events(shared: &Arc<Mutex<BridgeState>>) -> Vec<String> {
    let mut guard = shared.lock().unwrap();
    std::mem::take(&mut guard.events)
}

pub(crate) fn handle_tool_call(
    body: &[u8],
    shared: &Arc<Mutex<BridgeState>>,
    registry: &Arc<ToolRegistry>,
) -> BridgeResp {
    let call: ToolCallRequest = match serde_json::from_slice(body) {
        Ok(b) => b,
        Err(_) => return BridgeResp::json(400, r#"{"error":"invalid request body"}"#),
    };

    // Build PolicyContext and evaluate — hold lock briefly, then release.
    let decision = {
        let guard = shared.lock().unwrap();
        let elapsed_ms = guard.start_time.elapsed().as_millis() as u64;
        let ctx = PolicyContext {
            elapsed_ms,
            requested_tool: Some(call.tool.clone()),
            tool_labels: guard.tool_labels.clone(),
            tokens_spent: guard.tokens_spent,
            tool_call_counts: guard.tool_call_counts.clone(),
            tool_call_history: guard.tool_call_history.clone(),
            last_tool_args: HashMap::new(),
        };
        // Chain: tool permission first, then per-tool rules.
        match guard.tool_permission_policy.evaluate(&ctx) {
            PolicyDecision::Allow => guard.rule_evaluator.evaluate(&ctx),
            deny => deny,
        }
    };

    match decision {
        PolicyDecision::Deny { ref reason } => {
            let reason_name = stop_reason_name(reason).to_string();
            {
                let mut guard = shared.lock().unwrap();
                // Emit the correct event variant based on why the tool was denied:
                // ToolDenied  = allowlist violation (ToolPermissionPolicy, fires first)
                // RuleDenied  = rule or max_calls violation (RuleEvaluator, fires after allowlist)
                let event = match reason {
                    StopReason::RuleDenied { rule_name } => ExecutionEvent::RuleDenied {
                        ts: now_ms(),
                        tool: call.tool.clone(),
                        rule_name: rule_name.clone(),
                    },
                    _ => ExecutionEvent::ToolDenied {
                        ts: now_ms(),
                        tool: call.tool.clone(),
                    },
                };
                append_event(&mut guard, event);
                mark_stopped(&mut guard, &reason_name);
            }
            BridgeResp::json(200, serde_json::to_string(&denial_from(reason)).unwrap())
        }

        PolicyDecision::Allow => {
            // Execute tool — no lock held during execution (may be slow for http_get).
            let cost = registry.declared_cost(&call.tool).unwrap_or(0);
            let result = registry.call(&call.tool, &call.args);

            match result {
                Err(ToolCallError::NotFound { .. }) => {
                    // User-defined tool — the function body runs in the child process.
                    // The bridge just charges the declared token cost and records the call.
                    let cost = call.tokens.unwrap_or(0);
                    {
                        let mut guard = shared.lock().unwrap();
                        guard.tokens_spent += cost;
                        *guard.tool_call_counts.entry(call.tool.clone()).or_insert(0) += 1;
                        guard.tool_call_history.push(call.tool.clone());
                        append_event(&mut guard, ExecutionEvent::ToolAllowed {
                            ts: now_ms(),
                            tool: call.tool.clone(),
                        });
                    }
                    BridgeResp::json(200, serde_json::to_string(
                        &ToolCallResponse::Allowed { result: String::new() }
                    ).unwrap())
                }
                Err(ToolCallError::Execution { tool_name, source }) => {
                    {
                        let mut guard = shared.lock().unwrap();
                        append_event(&mut guard, ExecutionEvent::ToolFailed {
                            ts:    now_ms(),
                            tool:  tool_name.clone(),
                            error: source.to_string(),
                        });
                    }
                    BridgeResp::json(500, format!(
                        r#"{{"error":"tool execution failed","tool_name":"{}","message":"{}"}}"#,
                        tool_name, source
                    ))
                }
                Ok(output) => {
                    {
                        let mut guard = shared.lock().unwrap();
                        guard.tokens_spent += cost;
                        *guard.tool_call_counts.entry(call.tool.clone()).or_insert(0) += 1;
                        guard.tool_call_history.push(call.tool.clone());
                        append_event(&mut guard, ExecutionEvent::ToolAllowed {
                            ts: now_ms(),
                            tool: call.tool.clone(),
                        });
                    }
                    BridgeResp::json(200, serde_json::to_string(
                        &ToolCallResponse::Allowed { result: output.content }
                    ).unwrap())
                }
            }
        }
    }
}

pub(crate) fn handle_rule_evaluate(body: &[u8], shared: &Arc<Mutex<BridgeState>>) -> BridgeResp {
    let req: RuleEvalRequest = serde_json::from_slice(body).unwrap_or_default();

    let decision = {
        let guard = shared.lock().unwrap();
        let ctx = PolicyContext {
            tool_labels: guard.tool_labels.clone(),
            elapsed_ms: req.elapsed
                .unwrap_or_else(|| guard.start_time.elapsed().as_millis() as u64),
            requested_tool: req.tool.clone(),
            tokens_spent: req.tokens_spent.unwrap_or(guard.tokens_spent),
            tool_call_counts: if req.tool_call_counts.is_empty() {
                guard.tool_call_counts.clone()
            } else {
                req.tool_call_counts
            },
            tool_call_history: if req.tool_call_history.is_empty() {
                guard.tool_call_history.clone()
            } else {
                req.tool_call_history
            },
            last_tool_args: HashMap::new(),
        };
        guard.rule_evaluator.evaluate(&ctx)
    };

    let body = match decision {
        PolicyDecision::Allow =>
            r#"{"status":"allowed"}"#.to_string(),
        PolicyDecision::Deny { reason: StopReason::RuleDenied { rule_name } } =>
            format!(r#"{{"status":"denied","rule_name":"{}"}}"#, rule_name),
        PolicyDecision::Deny { reason } =>
            format!(r#"{{"status":"denied","reason":"{}"}}"#, stop_reason_name(&reason)),
    };
    BridgeResp::json(200, body)
}

pub(crate) fn handle_agent_enter(body: &[u8], shared: &Arc<Mutex<BridgeState>>) -> BridgeResp {
    let req: AgentEnterRequest = match serde_json::from_slice(body) {
        Ok(b) => b,
        Err(_) => return BridgeResp::json(400, r#"{"error":"invalid request body"}"#),
    };

    {
        let mut guard = shared.lock().unwrap();
        guard.agent_name_stack.push(req.name.clone());
        append_event(&mut guard, ExecutionEvent::AgentScopeEntered {
            ts: now_ms(),
            name: req.name.clone(),
        });
    }

    BridgeResp::json(200, r#"{"status":"ok"}"#)
}

pub(crate) fn handle_agent_exit(shared: &Arc<Mutex<BridgeState>>) -> BridgeResp {
    let mut guard = shared.lock().unwrap();
    let name = guard.agent_name_stack.pop().unwrap_or_default();
    append_event(&mut guard, ExecutionEvent::AgentScopeExited {
        ts: now_ms(),
        name,
    });
    BridgeResp::json(200, r#"{"status":"ok"}"#)
}

/// POST /llm/usage {"input": N, "output": N, "model"?: "...", "provider"?: "..."}
///
/// Submits LLM token usage from `nanny::report_usage` (Rust) or a
/// nanny.instrument()-wrapped client (Python). Records `input + output` tokens
/// and emits an `LlmUsageRecorded` audit event. The
/// optional `model`/`provider` are recorded as labels only — no pricing.
/// The optional `cache_read`/`cache_write` are a finer split of `input` for
/// providers that report prompt-caching usage — reporting only, never
/// counted separately from `input`.
///
/// Returns `{"status":"ok"}`. Usage is measured, never enforced: no token
/// count stops an execution.
pub(crate) fn handle_llm_usage(body: &[u8], shared: &Arc<Mutex<BridgeState>>) -> BridgeResp {
    #[derive(serde::Deserialize)]
    struct HarnessLabel {
        #[serde(default)]
        name: String,
        #[serde(default)]
        version: Option<String>,
    }

    #[derive(serde::Deserialize)]
    struct LlmUsageRequest {
        #[serde(default)]
        input: u64,
        #[serde(default)]
        output: u64,
        // Optional attribution labels — identifiers only, never content or pricing.
        #[serde(default)]
        model: Option<String>,
        #[serde(default)]
        provider: Option<String>,
        // Optional finer split of `input` (never additional tokens beyond
        // it), present only for providers that report prompt-caching usage.
        // Reporting only, same as model/provider — never debited separately,
        // `total` below still just sums input + output regardless.
        #[serde(default)]
        cache_read: Option<u64>,
        #[serde(default)]
        cache_write: Option<u64>,
        // Optional harness, auto-detected by the SDK and (re)sent on every call.
        // Deduped by `record_harness`, so resending it is cheap.
        #[serde(default)]
        harness: Option<HarnessLabel>,
    }

    let req: LlmUsageRequest = match serde_json::from_slice(body) {
        Ok(r) => r,
        Err(_) => return BridgeResp::json(400, r#"{"error":"invalid request body"}"#),
    };

    let mut guard = shared.lock().unwrap();

    // Record the harness (deduped) even for a zero-token call.
    if let Some(h) = req.harness {
        record_harness(&mut guard, h.name, h.version);
    }

    let total = req.input + req.output;
    if total == 0 {
        return BridgeResp::json(200, r#"{"status":"ok"}"#);
    }

    guard.tokens_spent += total;

    append_event(&mut guard, ExecutionEvent::LlmUsageRecorded {
        ts: now_ms(),
        input: req.input,
        output: req.output,
        model: req.model,
        provider: req.provider,
        cache_read: req.cache_read,
        cache_write: req.cache_write,
    });

    BridgeResp::json(200, r#"{"status":"ok"}"#)
}

/// POST /harness {"name": "...", "version"?: "..."}
///
/// Records the agentic harness that ran the loop (opencode, langgraph, …),
/// declared via `nanny::set_harness`. Emits a `HarnessIdentified` audit event —
/// our equivalent of OpenRouter's "app" column. Attribution label only: never
/// content and never pricing.
///
/// Returns `{"status":"ok"}` if accepted, `400` for a missing/empty name.
pub(crate) fn handle_harness(body: &[u8], shared: &Arc<Mutex<BridgeState>>) -> BridgeResp {
    #[derive(serde::Deserialize)]
    struct HarnessRequest {
        #[serde(default)]
        name: String,
        #[serde(default)]
        version: Option<String>,
    }

    let req: HarnessRequest = match serde_json::from_slice(body) {
        Ok(r) => r,
        Err(_) => return BridgeResp::json(400, r#"{"error":"invalid request body"}"#),
    };
    if req.name.trim().is_empty() {
        return BridgeResp::json(400, r#"{"error":"harness name required"}"#);
    }

    let mut guard = shared.lock().unwrap();
    record_harness(&mut guard, req.name, req.version);

    BridgeResp::json(200, r#"{"status":"ok"}"#)
}

/// POST /rules {"rules": ["no_send_after_read", ...]}
///
/// Records the rules this process has registered. Emits a `RulesDeclared`
/// audit event, deduped bridge-side, so a caller may safely redeclare.
///
/// This is the half of declared authority the governor cannot see for itself:
/// rules are compiled into the agent's process, not into nanny.toml. Without
/// it, the audit log records every refusal but never what could have refused,
/// which is the difference between "nothing was blocked" and "nothing was
/// watching".
///
/// Declaration only, exactly like `/harness` and `/app`: registering a rule
/// name here never enforces anything. Enforcement stays where the rule body
/// is.
///
/// Returns `{"status":"ok"}`, `400` for a malformed body.
pub(crate) fn handle_rules(body: &[u8], shared: &Arc<Mutex<BridgeState>>) -> BridgeResp {
    #[derive(serde::Deserialize)]
    struct RulesRequest {
        #[serde(default)]
        rules: Vec<String>,
    }

    let req: RulesRequest = match serde_json::from_slice(body) {
        Ok(r) => r,
        Err(_) => return BridgeResp::json(400, r#"{"error":"invalid request body"}"#),
    };

    let mut guard = shared.lock().unwrap();
    record_rules(&mut guard, req.rules);

    BridgeResp::json(200, r#"{"status":"ok"}"#)
}

/// Append a `RulesDeclared` event only when the rule set actually changes.
///
/// Sorted and de-duplicated first, so the same set declared in a different
/// order is the same declaration and does not produce a second event. Rust
/// rule registration order is link order, which is not stable across builds,
/// so without this the log would churn for no reason.
pub(crate) fn record_rules(state: &mut BridgeState, rules: Vec<String>) {
    let mut rules: Vec<String> =
        rules.into_iter().map(|r| r.trim().to_string()).filter(|r| !r.is_empty()).collect();
    rules.sort();
    rules.dedup();
    if rules.is_empty() || state.last_rules.as_ref() == Some(&rules) {
        return;
    }
    state.last_rules = Some(rules.clone());
    append_event(state, ExecutionEvent::RulesDeclared { ts: now_ms(), rules });
}

/// POST /app {"app_id": "app_...", "name": "..."}
///
/// Records which app this process is, read from its committed
/// `.nanny/app.json`. Emits an `AppIdentified` audit event, deduped bridge-side,
/// so a caller may safely (re)declare on every request.
///
/// This is the mechanism that keeps one governor serving many apps attributable
/// per app: identity travels in the payload, not in the credential, so a joined
/// process with no key and no nanny.toml of its own still lands under its own
/// name. Attribution label only: never content, never pricing, never a stop.
///
/// Returns `{"status":"ok"}` if accepted, `400` for a missing/empty app_id.
pub(crate) fn handle_app(body: &[u8], shared: &Arc<Mutex<BridgeState>>) -> BridgeResp {
    #[derive(serde::Deserialize)]
    struct AppRequest {
        #[serde(default)]
        app_id: String,
        #[serde(default)]
        name: String,
    }

    let req: AppRequest = match serde_json::from_slice(body) {
        Ok(r) => r,
        Err(_) => return BridgeResp::json(400, r#"{"error":"invalid request body"}"#),
    };
    if req.app_id.trim().is_empty() {
        return BridgeResp::json(400, r#"{"error":"app_id required"}"#);
    }

    let mut guard = shared.lock().unwrap();
    record_app(&mut guard, req.app_id, req.name);

    BridgeResp::json(200, r#"{"status":"ok"}"#)
}

// ── Unix domain socket transport ──────────────────────────────────────────────

/// Read a minimal HTTP/1.x request from any byte stream.
///
/// Handles the subset the bridge needs: method, path,
/// `X-Nanny-Session-Token`, `Content-Length`, and body.
/// Returns `None` if the stream ends unexpectedly or headers are malformed.
#[cfg(unix)]
fn parse_http_request(stream: &mut impl std::io::Read) -> Option<BridgeReq> {
    // Read byte-by-byte until we see the end-of-headers marker.
    let mut header_buf: Vec<u8> = Vec::with_capacity(512);
    let mut byte = [0u8; 1];
    loop {
        stream.read_exact(&mut byte).ok()?;
        header_buf.push(byte[0]);
        if header_buf.ends_with(b"\r\n\r\n") {
            break;
        }
        if header_buf.len() > 8192 {
            return None; // guard against oversized headers
        }
    }

    let header_str = std::str::from_utf8(&header_buf).ok()?;
    let mut lines = header_str.lines();

    // Request line: METHOD /path HTTP/1.x
    let first = lines.next()?;
    let mut parts = first.split_ascii_whitespace();
    let method = parts.next()?.to_string();
    let path   = parts.next()?.to_string();

    let mut token: Option<String> = None;
    let mut content_length: usize = 0;

    for line in lines {
        if line.is_empty() { break; }
        if let Some((name, value)) = line.split_once(':') {
            let name  = name.trim();
            let value = value.trim();
            if name.eq_ignore_ascii_case("x-nanny-session-token") {
                token = Some(value.to_string());
            } else if name.eq_ignore_ascii_case("content-length") {
                content_length = value.parse().unwrap_or(0);
            }
        }
    }

    let mut body = vec![0u8; content_length];
    if content_length > 0 {
        stream.read_exact(&mut body).ok()?;
    }

    Some(BridgeReq { method, path, token, body })
}

/// Write an HTTP/1.1 response to any byte stream.
#[cfg(unix)]
fn write_http_response(stream: &mut impl std::io::Write, resp: &BridgeResp) {
    let ct = match resp.content_type {
        ContentType::Json   => "application/json",
        ContentType::Ndjson => "application/x-ndjson",
    };
    let body = resp.body.as_bytes();
    let _ = write!(
        stream,
        "HTTP/1.1 {status} \r\nContent-Type: {ct}\r\nContent-Length: {len}\r\n\r\n",
        status = resp.status,
        ct = ct,
        len = body.len(),
    );
    let _ = stream.write_all(body);
}

// ── TCP transport (Windows / non-Unix) ────────────────────────────────────────

#[cfg(not(unix))]
fn serve_tcp(
    server: tiny_http::Server,
    shared: Arc<Mutex<BridgeState>>,
    registry: Arc<ToolRegistry>,
) {
    use std::io::Read;
    for mut request in server.incoming_requests() {
        let token = request
            .headers()
            .iter()
            .find(|h| {
                h.field.as_str().as_str().eq_ignore_ascii_case("x-nanny-session-token")
            })
            .map(|h| h.value.as_str().to_string());

        let mut body = Vec::new();
        request.as_reader().read_to_end(&mut body).unwrap_or(0);

        let req = BridgeReq {
            method: request.method().as_str().to_string(),
            path:   request.url().to_string(),
            token,
            body,
        };
        let resp = dispatch(req, &shared, &registry);
        let _ = request.respond(make_tiny_response(resp));
    }
}

#[cfg(not(unix))]
fn make_tiny_response(
    resp: BridgeResp,
) -> tiny_http::Response<std::io::Cursor<Vec<u8>>> {
    let ct = match resp.content_type {
        ContentType::Json   => "application/json",
        ContentType::Ndjson => "application/x-ndjson",
    };
    tiny_http::Response::from_data(resp.body.into_bytes())
        .with_status_code(tiny_http::StatusCode(resp.status))
        .with_header(
            tiny_http::Header::from_bytes("Content-Type", ct).unwrap(),
        )
}

// ── Request / response types ──────────────────────────────────────────────────

#[derive(serde::Deserialize)]
struct ToolCallRequest {
    tool: String,
    #[serde(default)]
    args: ToolArgs,
    /// Token cost declared by the macro at the call site.
    /// Used when the tool is not registered in the bridge registry (user-defined tools).
    #[serde(default)]
    tokens: Option<u64>,
}

#[derive(serde::Deserialize, Default)]
struct RuleEvalRequest {
    #[serde(default)] elapsed:           Option<u64>,
    #[serde(default)] tool:              Option<String>,
    #[serde(default)] tool_call_counts:  HashMap<String, u32>,
    #[serde(default)] tool_call_history: Vec<String>,
    #[serde(default)] tokens_spent:      Option<u64>,
}

#[derive(serde::Deserialize)]
struct AgentEnterRequest {
    name: String,
}

#[derive(serde::Serialize)]
#[serde(tag = "status")]
enum ToolCallResponse {
    #[serde(rename = "allowed")]
    Allowed { result: String },
    #[serde(rename = "denied")]
    Denied {
        reason: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        tool_name: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        rule_name: Option<String>,
    },
}

fn denial_from(reason: &StopReason) -> ToolCallResponse {
    match reason {
        StopReason::ToolDenied { tool_name } => ToolCallResponse::Denied {
            reason: "ToolDenied".into(),
            tool_name: Some(tool_name.clone()),
            rule_name: None,
        },
        StopReason::RuleDenied { rule_name } => ToolCallResponse::Denied {
            reason: "RuleDenied".into(),
            tool_name: None,
            rule_name: Some(rule_name.clone()),
        },
        other => ToolCallResponse::Denied {
            reason: stop_reason_name(other).into(),
            tool_name: None,
            rule_name: None,
        },
    }
}

fn stop_reason_name(reason: &StopReason) -> &'static str {
    match reason {
        StopReason::ToolDenied { .. } => "ToolDenied",
        StopReason::RuleDenied { .. } => "RuleDenied",
        StopReason::ManualStop        => "ManualStop",
        StopReason::AgentCompleted    => "AgentCompleted",
    }
}

pub(crate) fn handle_stop(body: &[u8], shared: &Arc<Mutex<BridgeState>>) -> BridgeResp {
    let parsed = serde_json::from_slice::<serde_json::Value>(body).unwrap_or_default();
    let raw = parsed["reason"].as_str().unwrap_or_default().to_string();
    // Validate against the closed set of known stop reasons.
    // An untrusted child process holds the session token and can POST /stop;
    // accepting arbitrary strings would let a misbehaving agent falsify the
    // event log (e.g. claim "AgentCompleted" while actually crashing).
    let reason = match raw.as_str() {
        "RuleDenied" | "ToolDenied" | "AgentCompleted" | "ManualStop" | "ToolFailed" => raw,
        _ => "ProcessCrashed".to_string(),
    };
    let mut guard = shared.lock().unwrap();
    // When the SDK reports a client-side rule denial it knows both the rule name
    // and the tool that triggered it — emit the RuleDenied event here so the
    // NDJSON stream contains it even though no /tool/call ever reached the bridge.
    if reason == "RuleDenied" {
        let tool      = parsed["tool"].as_str().unwrap_or("").to_string();
        let rule_name = parsed["rule_name"].as_str().unwrap_or("").to_string();
        if !tool.is_empty() && !rule_name.is_empty() {
            append_event(&mut guard, ExecutionEvent::RuleDenied { ts: now_ms(), tool, rule_name });
        }
    }
    mark_stopped(&mut guard, &reason);
    BridgeResp::json(200, r#"{"status":"ok"}"#)
}

// ── State helpers ─────────────────────────────────────────────────────────────

/// Mark execution as stopped.
/// Idempotent — does nothing if already stopped.
/// ExecutionStopped is emitted by the CLI, not the bridge.
pub(crate) fn mark_stopped(state: &mut BridgeState, reason: &str) {
    if matches!(state.execution, ExecutionState::Stopped { .. }) {
        return;
    }
    state.execution = ExecutionState::Stopped { reason: reason.to_string() };
}

pub(crate) fn append_event(state: &mut BridgeState, event: ExecutionEvent) {
    state.events.push(serde_json::to_string(&event).unwrap());
}

/// Append a `HarnessIdentified` event only when the harness actually changes.
/// The SDK may resend the harness on every LLM call (auto-detected from traffic),
/// so dedup keeps the append-only log from filling with identical entries. Empty
/// names are ignored.
pub(crate) fn record_harness(state: &mut BridgeState, name: String, version: Option<String>) {
    let name = name.trim().to_string();
    if name.is_empty() {
        return;
    }
    let candidate = (name, version);
    if state.last_harness.as_ref() == Some(&candidate) {
        return;
    }
    state.last_harness = Some(candidate.clone());
    append_event(
        state,
        ExecutionEvent::HarnessIdentified {
            ts: now_ms(),
            name: candidate.0,
            version: candidate.1,
        },
    );
}

/// Append an `AppIdentified` event only when the app actually changes.
/// Mirrors `record_harness` exactly, and for the same reason: a caller may
/// safely (re)declare its identity on every request, so dedup keeps the
/// append-only log from filling with identical entries.
///
/// An empty `app_id` is ignored. Identity is generated by `nanny init` and is
/// never blank, so a blank one means a malformed caller, not a real app. An
/// empty `name` is allowed through as-is: the id is what identifies, the name
/// is only a label.
pub(crate) fn record_app(state: &mut BridgeState, app_id: String, name: String) {
    let app_id = app_id.trim().to_string();
    if app_id.is_empty() {
        return;
    }
    let candidate = (app_id, name.trim().to_string());
    if state.last_app.as_ref() == Some(&candidate) {
        return;
    }
    state.last_app = Some(candidate.clone());
    append_event(
        state,
        ExecutionEvent::AppIdentified {
            ts: now_ms(),
            app_id: candidate.0,
            name: candidate.1,
        },
    );
}

pub(crate) fn now_ms() -> u64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

/// Return the typed stop reason if this run has stopped, else `None`.
///
/// Used to build the 410 Gone body so a stopped run tells the client *why* it
/// stopped (ToolDenied, RuleDenied, …) instead of a generic message.
pub(crate) fn stopped_reason(shared: &Arc<Mutex<BridgeState>>) -> Option<String> {
    match &shared.lock().unwrap().execution {
        ExecutionState::Stopped { reason } => Some(reason.clone()),
        ExecutionState::Running => None,
    }
}

/// Build the 410 Gone body for a stopped run.
///
/// `error` stays `"execution stopped"` for backward compatibility; `reason`
/// carries the specific stop-reason name so clients surface the true cause.
/// `reason` is always one of the closed set of stop-reason identifiers, so it
/// needs no JSON escaping.
pub(crate) fn stopped_response(reason: &str) -> BridgeResp {
    BridgeResp::json(410, format!(r#"{{"error":"execution stopped","reason":"{reason}"}}"#))
}

// ── Network server factory ────────────────────────────────────────────────────

/// Template for building fresh per-run enforcement state in the network server.
///
/// The governance server keeps one [`BridgeState`] per run id (see G3 —
/// "Nanny stops the run, not the host"). All runs share one immutable
/// [`ToolRegistry`]; everything else — counters, stop state, scope
/// stacks — is cloned per run from this template so each run is independently
/// governed and independently stoppable.
pub(crate) struct RunTemplate {
    session_token: String,
    allowed_tools: Vec<String>,
    per_tool_max_calls: HashMap<String, u32>,
    tool_labels: HashMap<String, Vec<String>>,
}

impl RunTemplate {
    /// Build a fresh, running [`BridgeState`] for a new run.
    ///
    /// Each call produces a distinct execution with zeroed counters, so a
    /// stop on one run never touches another.
    pub(crate) fn build_state(&self) -> Arc<Mutex<BridgeState>> {
        let tool_permission_policy =
            ToolPermissionPolicy::new(self.allowed_tools.clone());
        let rule_evaluator = RuleEvaluator::new(self.per_tool_max_calls.clone());

        Arc::new(Mutex::new(BridgeState {
            session_token: self.session_token.clone(),
            execution: ExecutionState::Running,
            tool_permission_policy,
            rule_evaluator,
            agent_name_stack: Vec::new(),
            allowed_tools: self.allowed_tools.clone(),
            tool_labels: self.tool_labels.clone(),
            tokens_spent: 0,
            tool_call_counts: HashMap::new(),
            tool_call_history: Vec::new(),
            start_time: std::time::Instant::now(),
            events: Vec::new(),
            last_harness: None,
            last_rules: None,
            last_app: None,
        }))
    }
}

/// Build the per-run state template and the shared registry for the network
/// server. The registry is immutable and shared across all runs; the template
/// mints a fresh [`BridgeState`] per run id.
pub(crate) fn init_run_template(
    components: BridgeComponents,
    token: String,
) -> (RunTemplate, Arc<ToolRegistry>) {
    let template = RunTemplate {
        session_token: token,
        allowed_tools: components.allowed_tools,
        per_tool_max_calls: components.per_tool_max_calls,
        tool_labels: components.tool_labels,
    };
    let registry = Arc::new(components.registry);
    (template, registry)
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use nanny_core::tool::{Tool, ToolError, ToolOutput};

    // ── Fixtures ──────────────────────────────────────────────────────────────

    struct EchoTool;
    impl Tool for EchoTool {
        fn name(&self) -> &str { "echo" }
        fn declared_cost(&self) -> u64 { 10 }
        fn execute(&self, args: &ToolArgs) -> Result<ToolOutput, ToolError> {
            Ok(ToolOutput { content: args.get("message").cloned().unwrap_or_default() })
        }
    }

    struct FailingTool;
    impl Tool for FailingTool {
        fn name(&self) -> &str { "fail" }
        fn declared_cost(&self) -> u64 { 5 }
        fn execute(&self, _args: &ToolArgs) -> Result<ToolOutput, ToolError> {
            Err(ToolError::ExecutionFailed("simulated network error".into()))
        }
    }

    fn echo_components(_max_tokens: u64) -> BridgeComponents {
        let mut registry = ToolRegistry::new();
        registry.register(Box::new(EchoTool));
        BridgeComponents {
            registry,
            allowed_tools: vec!["echo".to_string()],
            per_tool_max_calls: Default::default(),
            tool_labels: Default::default(),
        }
    }

    fn started(max_cost: u64) -> Bridge {
        let b = Bridge::start(echo_components(max_cost)).unwrap();
        // Small pause to let the server thread reach accept().
        std::thread::sleep(std::time::Duration::from_millis(20));
        b
    }

    /// Bridge with custom allowed tools and an empty registry — exercises
    /// the user-defined tool path (NotFound → charge tokens → return allowed).
    fn started_with_tools(allowed_tools: Vec<String>, _max_tokens: u64) -> Bridge {
        let components = BridgeComponents {
            registry: ToolRegistry::new(),
            allowed_tools,
            per_tool_max_calls: Default::default(),
            tool_labels: Default::default(),
        };
        let b = Bridge::start(components).unwrap();
        std::thread::sleep(std::time::Duration::from_millis(20));
        b
    }

    // ── HTTP helpers ──────────────────────────────────────────────────────────
    //
    // On Unix the bridge uses a Unix domain socket; on Windows it uses TCP.
    // These helpers abstract over the transport so all tests are identical.

    fn http_get(addr: &BridgeAddress, token: &str, path: &str) -> (u16, String) {
        #[cfg(unix)]
        if let BridgeAddress::Unix(socket_path) = addr {
            use std::io::{Read, Write};
            use std::os::unix::net::UnixStream;
            let mut s = UnixStream::connect(socket_path).unwrap();
            write!(
                s,
                "GET {path} HTTP/1.0\r\nX-Nanny-Session-Token: {token}\r\n\r\n"
            ).unwrap();
            let mut raw = String::new();
            s.read_to_string(&mut raw).unwrap();
            return parse_http(raw);
        }
        // TCP fallback (Windows)
        #[allow(unreachable_patterns)]
        let BridgeAddress::Tcp(port) = addr else { unreachable!() };
        tcp_get(*port, token, path)
    }

    fn http_post(addr: &BridgeAddress, token: &str, path: &str, body: &str) -> (u16, String) {
        #[cfg(unix)]
        if let BridgeAddress::Unix(socket_path) = addr {
            use std::io::{Read, Write};
            use std::os::unix::net::UnixStream;
            let mut s = UnixStream::connect(socket_path).unwrap();
            write!(
                s,
                "POST {path} HTTP/1.0\r\nX-Nanny-Session-Token: {token}\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{body}",
                body.len()
            ).unwrap();
            let mut raw = String::new();
            s.read_to_string(&mut raw).unwrap();
            return parse_http(raw);
        }
        // TCP fallback (Windows)
        #[allow(unreachable_patterns)]
        let BridgeAddress::Tcp(port) = addr else { unreachable!() };
        tcp_post(*port, token, path, body)
    }

    fn get(b: &Bridge, path: &str) -> (u16, String) {
        http_get(&b.address, &b.session_token, path)
    }

    fn post(b: &Bridge, path: &str, body: &str) -> (u16, String) {
        http_post(&b.address, &b.session_token, path, body)
    }

    // TCP helpers (used directly on Windows, used by http_get/http_post fallback)
    fn tcp_get(port: u16, token: &str, path: &str) -> (u16, String) {
        use std::io::{Read, Write};
        use std::net::TcpStream;
        let mut s = TcpStream::connect(("127.0.0.1", port)).unwrap();
        write!(
            s,
            "GET {path} HTTP/1.0\r\nHost: 127.0.0.1\r\nX-Nanny-Session-Token: {token}\r\n\r\n"
        ).unwrap();
        let mut raw = String::new();
        s.read_to_string(&mut raw).unwrap();
        parse_http(raw)
    }

    fn tcp_post(port: u16, token: &str, path: &str, body: &str) -> (u16, String) {
        use std::io::{Read, Write};
        use std::net::TcpStream;
        let mut s = TcpStream::connect(("127.0.0.1", port)).unwrap();
        write!(
            s,
            "POST {path} HTTP/1.0\r\nHost: 127.0.0.1\r\nX-Nanny-Session-Token: {token}\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{body}",
            body.len()
        ).unwrap();
        let mut raw = String::new();
        s.read_to_string(&mut raw).unwrap();
        parse_http(raw)
    }

    fn parse_http(raw: String) -> (u16, String) {
        let status = raw.lines().next()
            .and_then(|l| l.split_whitespace().nth(1))
            .and_then(|s| s.parse().ok())
            .unwrap_or(0u16);
        let body = raw.split_once("\r\n\r\n").map(|(_, b)| b.to_string()).unwrap_or_default();
        (status, body)
    }

    fn json_val(s: &str) -> serde_json::Value {
        serde_json::from_str(s).expect("expected valid JSON")
    }

    // ── Day 1 tests ───────────────────────────────────────────────────────────

    #[test]
    fn bridge_has_valid_address() {
        let b = started(1000);
        match &b.address {
            #[cfg(unix)]
            BridgeAddress::Unix(path) => assert!(path.exists(), "socket file must exist"),
            BridgeAddress::Tcp(port)  => assert!(*port > 0, "TCP port must be non-zero"),
        }
    }

    #[test]
    fn each_bridge_gets_a_unique_token() {
        let b1 = Bridge::start(echo_components(1000)).unwrap();
        let b2 = Bridge::start(echo_components(1000)).unwrap();
        assert_ne!(b1.session_token, b2.session_token);
    }

    #[test]
    fn health_returns_running_state() {
        let b = started(1000);
        let (s, body) = get(&b, "/health");
        assert_eq!(s, 200);
        assert_eq!(json_val(&body)["state"], "running");
    }

    #[test]
    fn wrong_token_returns_401() {
        let b = started(1000);
        let (s, _) = http_get(&b.address, "wrong-token", "/health");
        assert_eq!(s, 401);
    }

    #[test]
    fn missing_token_returns_401() {
        let b = started(1000);
        let (s, _) = http_get(&b.address, "", "/health");
        assert_eq!(s, 401);
    }

    #[test]
    fn unknown_route_returns_404() {
        let b = started(1000);
        let (s, _) = get(&b, "/nonexistent");
        assert_eq!(s, 404);
    }

    #[test]
    fn stop_reflects_in_health_response() {
        let b = started(1000);
        b.stop("ToolDenied");
        std::thread::sleep(std::time::Duration::from_millis(10));
        let (s, body) = get(&b, "/health");
        assert_eq!(s, 200);
        let v = json_val(&body);
        assert_eq!(v["state"], "stopped");
        assert_eq!(v["reason"], "ToolDenied");
    }

    // ── Day 2 tests ───────────────────────────────────────────────────────────

    #[test]
    fn tool_call_returns_allowed_and_result() {
        let b = started(1000);
        let (s, body) = post(&b, "/tool/call", r#"{"tool":"echo","args":{"message":"hi"}}"#);
        assert_eq!(s, 200);
        let v = json_val(&body);
        assert_eq!(v["status"], "allowed");
        assert_eq!(v["result"], "hi");
    }

    #[test]
    fn tool_call_charges_cost_and_tracks_counts() {
        let b = started(1000);
        post(&b, "/tool/call", r#"{"tool":"echo","args":{"message":"a"}}"#);
        post(&b, "/tool/call", r#"{"tool":"echo","args":{"message":"b"}}"#);

        let (_, body) = get(&b, "/status");
        let v = json_val(&body);
        assert_eq!(v["tokens_spent"], 20); // 2 calls × tokens 10
    }

    /// Each allowed tool call is counted and its token cost measured.
    ///
    /// This is the bridge-level regression guard for the bug where
    /// ExecutionStopped emitted zeros. metrics() must reflect the real
    /// accounting state so the CLI can emit accurate values.
    #[test]
    fn tool_call_is_counted_and_charged_in_metrics() {
        let b = started(1000);
        post(&b, "/tool/call", r#"{"tool":"echo","args":{"message":"x"}}"#);
        post(&b, "/tool/call", r#"{"tool":"echo","args":{"message":"y"}}"#);

        let m = b.metrics();
        assert_eq!(m.tool_call_count, 2,  "every tool call must be recorded");
        assert_eq!(m.tokens_spent,   20, "each tool call must charge declared token cost");
    }

    #[test]
    fn denied_tool_returns_denied_with_tool_name() {
        let b = Bridge::start(BridgeComponents {
            registry: ToolRegistry::new(),
            allowed_tools: vec![],   // empty allowlist — all tools denied
            per_tool_max_calls: Default::default(),
            tool_labels: Default::default(),
        }).unwrap();
        std::thread::sleep(std::time::Duration::from_millis(20));

        let (s, body) = post(&b, "/tool/call", r#"{"tool":"echo","args":{}}"#);
        assert_eq!(s, 200);
        let v = json_val(&body);
        assert_eq!(v["status"], "denied");
        assert_eq!(v["reason"], "ToolDenied");
        assert_eq!(v["tool_name"], "echo");
    }

    #[test]
    fn denied_tool_stops_execution() {
        let b = Bridge::start(BridgeComponents {
            registry: ToolRegistry::new(),
            allowed_tools: vec![],
            per_tool_max_calls: Default::default(),
            tool_labels: Default::default(),
        }).unwrap();
        std::thread::sleep(std::time::Duration::from_millis(20));

        post(&b, "/tool/call", r#"{"tool":"echo","args":{}}"#);
        assert!(matches!(b.execution_state(), ExecutionState::Stopped { .. }));
    }

    #[test]
    fn tokens_accumulate_without_stopping_execution() {
        // Tokens are measured, never enforced: no count ends a run.
        let b = started(10); // echo costs 10
        post(&b, "/tool/call", r#"{"tool":"echo","args":{"message":"x"}}"#);
        post(&b, "/tool/call", r#"{"tool":"echo","args":{"message":"y"}}"#);

        assert!(matches!(b.execution_state(), ExecutionState::Running));
        let (_, status) = get(&b, "/status");
        assert_eq!(json_val(&status)["tokens_spent"], 20);
    }

    #[test]
    fn tool_call_on_stopped_execution_returns_410() {
        let b = started(1000);
        b.stop("ToolDenied");
        std::thread::sleep(std::time::Duration::from_millis(10));
        let (s, _) = post(&b, "/tool/call", r#"{"tool":"echo","args":{}}"#);
        assert_eq!(s, 410);
    }

    /// G7: the 410 body carries the typed stop reason so the client reports the
    /// true cause (here ToolDenied) rather than a generic "execution stopped".
    #[test]
    fn stopped_410_body_carries_typed_reason() {
        let b = started(1000);
        b.stop("ToolDenied");
        std::thread::sleep(std::time::Duration::from_millis(10));
        let (s, body) = post(&b, "/tool/call", r#"{"tool":"echo","args":{}}"#);
        assert_eq!(s, 410);
        let v = json_val(&body);
        assert_eq!(v["reason"], "ToolDenied");
        assert_eq!(v["error"], "execution stopped");
    }

    #[test]
    fn invalid_request_body_returns_400() {
        let b = started(1000);
        let (s, _) = post(&b, "/tool/call", "not json");
        assert_eq!(s, 400);
    }

    #[test]
    fn max_calls_rule_stops_execution_on_excess() {
        let mut registry = ToolRegistry::new();
        registry.register(Box::new(EchoTool));
        let mut per_tool_max_calls = HashMap::new();
        per_tool_max_calls.insert("echo".to_string(), 1u32);
        let b = Bridge::start(BridgeComponents {
            registry,
            allowed_tools: vec!["echo".to_string()],
            per_tool_max_calls,
            tool_labels: Default::default(),
        }).unwrap();
        std::thread::sleep(std::time::Duration::from_millis(20));

        // First call: allowed
        let (_, body) = post(&b, "/tool/call", r#"{"tool":"echo","args":{}}"#);
        assert_eq!(json_val(&body)["status"], "allowed");

        // Second call: denied (max_calls = 1, already called once)
        let (_, body) = post(&b, "/tool/call", r#"{"tool":"echo","args":{}}"#);
        assert_eq!(json_val(&body)["status"], "denied");
        assert!(matches!(b.execution_state(), ExecutionState::Stopped { .. }));
    }

    // ── Day 3 tests ───────────────────────────────────────────────────────────

    #[test]
    fn rule_evaluate_allows_when_no_rules_configured() {
        let b = started(1000);
        let (s, body) = post(&b, "/rule/evaluate", "{}");
        assert_eq!(s, 200);
        assert_eq!(json_val(&body)["status"], "allowed");
    }

    #[test]
    fn rule_evaluate_denies_at_max_calls_with_provided_context() {
        let mut per_tool_max_calls = HashMap::new();
        per_tool_max_calls.insert("echo".to_string(), 2u32);
        let b = Bridge::start(BridgeComponents {
            registry: ToolRegistry::new(),
            allowed_tools: vec!["echo".to_string()],
            per_tool_max_calls,
            tool_labels: Default::default(),
        }).unwrap();
        std::thread::sleep(std::time::Duration::from_millis(20));

        let ctx = r#"{"tool":"echo","tool_call_counts":{"echo":2}}"#;
        let (_, body) = post(&b, "/rule/evaluate", ctx);
        let v = json_val(&body);
        assert_eq!(v["status"], "denied");
        assert_eq!(v["rule_name"], "echo.max_calls");
    }

    #[test]
    fn rule_evaluate_uses_tracked_state_when_no_context_provided() {
        let mut per_tool_max_calls = HashMap::new();
        per_tool_max_calls.insert("echo".to_string(), 1u32);
        let mut registry = ToolRegistry::new();
        registry.register(Box::new(EchoTool));
        let b = Bridge::start(BridgeComponents {
            registry,
            allowed_tools: vec!["echo".to_string()],
            per_tool_max_calls,
            tool_labels: Default::default(),
        }).unwrap();
        std::thread::sleep(std::time::Duration::from_millis(20));

        // Make one tool call so bridge tracks 1 echo call.
        post(&b, "/tool/call", r#"{"tool":"echo","args":{}}"#);

        // Rule evaluate with tool="echo" and no explicit counts — uses tracked state.
        let (_, body) = post(&b, "/rule/evaluate", r#"{"tool":"echo"}"#);
        let v = json_val(&body);
        assert_eq!(v["status"], "denied");
    }

    #[test]
    fn rules_declaration_emits_one_event_and_dedupes() {
        let b = started(1000);

        let (s, body) = post(&b, "/rules", r#"{"rules":["b_rule","a_rule"]}"#);
        assert_eq!(s, 200);
        assert_eq!(json_val(&body)["status"], "ok");

        // Same set, different order, plus a duplicate: still the same
        // declaration, so no second event.
        post(&b, "/rules", r#"{"rules":["a_rule","b_rule","a_rule"]}"#);

        let (_, events) = get(&b, "/events");
        let declared: Vec<serde_json::Value> = events
            .lines()
            .filter_map(|l| serde_json::from_str::<serde_json::Value>(l).ok())
            .filter(|v| v["event"] == "RulesDeclared")
            .collect();

        assert_eq!(declared.len(), 1, "redeclaring the same set must not re-emit");
        assert_eq!(
            declared[0]["rules"],
            serde_json::json!(["a_rule", "b_rule"]),
            "rules must be sorted: Rust registration order is link order, which \
             is not stable across builds"
        );
    }

    /// An empty declaration is not an event. "No rules registered" is the
    /// absence of a grant, not a grant of nothing.
    #[test]
    fn empty_rules_declaration_emits_nothing() {
        let b = started(1000);
        let (s, _) = post(&b, "/rules", r#"{"rules":[]}"#);
        assert_eq!(s, 200);

        let (_, events) = get(&b, "/events");
        assert!(!events.contains("RulesDeclared"));
    }

    /// `/status` carries tool labels. This is the wire contract every
    /// out-of-process SDK depends on: it never reads nanny.toml, so this
    /// response is the only place it can learn what a tool is.
    #[test]
    fn status_returns_tool_labels() {
        let mut tool_labels = HashMap::new();
        tool_labels.insert(
            "echo".to_string(),
            vec!["external_effect".to_string(), "moves_money".to_string()],
        );
        tool_labels.insert("quiet".to_string(), Vec::new());
        let mut registry = ToolRegistry::new();
        registry.register(Box::new(EchoTool));
        let b = Bridge::start(BridgeComponents {
            registry,
            allowed_tools: vec!["echo".to_string(), "quiet".to_string()],
            per_tool_max_calls: Default::default(),
            tool_labels,
        }).unwrap();
        std::thread::sleep(std::time::Duration::from_millis(20));

        let (_, body) = get(&b, "/status");
        let v = json_val(&body);

        assert_eq!(v["tool_labels"]["echo"][0], "external_effect");
        assert_eq!(v["tool_labels"]["echo"][1], "moves_money");
        assert!(
            v["tool_labels"]["quiet"].as_array().unwrap().is_empty(),
            "a declared-but-unlabelled tool must appear with an empty list, \
             so a reader can tell it apart from one never declared; got: {v}"
        );
        assert!(
            v["tool_labels"].get("ghost").is_none(),
            "an undeclared tool must be absent entirely; got: {v}"
        );
    }

    #[test]
    fn rule_evaluate_on_stopped_execution_returns_410() {
        let b = started(1000);
        b.stop("ManualStop");
        std::thread::sleep(std::time::Duration::from_millis(10));
        let (s, _) = post(&b, "/rule/evaluate", "{}");
        assert_eq!(s, 410);
    }

    // ── Day 4 tests ───────────────────────────────────────────────────────────

    #[test]
    fn agent_enter_records_the_scope() {
        let b = started(1000);

        let (s, body) = post(&b, "/agent/enter", r#"{"name":"researcher"}"#);
        assert_eq!(s, 200);
        assert_eq!(json_val(&body)["status"], "ok");

        let guard = b.shared.lock().unwrap();
        assert_eq!(guard.agent_name_stack, vec!["researcher".to_string()]);
    }

    #[test]
    fn agent_exit_pops_the_scope() {
        let b = started(1000);

        post(&b, "/agent/enter", r#"{"name":"researcher"}"#);
        let (s, _) = post(&b, "/agent/exit", "{}");
        assert_eq!(s, 200);

        let guard = b.shared.lock().unwrap();
        assert!(guard.agent_name_stack.is_empty());
    }

    #[test]
    fn nested_agent_enter_exit_round_trip() {
        let b = started(1000);

        post(&b, "/agent/enter", r#"{"name":"a"}"#);
        post(&b, "/agent/enter", r#"{"name":"b"}"#);
        post(&b, "/agent/exit",  "{}");
        post(&b, "/agent/exit",  "{}");

        let guard = b.shared.lock().unwrap();
        assert!(guard.agent_name_stack.is_empty(), "every scope must be popped");
    }

    // ── Day 5 tests ───────────────────────────────────────────────────────────

    #[test]
    fn tool_call_is_recorded_and_status_reports_running() {
        let b = started(1000);
        let (s, body) = post(&b, "/tool/call", r#"{"tool":"echo","args":{}}"#);
        assert_eq!(s, 200);
        assert_eq!(json_val(&body)["status"], "allowed");

        let (_, status) = get(&b, "/status");
        let v = json_val(&status);
        assert_eq!(v["state"], "running");
        assert_eq!(v["tool_call_history"][0], "echo");
    }


    #[test]
    fn llm_usage_debits_tokens_and_records_event() {
        let b = started(1000);
        let (s, body) = post(
            &b,
            "/llm/usage",
            r#"{"input":30,"output":12,"model":"gpt-4o","provider":"openai"}"#,
        );
        assert_eq!(s, 200);
        assert_eq!(json_val(&body)["status"], "ok");

        // Debited from the budget.
        let (_, status) = get(&b, "/status");
        assert_eq!(json_val(&status)["tokens_spent"], 42);

        // Recorded in the audit log with attribution labels.
        let (_, events) = get(&b, "/events");
        let usage = events
            .lines()
            .map(json_val)
            .find(|e| e["event"] == "LlmUsageRecorded")
            .expect("LlmUsageRecorded event must appear in /events");
        assert_eq!(usage["input"], 30);
        assert_eq!(usage["output"], 12);
        assert_eq!(usage["model"], "gpt-4o");
        assert_eq!(usage["provider"], "openai");
    }

    #[test]
    fn llm_usage_without_labels_omits_them() {
        let b = started(1000);
        post(&b, "/llm/usage", r#"{"input":5,"output":5}"#);
        let (_, events) = get(&b, "/events");
        let usage = events
            .lines()
            .map(json_val)
            .find(|e| e["event"] == "LlmUsageRecorded")
            .expect("LlmUsageRecorded event must appear");
        // Absent labels are skipped, not serialized as null.
        assert!(usage.get("model").is_none());
        assert!(usage.get("provider").is_none());
        assert!(usage.get("cache_read").is_none());
        assert!(usage.get("cache_write").is_none());
    }

    #[test]
    fn llm_usage_carries_cache_read_and_write() {
        let b = started(1000);
        let (s, body) = post(
            &b,
            "/llm/usage",
            r#"{"input":30,"output":12,"provider":"anthropic","cache_read":5,"cache_write":10}"#,
        );
        assert_eq!(s, 200);
        assert_eq!(json_val(&body)["status"], "ok");

        // Debited exactly input+output — cache_read/cache_write never change
        // the debit, they're a reporting-only split of input already
        // included in it, never additional tokens.
        let (_, status) = get(&b, "/status");
        assert_eq!(json_val(&status)["tokens_spent"], 42);

        let (_, events) = get(&b, "/events");
        let usage = events
            .lines()
            .map(json_val)
            .find(|e| e["event"] == "LlmUsageRecorded")
            .expect("LlmUsageRecorded event must appear in /events");
        assert_eq!(usage["cache_read"], 5);
        assert_eq!(usage["cache_write"], 10);
    }

    #[test]
    fn harness_records_event_does_not_charge_tokens() {
        let b = started(1000);
        let (s, body) = post(&b, "/harness", r#"{"name":"opencode","version":"0.3.2"}"#);
        assert_eq!(s, 200);
        assert_eq!(json_val(&body)["status"], "ok");

        // Attribution only — no tokens debited.
        let (_, status) = get(&b, "/status");
        assert_eq!(json_val(&status)["tokens_spent"], 0);

        // Recorded in the audit log.
        let (_, events) = get(&b, "/events");
        let harness = events
            .lines()
            .map(json_val)
            .find(|e| e["event"] == "HarnessIdentified")
            .expect("HarnessIdentified event must appear in /events");
        assert_eq!(harness["name"], "opencode");
        assert_eq!(harness["version"], "0.3.2");
    }

    #[test]
    fn harness_without_version_omits_it() {
        let b = started(1000);
        post(&b, "/harness", r#"{"name":"langgraph"}"#);
        let (_, events) = get(&b, "/events");
        let harness = events
            .lines()
            .map(json_val)
            .find(|e| e["event"] == "HarnessIdentified")
            .expect("HarnessIdentified event must appear");
        assert_eq!(harness["name"], "langgraph");
        assert!(harness.get("version").is_none());
    }

    #[test]
    fn harness_empty_name_rejected() {
        let b = started(1000);
        let (s, _) = post(&b, "/harness", r#"{"name":"  "}"#);
        assert_eq!(s, 400);
        let (_, events) = get(&b, "/events");
        assert!(!events.contains("HarnessIdentified"));
    }

    #[test]
    fn app_records_identity_and_dedups() {
        let b = started(1000);
        let (s, body) = post(&b, "/app", r#"{"app_id":"app_abc","name":"gotm-nanny"}"#);
        assert_eq!(s, 200);
        assert!(body.contains("\"ok\""));

        // Re-declaring the same identity must not append a second event: a
        // caller is allowed to (re)declare on every request.
        post(&b, "/app", r#"{"app_id":"app_abc","name":"gotm-nanny"}"#);

        let (_, events) = get(&b, "/events");
        let identified: Vec<_> = events
            .lines()
            .map(json_val)
            .filter(|e| e["event"] == "AppIdentified")
            .collect();
        assert_eq!(identified.len(), 1, "identical app must be deduped");
        assert_eq!(identified[0]["app_id"], "app_abc");
        assert_eq!(identified[0]["name"], "gotm-nanny");
    }

    #[test]
    fn app_emits_again_when_the_identity_changes() {
        // The case that matters for a governor: several apps join one server,
        // and each must be attributable rather than folded into the first.
        let b = started(1000);
        post(&b, "/app", r#"{"app_id":"app_one","name":"first"}"#);
        post(&b, "/app", r#"{"app_id":"app_two","name":"second"}"#);
        let (_, events) = get(&b, "/events");
        let ids: Vec<String> = events
            .lines()
            .map(json_val)
            .filter(|e| e["event"] == "AppIdentified")
            .map(|e| e["app_id"].as_str().unwrap().to_string())
            .collect();
        assert_eq!(ids, vec!["app_one", "app_two"]);
    }

    #[test]
    fn app_renamed_keeps_the_same_id_and_records_the_new_label() {
        // `name` is a display label and is free to change; `app_id` is the
        // identity. A rename is a real event, not a new app.
        let b = started(1000);
        post(&b, "/app", r#"{"app_id":"app_abc","name":"old-name"}"#);
        post(&b, "/app", r#"{"app_id":"app_abc","name":"new-name"}"#);
        let (_, events) = get(&b, "/events");
        let names: Vec<String> = events
            .lines()
            .map(json_val)
            .filter(|e| e["event"] == "AppIdentified")
            .map(|e| e["name"].as_str().unwrap().to_string())
            .collect();
        assert_eq!(names, vec!["old-name", "new-name"]);
    }

    #[test]
    fn app_empty_id_rejected() {
        let b = started(1000);
        let (s, _) = post(&b, "/app", r#"{"app_id":"  ","name":"whatever"}"#);
        assert_eq!(s, 400);
        let (_, events) = get(&b, "/events");
        assert!(!events.contains("AppIdentified"));
    }

    #[test]
    fn app_empty_name_is_allowed() {
        // The id identifies; the name is only a label, so a blank one is not a
        // malformed request the way a blank id is.
        let b = started(1000);
        let (s, _) = post(&b, "/app", r#"{"app_id":"app_abc","name":""}"#);
        assert_eq!(s, 200);
        let (_, events) = get(&b, "/events");
        assert!(events.contains("AppIdentified"));
    }

    #[test]
    fn app_identity_never_touches_enforcement() {
        // Attribution only: declaring an app must not consume tokens or calls.
        let b = started(1000);
        let before = b.metrics();
        post(&b, "/app", r#"{"app_id":"app_abc","name":"gotm-nanny"}"#);
        let after = b.metrics();
        assert_eq!(before.tokens_spent, after.tokens_spent, "must not charge tokens");
        assert_eq!(before.tool_call_count, after.tool_call_count, "must not count a tool call");
    }

    #[test]
    fn llm_usage_carries_harness_and_dedups() {
        let b = started(1000);
        // Harness rides on the usage report (the "every request" path).
        post(&b, "/llm/usage", r#"{"input":5,"output":5,"harness":{"name":"opencode"}}"#);
        // Same harness again — must NOT emit a second HarnessIdentified.
        post(&b, "/llm/usage", r#"{"input":5,"output":5,"harness":{"name":"opencode"}}"#);
        let (_, events) = get(&b, "/events");
        let harness_events = events
            .lines()
            .map(json_val)
            .filter(|e| e["event"] == "HarnessIdentified")
            .count();
        assert_eq!(harness_events, 1, "identical harness must be deduped");
        let identified = events
            .lines()
            .map(json_val)
            .find(|e| e["event"] == "HarnessIdentified")
            .expect("one HarnessIdentified");
        assert_eq!(identified["name"], "opencode");
    }

    #[test]
    fn llm_usage_records_without_stopping_execution() {
        let b = started(50);
        let (s, body) = post(&b, "/llm/usage", r#"{"input":40,"output":20}"#);
        assert_eq!(s, 200);
        assert_eq!(json_val(&body)["status"], "ok");
        assert!(matches!(b.execution_state(), ExecutionState::Running));

        let (_, status) = get(&b, "/status");
        assert_eq!(json_val(&status)["tokens_spent"], 60);
    }

    #[test]
    fn llm_usage_zero_tokens_is_noop() {
        let b = started(1000);
        let (s, body) = post(&b, "/llm/usage", r#"{"input":0,"output":0}"#);
        assert_eq!(s, 200);
        assert_eq!(json_val(&body)["status"], "ok");
        // No debit, no event.
        let (_, status) = get(&b, "/status");
        assert_eq!(json_val(&status)["tokens_spent"], 0);
        let (_, events) = get(&b, "/events");
        assert!(!events.contains("LlmUsageRecorded"));
    }

    #[test]
    fn status_returns_running_with_counters() {
        let b = started(1000);
        post(&b, "/tool/call", r#"{"tool":"echo","args":{}}"#);
        post(&b, "/tool/call", r#"{"tool":"echo","args":{}}"#);
        let (s, body) = get(&b, "/status");
        assert_eq!(s, 200);
        let v = json_val(&body);
        assert_eq!(v["state"], "running");
        assert_eq!(v["tool_call_counts"]["echo"], 2);
        assert_eq!(v["tokens_spent"], 20);
    }

    #[test]
    fn status_available_after_stop() {
        let b = started(1000);
        b.stop("ToolDenied");
        let (s, _) = get(&b, "/status");
        assert_eq!(s, 200);
    }

    #[test]
    fn events_contains_tool_allowed_after_call() {
        let b = started(1000);
        post(&b, "/tool/call", r#"{"tool":"echo","args":{}}"#);
        let (_, body) = get(&b, "/events");
        let events: Vec<serde_json::Value> = body.lines()
            .filter_map(|l| serde_json::from_str(l).ok())
            .collect();
        assert!(events.iter().any(|v| v["event"] == "ToolAllowed"));
    }

    #[test]
    fn tool_failure_emits_tool_failed_event() {
        // FailingTool always returns Err — the bridge must emit ToolFailed to /events
        // and must NOT stop execution (tool failure is an audit event, not a hard stop).
        let mut registry = ToolRegistry::new();
        registry.register(Box::new(FailingTool));
        let b = Bridge::start(BridgeComponents {
            registry,
            allowed_tools: vec!["fail".to_string()],
            per_tool_max_calls: Default::default(),
            tool_labels: Default::default(),
        }).unwrap();
        std::thread::sleep(std::time::Duration::from_millis(20));

        let (status, _body) = post(&b, "/tool/call", r#"{"tool":"fail","args":{}}"#);
        assert_eq!(status, 500, "tool execution failure must return 500");

        let (_, events_body) = get(&b, "/events");
        let events: Vec<serde_json::Value> = events_body.lines()
            .filter_map(|l| serde_json::from_str(l).ok())
            .collect();
        assert!(
            events.iter().any(|v| v["event"] == "ToolFailed"),
            "ToolFailed event must appear in /events after a tool execution error\ngot: {events_body}"
        );
        // Execution must NOT have stopped — ToolFailed is audit-only.
        assert!(
            matches!(b.execution_state(), ExecutionState::Running),
            "execution must remain running after a tool failure"
        );
    }

    #[test]
    fn events_do_not_contain_execution_stopped_from_bridge() {
        // ExecutionStopped is emitted by the CLI, not the bridge.
        // The bridge's event stream must never contain it.
        let b = started(1000);
        b.stop("ManualStop");
        std::thread::sleep(std::time::Duration::from_millis(10));
        let (_, body) = get(&b, "/events");
        let has_stopped = body.lines()
            .filter_map(|l| serde_json::from_str::<serde_json::Value>(l).ok())
            .any(|v| v["event"] == "ExecutionStopped");
        assert!(!has_stopped, "bridge must not emit ExecutionStopped — that is the CLI's job");
    }

    #[test]
    fn events_available_after_stop() {
        let b = started(1000);
        b.stop("ToolDenied");
        // /events must work even after execution stops
        assert_eq!(get(&b, "/events").0, 200);
    }

    #[test]
    fn stop_is_idempotent() {
        let b = started(1000);
        b.stop("ToolDenied");
        b.stop("ManualStop"); // second call is ignored
        // Reason is from the first stop, not the second
        assert_eq!(
            json_val(&get(&b, "/health").1)["reason"],
            "ToolDenied"
        );
    }

    // ── Day 7 — Security ──────────────────────────────────────────────────────

    /// On Unix: the bridge uses a socket file — no port, no conflicts.
    /// On Windows: the bridge binds to loopback and the port is reachable.
    #[test]
    fn bridge_has_valid_and_reachable_address() {
        let b = started(1000);
        match &b.address {
            #[cfg(unix)]
            BridgeAddress::Unix(path) => {
                assert!(path.exists(), "socket file must exist after start");
                // Reachable
                let conn = std::os::unix::net::UnixStream::connect(path);
                assert!(conn.is_ok(), "Unix socket must be connectable");
            }
            BridgeAddress::Tcp(port) => {
                assert!(*port > 0);
                let conn = std::net::TcpStream::connect(("127.0.0.1", *port));
                assert!(conn.is_ok(), "TCP loopback must be connectable");
            }
        }
    }

    /// Socket file is cleaned up when the Bridge is dropped.
    #[cfg(unix)]
    #[test]
    fn socket_file_is_removed_on_drop() {
        let path = {
            let b = started(1000);
            let BridgeAddress::Unix(ref p) = b.address else { panic!("expected Unix") };
            p.clone()
        }; // bridge dropped here
        assert!(!path.exists(), "socket file must be removed on drop");
    }

    /// Action endpoints return 410 once execution is stopped.
    #[test]
    fn action_endpoints_return_410_after_stop() {
        let b = started(1000);
        b.stop("ToolDenied");
        std::thread::sleep(std::time::Duration::from_millis(10));

        for (path, body) in &[
            ("/tool/call",     r#"{"tool":"echo","args":{}}"#),
            ("/rule/evaluate", "{}"),
            ("/agent/enter",   r#"{"name":"researcher"}"#),
            ("/agent/exit",    "{}"),
            ("/llm/usage",     r#"{"input":1,"output":1}"#),
        ] {
            let (status, _) = post(&b, path, body);
            assert_eq!(status, 410, "POST {path} must return 410 when stopped");
        }
    }

    /// Read-only endpoints remain available after stop.
    #[test]
    fn read_endpoints_available_after_stop() {
        let b = started(1000);
        b.stop("ToolDenied");
        for path in &["/health", "/status", "/events"] {
            assert_eq!(get(&b, path).0, 200, "{path} must stay available after stop");
        }
    }

    /// Wrong token is always 401, even after stop.
    #[test]
    fn stale_token_is_rejected_after_stop() {
        let b = started(1000);
        b.stop("AgentCompleted");
        let (status, _) = http_get(&b.address, "wrong-token", "/health");
        assert_eq!(status, 401);
    }

    // ── drain_events ──────────────────────────────────────────────────────────

    /// drain_events returns accumulated lines and clears the internal buffer.
    #[test]
    fn drain_events_returns_and_clears_events() {
        let b = started(1000);

        // Trigger a real tool call to produce a ToolAllowed event.
        post(&b, "/tool/call", r#"{"tool":"echo","args":{}}"#);
        std::thread::sleep(std::time::Duration::from_millis(10));

        let lines = b.drain_events();
        assert!(!lines.is_empty(), "drain must return at least one event after a tool call");

        // Every line must be valid JSON with an "event" field.
        for line in &lines {
            let v: serde_json::Value = serde_json::from_str(line)
                .unwrap_or_else(|_| panic!("drain_events line is not valid JSON: {line}"));
            assert!(v.get("event").is_some(), "every drained line must have an 'event' field");
        }

        // Buffer is now empty.
        let lines2 = b.drain_events();
        assert!(lines2.is_empty(), "second drain must return nothing");
    }

    /// drain_events after a tool call contains a ToolAllowed event.
    #[test]
    fn drain_events_contains_tool_allowed_after_call() {
        let b = started_with_tools(vec!["search_web".to_string()], 1000);
        post(&b, "/tool/call", r#"{"tool":"search_web","tokens":10}"#);
        std::thread::sleep(std::time::Duration::from_millis(10));

        let lines = b.drain_events();
        let has_tool_allowed = lines.iter().any(|l| {
            serde_json::from_str::<serde_json::Value>(l)
                .map(|v| v["event"] == "ToolAllowed")
                .unwrap_or(false)
        });
        assert!(has_tool_allowed, "drain must contain ToolAllowed after an allowed tool call");
    }

    // ── handle_stop — RuleDenied event emission ───────────────────────────────

    /// POST /stop with RuleDenied + tool + rule_name emits a RuleDenied event.
    ///
    /// Client-side rule denials never reach /tool/call, so the bridge must emit
    /// the event here using the metadata the SDK sends in the /stop payload.
    #[test]
    fn handle_stop_rule_denied_with_metadata_emits_rule_denied_event() {
        let b = started(1000);
        post(
            &b,
            "/stop",
            r#"{"reason":"RuleDenied","tool":"read_file","rule_name":"no_sensitive_files"}"#,
        );
        std::thread::sleep(std::time::Duration::from_millis(10));

        let lines = b.drain_events();
        let event = lines.iter().find_map(|l| {
            let v: serde_json::Value = serde_json::from_str(l).ok()?;
            if v["event"] == "RuleDenied" { Some(v) } else { None }
        });

        let event = event.expect("drain must contain a RuleDenied event after /stop with metadata");
        assert_eq!(event["tool"],      "read_file",           "tool field must match");
        assert_eq!(event["rule_name"], "no_sensitive_files",  "rule_name field must match");
        assert!(event["ts"].is_number(), "ts field must be present");
    }

    /// POST /stop with RuleDenied but no tool or rule_name does not emit an event.
    ///
    /// The bridge cannot construct a meaningful RuleDenied event without both
    /// fields — omitting the event is safer than emitting one with empty fields.
    #[test]
    fn handle_stop_rule_denied_without_metadata_emits_no_rule_denied_event() {
        let b = started(1000);
        post(&b, "/stop", r#"{"reason":"RuleDenied"}"#);
        std::thread::sleep(std::time::Duration::from_millis(10));

        let lines = b.drain_events();
        let has_rule_denied = lines.iter().any(|l| {
            serde_json::from_str::<serde_json::Value>(l)
                .map(|v| v["event"] == "RuleDenied")
                .unwrap_or(false)
        });
        assert!(!has_rule_denied, "no RuleDenied event must be emitted when tool/rule_name are absent");
    }

    /// POST /stop with RuleDenied and empty-string fields does not emit an event.
    #[test]
    fn handle_stop_rule_denied_empty_fields_emits_no_rule_denied_event() {
        let b = started(1000);
        post(&b, "/stop", r#"{"reason":"RuleDenied","tool":"","rule_name":""}"#);
        std::thread::sleep(std::time::Duration::from_millis(10));

        let lines = b.drain_events();
        let has_rule_denied = lines.iter().any(|l| {
            serde_json::from_str::<serde_json::Value>(l)
                .map(|v| v["event"] == "RuleDenied")
                .unwrap_or(false)
        });
        assert!(!has_rule_denied, "no RuleDenied event must be emitted when tool/rule_name are empty strings");
    }

    /// POST /stop with a non-RuleDenied reason never emits a RuleDenied event.
    #[test]
    fn handle_stop_other_reason_emits_no_rule_denied_event() {
        let b = started(1000);
        post(&b, "/stop", r#"{"reason":"AgentCompleted","tool":"read_file","rule_name":"some_rule"}"#);
        std::thread::sleep(std::time::Duration::from_millis(10));

        let lines = b.drain_events();
        let has_rule_denied = lines.iter().any(|l| {
            serde_json::from_str::<serde_json::Value>(l)
                .map(|v| v["event"] == "RuleDenied")
                .unwrap_or(false)
        });
        assert!(!has_rule_denied, "RuleDenied event must only fire for RuleDenied reason, not AgentCompleted");
    }
}
