// nanny-bridge — local enforcement server.
//
// Runs as a background thread inside the `nanny run` process.
// The child process communicates with it over HTTP on loopback only.
//
// Listens on 127.0.0.1 exclusively — never 0.0.0.0.
// Every request must carry the session token in `X-Nanny-Session-Token`.
// The token is a UUID v4 generated fresh for each execution.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use tiny_http::{Request, Response, Server, StatusCode};
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

// ── BridgeComponents ──────────────────────────────────────────────────────────

/// Configuration the CLI passes to `Bridge::start`.
pub struct BridgeComponents {
    pub registry: ToolRegistry,
    pub limits: Limits,
    /// All named limits sets, pre-resolved with inheritance applied.
    /// Used by `POST /agent/enter` to switch active limits.
    pub named_limits: HashMap<String, Limits>,
    pub allowed_tools: Vec<String>,
    /// Per-tool max call counts from nanny.toml `[tools.<name>] max_calls`.
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

    // Agent context switching (Day 4) ─────────────────────────────────────────
    default_limits: Limits,
    current_limits: Limits,
    named_limits: HashMap<String, Limits>,
    limits_stack: Vec<Limits>, // pushed/popped on agent enter/exit
    allowed_tools: Vec<String>, // constant throughout — used to rebuild LimitsPolicy

    // Execution tracking — rebuilt into PolicyContext on every request ─────────
    cost_units_spent: u64,
    tool_call_counts: HashMap<String, u32>,
    tool_call_history: Vec<String>,
    step_count: u32,
    start_time: std::time::Instant,

    // Append-only event log (Day 5) ────────────────────────────────────────────
    events: Vec<String>,
}

// ── Bridge ────────────────────────────────────────────────────────────────────

/// A running bridge instance.
///
/// Inject `port` and `session_token` into the child process environment:
///   `NANNY_BRIDGE_PORT`   — the port to connect to on 127.0.0.1
///   `NANNY_SESSION_TOKEN` — must be sent as `X-Nanny-Session-Token` on every request
pub struct Bridge {
    shared: Arc<Mutex<BridgeState>>,
    // ToolRegistry is read-only after start — kept outside the Mutex so tool
    // execution never blocks state mutations (e.g. CLI calling stop()).
    // Used in Day 6 when /tool/call actually executes the tool.
    #[allow(dead_code)]
    registry: Arc<ToolRegistry>,
    /// Port the bridge is listening on (loopback only).
    pub port: u16,
    /// Session token the child process must present on every request.
    pub session_token: String,
}

impl Bridge {
    /// Start the bridge on a random loopback port.
    ///
    /// Binds before returning — `port` is ready to use immediately.
    /// The server loop runs in a background thread.
    pub fn start(components: BridgeComponents) -> Result<Self, BridgeError> {
        let token = Uuid::new_v4().to_string();

        let limits_policy = LimitsPolicy::new(
            components.limits.clone(),
            components.allowed_tools.clone(),
        );
        let rule_evaluator = RuleEvaluator::new(components.per_tool_max_calls);
        let max_cost = components.limits.max_cost_units;

        let server = Server::http("127.0.0.1:0")
            .map_err(|e| BridgeError::Start(e.to_string()))?;

        let tiny_http::ListenAddr::IP(addr) = server.server_addr() else {
            return Err(BridgeError::Start("non-IP address not supported".into()));
        };
        let port = addr.port();

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

        {
            let shared = shared.clone();
            let registry = registry.clone();
            std::thread::spawn(move || serve(server, shared, registry));
        }

        Ok(Bridge { shared, registry, port, session_token: token })
    }

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

// ── Server loop ───────────────────────────────────────────────────────────────

fn serve(server: Server, shared: Arc<Mutex<BridgeState>>, registry: Arc<ToolRegistry>) {
    for request in server.incoming_requests() {
        handle(request, &shared, &registry);
    }
}

fn handle(request: Request, shared: &Arc<Mutex<BridgeState>>, registry: &Arc<ToolRegistry>) {
    if !token_valid(&request, shared) {
        let _ = request.respond(json(401, r#"{"error":"Unauthorized"}"#));
        return;
    }

    let method = request.method().as_str();
    let url = request.url();

    // Read-only endpoints — available even after execution stops.
    match (method, url) {
        ("GET", "/health")  => { handle_health(request, shared);  return; }
        ("GET", "/status")  => { handle_status(request, shared);  return; }
        ("GET", "/events")  => { handle_events(request, shared);  return; }
        _ => {}
    }

    // All action endpoints return 410 Gone once execution has stopped.
    if is_stopped(shared) {
        let _ = request.respond(json(410, r#"{"error":"execution stopped"}"#));
        return;
    }

    match (method, url) {
        ("POST", "/tool/call")     => handle_tool_call(request, shared, registry),
        ("POST", "/rule/evaluate") => handle_rule_evaluate(request, shared),
        ("POST", "/agent/enter")   => handle_agent_enter(request, shared),
        ("POST", "/agent/exit")    => handle_agent_exit(request, shared),
        ("POST", "/step")          => handle_step(request, shared),
        _ => { let _ = request.respond(json(404, r#"{"error":"Not Found"}"#)); }
    }
}

// ── Handlers ──────────────────────────────────────────────────────────────────

fn handle_health(request: Request, shared: &Arc<Mutex<BridgeState>>) {
    let body = {
        let guard = shared.lock().unwrap();
        match &guard.execution {
            ExecutionState::Running =>
                r#"{"state":"running"}"#.to_string(),
            ExecutionState::Stopped { reason } =>
                format!(r#"{{"state":"stopped","reason":"{}"}}"#, reason),
        }
    };
    let _ = request.respond(json(200, &body));
}

fn handle_status(request: Request, shared: &Arc<Mutex<BridgeState>>) {
    let body = {
        let guard = shared.lock().unwrap();
        let elapsed_ms = guard.start_time.elapsed().as_millis() as u64;
        match &guard.execution {
            ExecutionState::Running => format!(
                r#"{{"state":"running","step":{},"cost_spent":{},"elapsed_ms":{}}}"#,
                guard.step_count, guard.cost_units_spent, elapsed_ms
            ),
            ExecutionState::Stopped { reason } => format!(
                r#"{{"state":"stopped","reason":"{}","step":{},"cost_spent":{},"elapsed_ms":{}}}"#,
                reason, guard.step_count, guard.cost_units_spent, elapsed_ms
            ),
        }
    };
    let _ = request.respond(json(200, &body));
}

fn handle_events(request: Request, shared: &Arc<Mutex<BridgeState>>) {
    let body = {
        let guard = shared.lock().unwrap();
        guard.events.join("\n")
    };
    let _ = request.respond(ndjson(&body));
}

fn handle_tool_call(
    mut request: Request,
    shared: &Arc<Mutex<BridgeState>>,
    registry: &Arc<ToolRegistry>,
) {
    let call: ToolCallRequest = match serde_json::from_reader(request.as_reader()) {
        Ok(b) => b,
        Err(_) => {
            let _ = request.respond(json(400, r#"{"error":"invalid request body"}"#));
            return;
        }
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
            let body = serde_json::to_string(&denial_from(reason)).unwrap();
            let _ = request.respond(json(200, &body));
        }

        PolicyDecision::Allow => {
            // Execute tool — no lock held during execution (may be slow for http_get).
            let cost = registry.declared_cost(&call.tool).unwrap_or(0);
            let result = registry.call(&call.tool, &call.args);

            match result {
                Err(ToolCallError::NotFound { tool_name }) => {
                    let body = format!(
                        r#"{{"error":"tool not found","tool_name":"{}"}}"#,
                        tool_name
                    );
                    let _ = request.respond(json(404, &body));
                }
                Err(ToolCallError::Execution { tool_name, source }) => {
                    let body = format!(
                        r#"{{"error":"tool execution failed","tool_name":"{}","message":"{}"}}"#,
                        tool_name, source
                    );
                    let _ = request.respond(json(500, &body));
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
                    let body = serde_json::to_string(&ToolCallResponse::Allowed {
                        result: output.content,
                    })
                    .unwrap();
                    let _ = request.respond(json(200, &body));
                }
            }
        }
    }
}

fn handle_rule_evaluate(mut request: Request, shared: &Arc<Mutex<BridgeState>>) {
    // Accept an optional context payload — defaults to current tracked state.
    let req: RuleEvalRequest = serde_json::from_reader(request.as_reader())
        .unwrap_or_default();

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
        PolicyDecision::Allow => r#"{"status":"allowed"}"#.to_string(),
        PolicyDecision::Deny { reason: StopReason::RuleDenied { rule_name } } => {
            format!(r#"{{"status":"denied","rule_name":"{}"}}"#, rule_name)
        }
        PolicyDecision::Deny { reason } => {
            format!(r#"{{"status":"denied","reason":"{}"}}"#, stop_reason_name(&reason))
        }
    };
    let _ = request.respond(json(200, &body));
}

fn handle_agent_enter(mut request: Request, shared: &Arc<Mutex<BridgeState>>) {
    let req: AgentEnterRequest = match serde_json::from_reader(request.as_reader()) {
        Ok(b) => b,
        Err(_) => {
            let _ = request.respond(json(400, r#"{"error":"invalid request body"}"#));
            return;
        }
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
        Ok(limits) => {
            let body = format!(
                r#"{{"status":"ok","limits":{{"steps":{},"cost":{},"timeout":{}}}}}"#,
                limits.max_steps, limits.max_cost_units, limits.timeout_ms
            );
            let _ = request.respond(json(200, &body));
        }
        Err(name) => {
            let body = format!(r#"{{"error":"named limits set '{}' not found"}}"#, name);
            let _ = request.respond(json(404, &body));
        }
    }
}

fn handle_agent_exit(request: Request, shared: &Arc<Mutex<BridgeState>>) {
    {
        let mut guard = shared.lock().unwrap();
        let prev = guard.limits_stack.pop().unwrap_or(guard.default_limits.clone());
        guard.current_limits = prev.clone();
        guard.limits_policy = LimitsPolicy::new(prev, guard.allowed_tools.clone());
    }
    let _ = request.respond(json(200, r#"{"status":"ok"}"#));
}

fn handle_step(request: Request, shared: &Arc<Mutex<BridgeState>>) {
    let body = {
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
                format!(
                    r#"{{"status":"stopped","reason":"{}","step":{}}}"#,
                    name, guard.step_count
                )
            }
            PolicyDecision::Allow => {
                format!(r#"{{"status":"running","step":{}}}"#, guard.step_count)
            }
        }
    };
    let _ = request.respond(json(200, &body));
}

// ── Request / response types ──────────────────────────────────────────────────

#[derive(serde::Deserialize)]
struct ToolCallRequest {
    tool: String,
    #[serde(default)]
    args: ToolArgs,
}

#[derive(serde::Deserialize, Default)]
struct RuleEvalRequest {
    #[serde(default)]
    step: Option<u32>,
    #[serde(default)]
    elapsed: Option<u64>,
    #[serde(default)]
    tool: Option<String>,
    #[serde(default)]
    tool_call_counts: HashMap<String, u32>,
    #[serde(default)]
    tool_call_history: Vec<String>,
    #[serde(default)]
    cost_spent: Option<u64>,
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

// ── HTTP helpers ──────────────────────────────────────────────────────────────

fn is_stopped(shared: &Arc<Mutex<BridgeState>>) -> bool {
    matches!(shared.lock().unwrap().execution, ExecutionState::Stopped { .. })
}

fn token_valid(request: &Request, shared: &Arc<Mutex<BridgeState>>) -> bool {
    let guard = shared.lock().unwrap();
    request
        .headers()
        .iter()
        .find(|h| h.field.as_str().as_str().eq_ignore_ascii_case("x-nanny-session-token"))
        .map(|h| h.value.as_str() == guard.session_token.as_str())
        .unwrap_or(false)
}

fn json(status: u16, body: &str) -> Response<std::io::Cursor<Vec<u8>>> {
    Response::from_data(body.as_bytes().to_vec())
        .with_status_code(StatusCode(status))
        .with_header(
            tiny_http::Header::from_bytes("Content-Type", "application/json").unwrap(),
        )
}

fn ndjson(body: &str) -> Response<std::io::Cursor<Vec<u8>>> {
    Response::from_data(body.as_bytes().to_vec())
        .with_status_code(StatusCode(200))
        .with_header(
            tiny_http::Header::from_bytes("Content-Type", "application/x-ndjson").unwrap(),
        )
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
        std::thread::sleep(std::time::Duration::from_millis(20));
        b
    }

    // ── HTTP helpers ──────────────────────────────────────────────────────────

    fn get(port: u16, token: &str, path: &str) -> (u16, String) {
        use std::io::{Read, Write};
        use std::net::TcpStream;
        let mut s = TcpStream::connect(("127.0.0.1", port)).unwrap();
        write!(s, "GET {path} HTTP/1.0\r\nHost: 127.0.0.1\r\nX-Nanny-Session-Token: {token}\r\n\r\n").unwrap();
        let mut raw = String::new();
        s.read_to_string(&mut raw).unwrap();
        parse_http(raw)
    }

    fn post(port: u16, token: &str, path: &str, body: &str) -> (u16, String) {
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
    fn bridge_binds_to_a_port() {
        assert!(started(1000).port > 0);
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
        let (s, body) = get(b.port, &b.session_token, "/health");
        assert_eq!(s, 200);
        assert_eq!(json_val(&body)["state"], "running");
    }

    #[test]
    fn wrong_token_returns_401() {
        let b = started(1000);
        assert_eq!(get(b.port, "wrong", "/health").0, 401);
    }

    #[test]
    fn missing_token_returns_401() {
        let b = started(1000);
        assert_eq!(get(b.port, "", "/health").0, 401);
    }

    #[test]
    fn stop_reflects_in_health_response() {
        let b = started(1000);
        b.stop("TimeoutExpired");
        let (_, body) = get(b.port, &b.session_token, "/health");
        let v = json_val(&body);
        assert_eq!(v["state"], "stopped");
        assert_eq!(v["reason"], "TimeoutExpired");
    }

    #[test]
    fn unknown_route_returns_404() {
        let b = started(1000);
        assert_eq!(get(b.port, &b.session_token, "/does-not-exist").0, 404);
    }

    // ── Day 2 tests ───────────────────────────────────────────────────────────

    #[test]
    fn tool_call_returns_allowed_and_result() {
        let b = started(1000);
        let (s, body) = post(b.port, &b.session_token, "/tool/call",
            r#"{"tool":"echo","args":{"message":"hello"}}"#);
        assert_eq!(s, 200);
        let v = json_val(&body);
        assert_eq!(v["status"], "allowed");
        assert_eq!(v["result"], "hello");
    }

    #[test]
    fn tool_call_charges_cost_and_tracks_counts() {
        let b = started(1000);
        let body = r#"{"tool":"echo","args":{}}"#;
        post(b.port, &b.session_token, "/tool/call", body);
        post(b.port, &b.session_token, "/tool/call", body);
        let state = b.shared.lock().unwrap();
        assert_eq!(state.cost_units_spent, 20);
        assert_eq!(state.tool_call_counts["echo"], 2);
        assert_eq!(state.tool_call_history, vec!["echo", "echo"]);
    }

    #[test]
    fn denied_tool_returns_denied_with_tool_name() {
        let b = started(1000);
        let (s, body) = post(b.port, &b.session_token, "/tool/call",
            r#"{"tool":"write_file","args":{}}"#);
        assert_eq!(s, 200);
        let v = json_val(&body);
        assert_eq!(v["status"], "denied");
        assert_eq!(v["reason"], "ToolDenied");
        assert_eq!(v["tool_name"], "write_file");
    }

    #[test]
    fn denied_tool_stops_execution() {
        let b = started(1000);
        post(b.port, &b.session_token, "/tool/call", r#"{"tool":"write_file","args":{}}"#);
        assert!(matches!(b.execution_state(), ExecutionState::Stopped { .. }));
    }

    #[test]
    fn budget_exhaustion_stops_execution_and_returns_denied() {
        let b = started(10); // max_cost=10, echo costs 10
        let body = r#"{"tool":"echo","args":{}}"#;
        let (_, r1) = post(b.port, &b.session_token, "/tool/call", body);
        assert_eq!(json_val(&r1)["status"], "allowed");
        let (_, r2) = post(b.port, &b.session_token, "/tool/call", body);
        let v = json_val(&r2);
        assert_eq!(v["status"], "denied");
        assert_eq!(v["reason"], "BudgetExhausted");
        assert!(matches!(b.execution_state(), ExecutionState::Stopped { .. }));
    }

    #[test]
    fn tool_call_on_stopped_execution_returns_410() {
        let b = started(1000);
        b.stop("ManualStop");
        assert_eq!(post(b.port, &b.session_token, "/tool/call", r#"{"tool":"echo","args":{}}"#).0, 410);
    }

    #[test]
    fn invalid_request_body_returns_400() {
        let b = started(1000);
        assert_eq!(post(b.port, &b.session_token, "/tool/call", "not json").0, 400);
    }

    #[test]
    fn max_calls_rule_stops_execution_on_excess() {
        let mut per_tool = HashMap::new();
        per_tool.insert("echo".to_string(), 1u32);
        let mut registry = ToolRegistry::new();
        registry.register(Box::new(EchoTool));
        let b = Bridge::start(BridgeComponents {
            registry,
            limits: Limits { max_steps: 100, max_cost_units: 1000, timeout_ms: 30_000 },
            named_limits: Default::default(),
            allowed_tools: vec!["echo".to_string()],
            per_tool_max_calls: per_tool,
        }).unwrap();
        std::thread::sleep(std::time::Duration::from_millis(20));
        let body = r#"{"tool":"echo","args":{}}"#;
        let (_, r1) = post(b.port, &b.session_token, "/tool/call", body);
        assert_eq!(json_val(&r1)["status"], "allowed");
        let (_, r2) = post(b.port, &b.session_token, "/tool/call", body);
        let v = json_val(&r2);
        assert_eq!(v["status"], "denied");
        assert_eq!(v["reason"], "RuleDenied");
        assert_eq!(v["rule_name"], "echo.max_calls");
    }

    // ── Day 3 tests ───────────────────────────────────────────────────────────

    #[test]
    fn rule_evaluate_allows_when_no_rules_configured() {
        let b = started(1000);
        let (s, body) = post(b.port, &b.session_token, "/rule/evaluate",
            r#"{"tool":"echo","tool_call_counts":{"echo":99}}"#);
        assert_eq!(s, 200);
        assert_eq!(json_val(&body)["status"], "allowed"); // no max_calls configured
    }

    #[test]
    fn rule_evaluate_denies_at_max_calls_with_provided_context() {
        let mut per_tool = HashMap::new();
        per_tool.insert("echo".to_string(), 2u32);
        let mut registry = ToolRegistry::new();
        registry.register(Box::new(EchoTool));
        let b = Bridge::start(BridgeComponents {
            registry,
            limits: Limits { max_steps: 100, max_cost_units: 1000, timeout_ms: 30_000 },
            named_limits: Default::default(),
            allowed_tools: vec!["echo".to_string()],
            per_tool_max_calls: per_tool,
        }).unwrap();
        std::thread::sleep(std::time::Duration::from_millis(20));
        // Provide context showing echo has been called 2 times (= max)
        let (s, body) = post(b.port, &b.session_token, "/rule/evaluate",
            r#"{"tool":"echo","tool_call_counts":{"echo":2}}"#);
        assert_eq!(s, 200);
        let v = json_val(&body);
        assert_eq!(v["status"], "denied");
        assert_eq!(v["rule_name"], "echo.max_calls");
    }

    #[test]
    fn rule_evaluate_uses_tracked_state_when_no_context_provided() {
        let mut per_tool = HashMap::new();
        per_tool.insert("echo".to_string(), 1u32);
        let mut registry = ToolRegistry::new();
        registry.register(Box::new(EchoTool));
        let b = Bridge::start(BridgeComponents {
            registry,
            limits: Limits { max_steps: 100, max_cost_units: 1000, timeout_ms: 30_000 },
            named_limits: Default::default(),
            allowed_tools: vec!["echo".to_string()],
            per_tool_max_calls: per_tool,
        }).unwrap();
        std::thread::sleep(std::time::Duration::from_millis(20));
        // Make one call to build tracked state
        post(b.port, &b.session_token, "/tool/call", r#"{"tool":"echo","args":{}}"#);
        // Now rule/evaluate with just the tool name — bridge uses tracked counts
        let (_, body) = post(b.port, &b.session_token, "/rule/evaluate", r#"{"tool":"echo"}"#);
        let v = json_val(&body);
        assert_eq!(v["status"], "denied");
        assert_eq!(v["rule_name"], "echo.max_calls");
    }

    #[test]
    fn rule_evaluate_on_stopped_execution_returns_410() {
        let b = started(1000);
        b.stop("ManualStop");
        assert_eq!(post(b.port, &b.session_token, "/rule/evaluate", "{}").0, 410);
    }

    // ── Day 4 tests ───────────────────────────────────────────────────────────

    fn bridge_with_named_limits() -> Bridge {
        let mut named = HashMap::new();
        named.insert("researcher".to_string(), Limits {
            max_steps: 500, max_cost_units: 5000, timeout_ms: 600_000,
        });
        named.insert("writer".to_string(), Limits {
            max_steps: 50, max_cost_units: 200, timeout_ms: 60_000,
        });
        let b = Bridge::start(BridgeComponents {
            registry: ToolRegistry::new(),
            limits: Limits { max_steps: 100, max_cost_units: 1000, timeout_ms: 30_000 },
            named_limits: named,
            allowed_tools: vec![],
            per_tool_max_calls: Default::default(),
        }).unwrap();
        std::thread::sleep(std::time::Duration::from_millis(20));
        b
    }

    #[test]
    fn agent_enter_switches_limits() {
        let b = bridge_with_named_limits();
        let (s, body) = post(b.port, &b.session_token, "/agent/enter", r#"{"name":"researcher"}"#);
        assert_eq!(s, 200);
        let v = json_val(&body);
        assert_eq!(v["status"], "ok");
        assert_eq!(v["limits"]["steps"], 500);
        assert_eq!(v["limits"]["cost"], 5000);
        // Verify the internal policy was updated
        let guard = b.shared.lock().unwrap();
        assert_eq!(guard.current_limits.max_steps, 500);
    }

    #[test]
    fn agent_enter_missing_set_returns_404() {
        let b = bridge_with_named_limits();
        let (s, body) = post(b.port, &b.session_token, "/agent/enter", r#"{"name":"ghost"}"#);
        assert_eq!(s, 404);
        assert!(json_val(&body)["error"].as_str().unwrap().contains("ghost"));
    }

    #[test]
    fn agent_exit_reverts_to_previous_limits() {
        let b = bridge_with_named_limits();
        post(b.port, &b.session_token, "/agent/enter", r#"{"name":"researcher"}"#);
        let (s, _) = post(b.port, &b.session_token, "/agent/exit", "{}");
        assert_eq!(s, 200);
        let guard = b.shared.lock().unwrap();
        assert_eq!(guard.current_limits.max_steps, 100); // reverted to global default
    }

    #[test]
    fn nested_agent_enter_exit_round_trip() {
        let b = bridge_with_named_limits();
        // Enter researcher
        post(b.port, &b.session_token, "/agent/enter", r#"{"name":"researcher"}"#);
        // Enter writer (nested)
        post(b.port, &b.session_token, "/agent/enter", r#"{"name":"writer"}"#);
        {
            let guard = b.shared.lock().unwrap();
            assert_eq!(guard.current_limits.max_steps, 50); // writer
            assert_eq!(guard.limits_stack.len(), 2); // global + researcher on stack
        }
        // Exit writer → back to researcher
        post(b.port, &b.session_token, "/agent/exit", "{}");
        {
            let guard = b.shared.lock().unwrap();
            assert_eq!(guard.current_limits.max_steps, 500); // researcher
        }
        // Exit researcher → back to global
        post(b.port, &b.session_token, "/agent/exit", "{}");
        {
            let guard = b.shared.lock().unwrap();
            assert_eq!(guard.current_limits.max_steps, 100); // global
            assert!(guard.limits_stack.is_empty());
        }
    }

    // ── Day 5 tests ───────────────────────────────────────────────────────────

    #[test]
    fn step_increments_count_and_returns_running() {
        let b = started(1000);
        let (s, body) = post(b.port, &b.session_token, "/step", "{}");
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
        let (_, body) = post(b.port, &b.session_token, "/step", "{}");
        let v = json_val(&body);
        assert_eq!(v["status"], "stopped");
        assert_eq!(v["reason"], "MaxStepsReached");
        assert!(matches!(b.execution_state(), ExecutionState::Stopped { .. }));
    }

    #[test]
    fn status_returns_running_with_counters() {
        let b = started(1000);
        post(b.port, &b.session_token, "/step", "{}");
        let (s, body) = get(b.port, &b.session_token, "/status");
        assert_eq!(s, 200);
        let v = json_val(&body);
        assert_eq!(v["state"], "running");
        assert_eq!(v["step"], 1);
    }

    #[test]
    fn status_available_after_stop() {
        let b = started(1000);
        b.stop("ManualStop");
        let (s, body) = get(b.port, &b.session_token, "/status");
        assert_eq!(s, 200);
        assert_eq!(json_val(&body)["state"], "stopped");
    }

    #[test]
    fn events_contains_tool_allowed_after_call() {
        let b = started(1000);
        post(b.port, &b.session_token, "/tool/call", r#"{"tool":"echo","args":{}}"#);
        let (s, body) = get(b.port, &b.session_token, "/events");
        assert_eq!(s, 200);
        let events: Vec<serde_json::Value> = body.lines()
            .filter_map(|l| serde_json::from_str(l).ok())
            .collect();
        assert!(events.iter().any(|v| v["event"] == "ToolAllowed"));
    }

    #[test]
    fn events_contains_execution_stopped_after_stop() {
        let b = started(1000);
        b.stop("ManualStop");
        let (_, body) = get(b.port, &b.session_token, "/events");
        let events: Vec<serde_json::Value> = body.lines()
            .filter_map(|l| serde_json::from_str(l).ok())
            .collect();
        assert!(events.iter().any(|v| v["event"] == "ExecutionStopped"));
    }

    #[test]
    fn events_contains_step_completed_after_step() {
        let b = started(1000);
        post(b.port, &b.session_token, "/step", "{}");
        let (_, body) = get(b.port, &b.session_token, "/events");
        let events: Vec<serde_json::Value> = body.lines()
            .filter_map(|l| serde_json::from_str(l).ok())
            .collect();
        assert!(events.iter().any(|v| v["event"] == "StepCompleted"));
    }

    #[test]
    fn events_available_after_stop() {
        let b = started(1000);
        b.stop("TimeoutExpired");
        // /events must work even after execution stops
        assert_eq!(get(b.port, &b.session_token, "/events").0, 200);
    }

    #[test]
    fn stop_is_idempotent() {
        let b = started(1000);
        b.stop("TimeoutExpired");
        b.stop("ManualStop"); // second call is ignored
        // Events should only have one ExecutionStopped
        let (_, body) = get(b.port, &b.session_token, "/events");
        let count = body.lines()
            .filter_map(|l| serde_json::from_str::<serde_json::Value>(l).ok())
            .filter(|v| v["event"] == "ExecutionStopped")
            .count();
        assert_eq!(count, 1);
        // Reason is the first stop, not the second
        assert_eq!(
            json_val(&get(b.port, &b.session_token, "/health").1)["reason"],
            "TimeoutExpired"
        );
    }
}
