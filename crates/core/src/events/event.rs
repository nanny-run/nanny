use serde::{Deserialize, Serialize};

// ── LimitsSnapshot ────────────────────────────────────────────────────────────

/// Short-name snapshot of the active limits, matching nanny.toml field names.
///
/// Distinct from `Limits` (which uses descriptive Rust names).
/// Written into `ExecutionStarted` so any reader can reconstruct enforcement context.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LimitsSnapshot {
    pub steps: u32,
    pub cost: u64,
    pub timeout: u64,
}

// ── now_ms ────────────────────────────────────────────────────────────────────

/// Current time as milliseconds since the Unix epoch.
pub fn now_ms() -> u64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

// ── ExecutionEvent ────────────────────────────────────────────────────────────

/// The canonical event type for every event the nanny ecosystem emits.
///
/// Used by both the bridge (per-tool and per-step events) and the CLI
/// (bookend events). Every event carries a `ts` (ms since epoch) for
/// ordering and correlation.
///
/// The log is append-only. Events are never modified or deleted.
/// If `ExecutionStopped` is missing from a log, the process crashed —
/// that absence is itself an auditable fact.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "event")]
pub enum ExecutionEvent {
    /// Emitted exactly once when execution begins.
    /// Records the limits in effect and the command being run.
    ExecutionStarted {
        ts: u64,
        limits: LimitsSnapshot,
        limits_set: String,
        command: String,
    },

    /// Emitted when a tool call is evaluated and allowed by policy.
    ToolAllowed {
        ts: u64,
        tool: String,
    },

    /// Emitted when a tool call is blocked by the allowlist ([tools] allowed).
    ///
    /// The tool was not in the permitted set — execution stops immediately.
    /// Distinct from `RuleDenied`: this fires from `LimitsPolicy`, before any
    /// rule evaluation.
    ToolDenied {
        ts: u64,
        tool: String,
    },

    /// Emitted when a tool call is blocked by a rule or per-tool call limit.
    ///
    /// `rule_name` identifies the rule that fired (e.g. `"no_spiral"`) or the
    /// auto-generated name for a `max_calls` limit (e.g. `"http_get.max_calls"`).
    /// Distinct from `ToolDenied`: this fires from `RuleEvaluator`, after the
    /// allowlist check passes.
    RuleDenied {
        ts: u64,
        tool: String,
        rule_name: String,
    },

    /// Emitted when a permitted tool fails during execution.
    ///
    /// Distinct from a policy denial — the tool was allowed but encountered
    /// an error (network failure, bad args, timeout).
    /// No cost is charged on tool failure.
    ToolFailed {
        ts: u64,
        tool: String,
        error: String,
    },

    /// Emitted when a step completes.
    StepCompleted {
        ts: u64,
        step: u32,
    },

    /// Emitted when the agent activates a named limits set via `agent_enter`.
    ///
    /// Records the name of the limits set and the limits now in effect,
    /// so the audit log captures exactly which budget governed each scope.
    AgentScopeEntered {
        ts: u64,
        name: String,
        limits: LimitsSnapshot,
    },

    /// Emitted when the agent exits a named limits scope via `agent_exit`.
    ///
    /// Paired with `AgentScopeEntered` — together they bracket the governed scope.
    AgentScopeExited {
        ts: u64,
        name: String,
    },

    /// Emitted as the final event when execution stops for any reason.
    ///
    /// This event is always the last one in any complete execution log.
    /// The CLI is the sole owner of this event.
    ExecutionStopped {
        ts: u64,
        reason: String,
        steps: u32,
        cost_spent: u64,
        elapsed_ms: u64,
    },
}
