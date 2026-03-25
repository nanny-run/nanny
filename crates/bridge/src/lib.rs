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

use nanny_core::agent::limits::Limits;
use nanny_core::agent::state::StopReason;
use nanny_core::ledger::Ledger;
use nanny_core::policy::{Policy, PolicyContext, PolicyDecision};
use nanny_core::tool::{ToolArgs, ToolCallError, ToolExecutor};
use nanny_ledger::FakeLedger;
use nanny_policy::{LimitsPolicy, RuleEvaluator};
use nanny_tools::ToolRegistry;

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

// ── BridgeAddress ─────────────────────────────────────────────────────────────

/// How the child process reaches the bridge.
///
/// On Unix (macOS / Linux): a Unix domain socket. No port, no conflicts.
///   Inject `NANNY_BRIDGE_SOCKET` into the child environment.
///
/// On Windows: TCP loopback on an OS-assigned port.
///   Inject `NANNY_BRIDGE_PORT` into the child environment.
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
    pub limits: Limits,
    /// All named limits sets, pre-resolved with inheritance applied.
    /// Used by `POST /agent/enter` to switch active limits.
    pub named_limits: HashMap<String, Limits>,
    pub allowed_tools: Vec<String>,
    /// Per-tool max call counts from `[tools.<name>] max_calls`.
    pub per_tool_max_calls: HashMap<String, u32>,
}

// ── Internal state ────────────────────────────────────────────────────────────

struct BridgeState {
    session_token: String,
    execution: ExecutionState,

    // Enforcement — stored separately so /rule/evaluate can access
    // rule_evaluator directly without evaluating the full limits chain.
    limits_policy: LimitsPolicy,
    rule_evaluator: RuleEvaluator,
    ledger: FakeLedger,

    // Agent context switching ─────────────────────────────────────────────────
    default_limits: Limits,
    current_limits: Limits,
    named_limits: HashMap<String, Limits>,
    limits_stack: Vec<Limits>,
    allowed_tools: Vec<String>,

    // Execution tracking ──────────────────────────────────────────────────────
    cost_units_spent: u64,
    tool_call_counts: HashMap<String, u32>,
    tool_call_history: Vec<String>,
    step_count: u32,
    start_time: std::time::Instant,

    // Append-only event log ───────────────────────────────────────────────────
    events: Vec<String>,
}

// ── Bridge ────────────────────────────────────────────────────────────────────

/// A running bridge instance.
///
/// Inject `address` and `session_token` into the child process environment
/// before spawning it. On Unix set `NANNY_BRIDGE_SOCKET`; on Windows set
/// `NANNY_BRIDGE_PORT`. Always set `NANNY_SESSION_TOKEN`.
pub struct Bridge {
    shared: Arc<Mutex<BridgeState>>,
    // ToolRegistry is read-only after start — kept outside the Mutex so tool
    // execution never blocks state mutations (e.g. CLI calling stop()).
    #[allow(dead_code)]
    registry: Arc<ToolRegistry>,
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

        let limits_policy = LimitsPolicy::new(
            components.limits.clone(),
            components.allowed_tools.clone(),
        );
        let rule_evaluator = RuleEvaluator::new(components.per_tool_max_calls);
        let max_cost = components.limits.max_cost_units;

        let shared = Arc::new(Mutex::new(BridgeState {
            session_token: token.clone(),
            execution: ExecutionState::Running,
            limits_policy,
            rule_evaluator,
            ledger: FakeLedger::new(max_cost),
            default_limits: components.limits.clone(),
            current_limits: components.limits.clone(),
            named_limits: components.named_limits,
            limits_stack: Vec::new(),
            allowed_tools: components.allowed_tools,
            cost_units_spent: 0,
            tool_call_counts: HashMap::new(),
            tool_call_history: Vec::new(),
            step_count: 0,
            start_time: std::time::Instant::now(),
            events: Vec::new(),
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
        registry,
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
    let server = tiny_http::Server::http("127.0.0.1:47374")
        .map_err(|e| BridgeError::Start(e.to_string()))?;

    {
        let shared = shared.clone();
        let registry = registry.clone();
        std::thread::spawn(move || serve_tcp(server, shared, registry));
    }

    Ok(Bridge {
        shared,
        registry,
        address: BridgeAddress::Tcp(47374),
        session_token: token,
    })
}

impl Bridge {

    /// Read the current execution state.
    pub fn execution_state(&self) -> ExecutionState {
        self.shared.lock().unwrap().execution.clone()
    }

    /// Mark the execution as stopped with the given reason.
    ///
    /// Idempotent — calling twice does nothing after the first stop.
    pub fn stop(&self, reason: impl Into<String>) {
        let reason: String = reason.into();
        let mut guard = self.shared.lock().unwrap();
        mark_stopped(&mut guard, &reason);
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

enum ContentType {
    Json,
    Ndjson,
}

struct BridgeResp {
    status: u16,
    body: String,
    content_type: ContentType,
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

    // All action endpoints return 410 Gone once execution has stopped.
    if is_stopped(shared) {
        return BridgeResp::json(410, r#"{"error":"execution stopped"}"#);
    }

    match (method, path) {
        ("POST", "/tool/call")     => handle_tool_call(&req.body, shared, registry),
        ("POST", "/rule/evaluate") => handle_rule_evaluate(&req.body, shared),
        ("POST", "/agent/enter")   => handle_agent_enter(&req.body, shared),
        ("POST", "/agent/exit")    => handle_agent_exit(shared),
        ("POST", "/step")          => handle_step(shared),
        _                          => BridgeResp::json(404, r#"{"error":"Not Found"}"#),
    }
}

// ── Handlers (transport-agnostic) ─────────────────────────────────────────────

fn handle_health(shared: &Arc<Mutex<BridgeState>>) -> BridgeResp {
    let guard = shared.lock().unwrap();
    let body = match &guard.execution {
        ExecutionState::Running =>
            r#"{"state":"running"}"#.to_string(),
        ExecutionState::Stopped { reason } =>
            format!(r#"{{"state":"stopped","reason":"{}"}}"#, reason),
    };
    BridgeResp::json(200, body)
}

fn handle_status(shared: &Arc<Mutex<BridgeState>>) -> BridgeResp {
    let guard = shared.lock().unwrap();
    let elapsed_ms = guard.start_time.elapsed().as_millis() as u64;
    let body = match &guard.execution {
        ExecutionState::Running => format!(
            r#"{{"state":"running","step":{},"cost_spent":{},"elapsed_ms":{}}}"#,
            guard.step_count, guard.cost_units_spent, elapsed_ms
        ),
        ExecutionState::Stopped { reason } => format!(
            r#"{{"state":"stopped","reason":"{}","step":{},"cost_spent":{},"elapsed_ms":{}}}"#,
            reason, guard.step_count, guard.cost_units_spent, elapsed_ms
        ),
    };
    BridgeResp::json(200, body)
}

fn handle_events(shared: &Arc<Mutex<BridgeState>>) -> BridgeResp {
    let guard = shared.lock().unwrap();
    BridgeResp::ndjson(guard.events.join("\n"))
}

fn handle_tool_call(
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
            step_count: guard.step_count,
            elapsed_ms,
            requested_tool: Some(call.tool.clone()),
            cost_units_spent: guard.cost_units_spent,
            tool_call_counts: guard.tool_call_counts.clone(),
            tool_call_history: guard.tool_call_history.clone(),
        };
        // Chain: limits first, then per-tool rules.
        match guard.limits_policy.evaluate(&ctx) {
            PolicyDecision::Allow => guard.rule_evaluator.evaluate(&ctx),
            deny => deny,
        }
    };

    match decision {
        PolicyDecision::Deny { ref reason } => {
            let reason_name = stop_reason_name(reason).to_string();
            {
                let mut guard = shared.lock().unwrap();
                append_event(&mut guard, serde_json::json!({
                    "event": "ToolDenied",
                    "ts": now_ms(),
                    "tool": &call.tool,
                    "reason": &reason_name
                }));
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
                    // The bridge just charges the declared cost and records the call.
                    let cost = call.cost.unwrap_or(0);
                    {
                        let mut guard = shared.lock().unwrap();
                        let _ = guard.ledger.debit(cost);
                        guard.cost_units_spent += cost;
                        *guard.tool_call_counts.entry(call.tool.clone()).or_insert(0) += 1;
                        guard.tool_call_history.push(call.tool.clone());
                        append_event(&mut guard, serde_json::json!({
                            "event": "ToolAllowed",
                            "ts": now_ms(),
                            "tool": &call.tool
                        }));
                        if guard.cost_units_spent >= guard.current_limits.max_cost_units {
                            mark_stopped(&mut guard, "BudgetExhausted");
                        }
                    }
                    BridgeResp::json(200, serde_json::to_string(
                        &ToolCallResponse::Allowed { result: String::new() }
                    ).unwrap())
                }
                Err(ToolCallError::Execution { tool_name, source }) => {
                    BridgeResp::json(500, format!(
                        r#"{{"error":"tool execution failed","tool_name":"{}","message":"{}"}}"#,
                        tool_name, source
                    ))
                }
                Ok(output) => {
                    {
                        let mut guard = shared.lock().unwrap();
                        let _ = guard.ledger.debit(cost);
                        guard.cost_units_spent += cost;
                        *guard.tool_call_counts.entry(call.tool.clone()).or_insert(0) += 1;
                        guard.tool_call_history.push(call.tool.clone());
                        append_event(&mut guard, serde_json::json!({
                            "event": "ToolAllowed",
                            "ts": now_ms(),
                            "tool": &call.tool
                        }));
                    }
                    BridgeResp::json(200, serde_json::to_string(
                        &ToolCallResponse::Allowed { result: output.content }
                    ).unwrap())
                }
            }
        }
    }
}

fn handle_rule_evaluate(body: &[u8], shared: &Arc<Mutex<BridgeState>>) -> BridgeResp {
    let req: RuleEvalRequest = serde_json::from_slice(body).unwrap_or_default();

    let decision = {
        let guard = shared.lock().unwrap();
        let ctx = PolicyContext {
            step_count: req.step.unwrap_or(guard.step_count),
            elapsed_ms: req.elapsed
                .unwrap_or_else(|| guard.start_time.elapsed().as_millis() as u64),
            requested_tool: req.tool.clone(),
            cost_units_spent: req.cost_spent.unwrap_or(guard.cost_units_spent),
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

fn handle_agent_enter(body: &[u8], shared: &Arc<Mutex<BridgeState>>) -> BridgeResp {
    let req: AgentEnterRequest = match serde_json::from_slice(body) {
        Ok(b) => b,
        Err(_) => return BridgeResp::json(400, r#"{"error":"invalid request body"}"#),
    };

    let result = {
        let mut guard = shared.lock().unwrap();
        if let Some(new_limits) = guard.named_limits.get(&req.name).cloned() {
            let prev = guard.current_limits.clone();
            guard.limits_stack.push(prev);
            guard.current_limits = new_limits.clone();
            guard.limits_policy =
                LimitsPolicy::new(new_limits.clone(), guard.allowed_tools.clone());
            Ok(new_limits)
        } else {
            Err(req.name.clone())
        }
    };

    match result {
        Ok(limits) => BridgeResp::json(200, format!(
            r#"{{"status":"ok","limits":{{"steps":{},"cost":{},"timeout":{}}}}}"#,
            limits.max_steps, limits.max_cost_units, limits.timeout_ms
        )),
        Err(name) => BridgeResp::json(404, format!(
            r#"{{"error":"named limits set '{}' not found"}}"#, name
        )),
    }
}

fn handle_agent_exit(shared: &Arc<Mutex<BridgeState>>) -> BridgeResp {
    let mut guard = shared.lock().unwrap();
    let prev = guard.limits_stack.pop().unwrap_or_else(|| guard.default_limits.clone());
    guard.current_limits = prev.clone();
    guard.limits_policy = LimitsPolicy::new(prev, guard.allowed_tools.clone());
    BridgeResp::json(200, r#"{"status":"ok"}"#)
}

fn handle_step(shared: &Arc<Mutex<BridgeState>>) -> BridgeResp {
    let mut guard = shared.lock().unwrap();
    guard.step_count += 1;

    let elapsed_ms = guard.start_time.elapsed().as_millis() as u64;
    let ctx = PolicyContext {
        step_count: guard.step_count,
        elapsed_ms,
        requested_tool: None,
        cost_units_spent: guard.cost_units_spent,
        tool_call_counts: guard.tool_call_counts.clone(),
        tool_call_history: guard.tool_call_history.clone(),
    };

    let step_now = guard.step_count;
    append_event(&mut guard, serde_json::json!({
        "event": "StepCompleted",
        "ts": now_ms(),
        "step": step_now
    }));

    match guard.limits_policy.evaluate(&ctx) {
        PolicyDecision::Deny { reason } => {
            let name = stop_reason_name(&reason).to_string();
            mark_stopped(&mut guard, &name);
            BridgeResp::json(200, format!(
                r#"{{"status":"stopped","reason":"{}","step":{}}}"#,
                name, guard.step_count
            ))
        }
        PolicyDecision::Allow => BridgeResp::json(
            200,
            format!(r#"{{"status":"running","step":{}}}"#, guard.step_count),
        ),
    }
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
    /// Cost declared by the macro at the call site.
    /// Used when the tool is not registered in the bridge registry (user-defined tools).
    #[serde(default)]
    cost: Option<u64>,
}

#[derive(serde::Deserialize, Default)]
struct RuleEvalRequest {
    #[serde(default)] step:              Option<u32>,
    #[serde(default)] elapsed:           Option<u64>,
    #[serde(default)] tool:              Option<String>,
    #[serde(default)] tool_call_counts:  HashMap<String, u32>,
    #[serde(default)] tool_call_history: Vec<String>,
    #[serde(default)] cost_spent:        Option<u64>,
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
        StopReason::MaxStepsReached   => "MaxStepsReached",
        StopReason::BudgetExhausted   => "BudgetExhausted",
        StopReason::TimeoutExpired    => "TimeoutExpired",
        StopReason::ToolDenied { .. } => "ToolDenied",
        StopReason::RuleDenied { .. } => "RuleDenied",
        StopReason::ManualStop        => "ManualStop",
        StopReason::AgentCompleted    => "AgentCompleted",
    }
}

// ── State helpers ─────────────────────────────────────────────────────────────

/// Mark execution as stopped and emit an `ExecutionStopped` event.
/// Idempotent — does nothing if already stopped.
fn mark_stopped(state: &mut BridgeState, reason: &str) {
    if matches!(state.execution, ExecutionState::Stopped { .. }) {
        return;
    }
    let elapsed_ms = state.start_time.elapsed().as_millis() as u64;
    state.execution = ExecutionState::Stopped { reason: reason.to_string() };
    append_event(state, serde_json::json!({
        "event": "ExecutionStopped",
        "ts": now_ms(),
        "reason": reason,
        "steps": state.step_count,
        "cost_spent": state.cost_units_spent,
        "elapsed_ms": elapsed_ms
    }));
}

fn append_event(state: &mut BridgeState, event: serde_json::Value) {
    if let Ok(s) = serde_json::to_string(&event) {
        state.events.push(s);
    }
}

fn now_ms() -> u64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

fn is_stopped(shared: &Arc<Mutex<BridgeState>>) -> bool {
    matches!(shared.lock().unwrap().execution, ExecutionState::Stopped { .. })
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

    fn echo_components(max_cost: u64) -> BridgeComponents {
        let mut registry = ToolRegistry::new();
        registry.register(Box::new(EchoTool));
        BridgeComponents {
            registry,
            limits: Limits { max_steps: 100, max_cost_units: max_cost, timeout_ms: 30_000 },
            named_limits: Default::default(),
            allowed_tools: vec!["echo".to_string()],
            per_tool_max_calls: Default::default(),
        }
    }

    fn started(max_cost: u64) -> Bridge {
        let b = Bridge::start(echo_components(max_cost)).unwrap();
        // Small pause to let the server thread reach accept().
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
        b.stop("TimeoutExpired");
        std::thread::sleep(std::time::Duration::from_millis(10));
        let (s, body) = get(&b, "/health");
        assert_eq!(s, 200);
        let v = json_val(&body);
        assert_eq!(v["state"], "stopped");
        assert_eq!(v["reason"], "TimeoutExpired");
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
        assert_eq!(v["cost_spent"], 20); // 2 calls × cost 10
    }

    #[test]
    fn denied_tool_returns_denied_with_tool_name() {
        let b = Bridge::start(BridgeComponents {
            registry: ToolRegistry::new(),
            limits: Limits { max_steps: 100, max_cost_units: 1000, timeout_ms: 30_000 },
            named_limits: Default::default(),
            allowed_tools: vec![],   // empty allowlist — all tools denied
            per_tool_max_calls: Default::default(),
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
            limits: Limits { max_steps: 100, max_cost_units: 1000, timeout_ms: 30_000 },
            named_limits: Default::default(),
            allowed_tools: vec![],
            per_tool_max_calls: Default::default(),
        }).unwrap();
        std::thread::sleep(std::time::Duration::from_millis(20));

        post(&b, "/tool/call", r#"{"tool":"echo","args":{}}"#);
        assert!(matches!(b.execution_state(), ExecutionState::Stopped { .. }));
    }

    #[test]
    fn budget_exhaustion_stops_execution_and_returns_denied() {
        let b = started(10); // budget = 10, echo costs 10
        let (_, body) = post(&b, "/tool/call", r#"{"tool":"echo","args":{"message":"x"}}"#);
        let v = json_val(&body);
        // First call succeeds, charges 10, exhausts budget.
        // Subsequent calls see BudgetExhausted.
        let _ = post(&b, "/tool/call", r#"{"tool":"echo","args":{"message":"y"}}"#);
        assert!(matches!(b.execution_state(), ExecutionState::Stopped { .. }));
        drop(v);
    }

    #[test]
    fn tool_call_on_stopped_execution_returns_410() {
        let b = started(1000);
        b.stop("TimeoutExpired");
        std::thread::sleep(std::time::Duration::from_millis(10));
        let (s, _) = post(&b, "/tool/call", r#"{"tool":"echo","args":{}}"#);
        assert_eq!(s, 410);
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
            limits: Limits { max_steps: 100, max_cost_units: 10_000, timeout_ms: 30_000 },
            named_limits: Default::default(),
            allowed_tools: vec!["echo".to_string()],
            per_tool_max_calls,
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
            limits: Limits { max_steps: 100, max_cost_units: 1000, timeout_ms: 30_000 },
            named_limits: Default::default(),
            allowed_tools: vec!["echo".to_string()],
            per_tool_max_calls,
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
            limits: Limits { max_steps: 100, max_cost_units: 10_000, timeout_ms: 30_000 },
            named_limits: Default::default(),
            allowed_tools: vec!["echo".to_string()],
            per_tool_max_calls,
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
    fn rule_evaluate_on_stopped_execution_returns_410() {
        let b = started(1000);
        b.stop("ManualStop");
        std::thread::sleep(std::time::Duration::from_millis(10));
        let (s, _) = post(&b, "/rule/evaluate", "{}");
        assert_eq!(s, 410);
    }

    // ── Day 4 tests ───────────────────────────────────────────────────────────

    #[test]
    fn agent_enter_switches_limits() {
        let mut named = HashMap::new();
        named.insert("researcher".to_string(), Limits {
            max_steps: 200, max_cost_units: 5000, timeout_ms: 60_000,
        });
        let b = Bridge::start(BridgeComponents {
            registry: ToolRegistry::new(),
            limits: Limits { max_steps: 10, max_cost_units: 100, timeout_ms: 5_000 },
            named_limits: named,
            allowed_tools: vec![],
            per_tool_max_calls: Default::default(),
        }).unwrap();
        std::thread::sleep(std::time::Duration::from_millis(20));

        let (s, body) = post(&b, "/agent/enter", r#"{"name":"researcher"}"#);
        assert_eq!(s, 200);
        let v = json_val(&body);
        assert_eq!(v["status"], "ok");
        assert_eq!(v["limits"]["steps"], 200);
    }

    #[test]
    fn agent_exit_reverts_to_previous_limits() {
        let mut named = HashMap::new();
        named.insert("researcher".to_string(), Limits {
            max_steps: 200, max_cost_units: 5000, timeout_ms: 60_000,
        });
        let b = Bridge::start(BridgeComponents {
            registry: ToolRegistry::new(),
            limits: Limits { max_steps: 10, max_cost_units: 100, timeout_ms: 5_000 },
            named_limits: named,
            allowed_tools: vec![],
            per_tool_max_calls: Default::default(),
        }).unwrap();
        std::thread::sleep(std::time::Duration::from_millis(20));

        post(&b, "/agent/enter", r#"{"name":"researcher"}"#);
        let (s, _) = post(&b, "/agent/exit", "{}");
        assert_eq!(s, 200);

        // Verify limits reverted by checking step limit is back to 10.
        let guard = b.shared.lock().unwrap();
        assert_eq!(guard.current_limits.max_steps, 10);
    }

    #[test]
    fn agent_enter_missing_set_returns_404() {
        let b = started(1000);
        let (s, _) = post(&b, "/agent/enter", r#"{"name":"ghost"}"#);
        assert_eq!(s, 404);
    }

    #[test]
    fn nested_agent_enter_exit_round_trip() {
        let mut named = HashMap::new();
        named.insert("a".to_string(), Limits { max_steps: 50, max_cost_units: 200, timeout_ms: 10_000 });
        named.insert("b".to_string(), Limits { max_steps: 99, max_cost_units: 300, timeout_ms: 20_000 });
        let b = Bridge::start(BridgeComponents {
            registry: ToolRegistry::new(),
            limits: Limits { max_steps: 10, max_cost_units: 100, timeout_ms: 5_000 },
            named_limits: named,
            allowed_tools: vec![],
            per_tool_max_calls: Default::default(),
        }).unwrap();
        std::thread::sleep(std::time::Duration::from_millis(20));

        post(&b, "/agent/enter", r#"{"name":"a"}"#);
        post(&b, "/agent/enter", r#"{"name":"b"}"#);
        post(&b, "/agent/exit",  "{}");
        post(&b, "/agent/exit",  "{}");

        let guard = b.shared.lock().unwrap();
        assert_eq!(guard.current_limits.max_steps, 10); // back to root
    }

    // ── Day 5 tests ───────────────────────────────────────────────────────────

    #[test]
    fn step_increments_count_and_returns_running() {
        let b = started(1000);
        let (s, body) = post(&b, "/step", "{}");
        assert_eq!(s, 200);
        let v = json_val(&body);
        assert_eq!(v["status"], "running");
        assert_eq!(v["step"], 1);
    }

    #[test]
    fn step_stops_at_max_steps() {
        let b = Bridge::start(BridgeComponents {
            registry: ToolRegistry::new(),
            limits: Limits { max_steps: 1, max_cost_units: 1000, timeout_ms: 30_000 },
            named_limits: Default::default(),
            allowed_tools: vec![],
            per_tool_max_calls: Default::default(),
        }).unwrap();
        std::thread::sleep(std::time::Duration::from_millis(20));
        let (_, body) = post(&b, "/step", "{}");
        let v = json_val(&body);
        assert_eq!(v["status"], "stopped");
        assert_eq!(v["reason"], "MaxStepsReached");
        assert!(matches!(b.execution_state(), ExecutionState::Stopped { .. }));
    }

    #[test]
    fn status_returns_running_with_counters() {
        let b = started(1000);
        post(&b, "/step", "{}");
        post(&b, "/tool/call", r#"{"tool":"echo","args":{}}"#);
        let (s, body) = get(&b, "/status");
        assert_eq!(s, 200);
        let v = json_val(&body);
        assert_eq!(v["state"], "running");
        assert_eq!(v["step"], 1);
        assert_eq!(v["cost_spent"], 10);
    }

    #[test]
    fn status_available_after_stop() {
        let b = started(1000);
        b.stop("BudgetExhausted");
        let (s, _) = get(&b, "/status");
        assert_eq!(s, 200);
    }

    #[test]
    fn events_contains_step_completed_after_step() {
        let b = started(1000);
        post(&b, "/step", "{}");
        let (_, body) = get(&b, "/events");
        let events: Vec<serde_json::Value> = body.lines()
            .filter_map(|l| serde_json::from_str(l).ok())
            .collect();
        assert!(events.iter().any(|v| v["event"] == "StepCompleted"));
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
    fn events_contains_execution_stopped_after_stop() {
        let b = started(1000);
        b.stop("ManualStop");
        std::thread::sleep(std::time::Duration::from_millis(10));
        let (_, body) = get(&b, "/events");
        let events: Vec<serde_json::Value> = body.lines()
            .filter_map(|l| serde_json::from_str(l).ok())
            .collect();
        assert!(events.iter().any(|v| v["event"] == "ExecutionStopped"));
    }

    #[test]
    fn events_available_after_stop() {
        let b = started(1000);
        b.stop("TimeoutExpired");
        // /events must work even after execution stops
        assert_eq!(get(&b, "/events").0, 200);
    }

    #[test]
    fn stop_is_idempotent() {
        let b = started(1000);
        b.stop("TimeoutExpired");
        b.stop("ManualStop"); // second call is ignored
        // Events should have exactly one ExecutionStopped
        let (_, body) = get(&b, "/events");
        let count = body.lines()
            .filter_map(|l| serde_json::from_str::<serde_json::Value>(l).ok())
            .filter(|v| v["event"] == "ExecutionStopped")
            .count();
        assert_eq!(count, 1);
        // Reason is from the first stop, not the second
        assert_eq!(
            json_val(&get(&b, "/health").1)["reason"],
            "TimeoutExpired"
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
        b.stop("TimeoutExpired");
        std::thread::sleep(std::time::Duration::from_millis(10));

        for (path, body) in &[
            ("/tool/call",     r#"{"tool":"echo","args":{}}"#),
            ("/rule/evaluate", "{}"),
            ("/agent/enter",   r#"{"name":"researcher"}"#),
            ("/agent/exit",    "{}"),
            ("/step",          "{}"),
        ] {
            let (status, _) = post(&b, path, body);
            assert_eq!(status, 410, "POST {path} must return 410 when stopped");
        }
    }

    /// Read-only endpoints remain available after stop.
    #[test]
    fn read_endpoints_available_after_stop() {
        let b = started(1000);
        b.stop("BudgetExhausted");
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
}
