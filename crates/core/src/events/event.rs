use serde::{Deserialize, Serialize};

// ── LimitsSnapshot ────────────────────────────────────────────────────────────

/// Short-name snapshot of the active limits, matching nanny.toml field names.
///
/// Distinct from `Limits` (which uses descriptive Rust names).
/// Written into `ExecutionStarted` so any reader can reconstruct enforcement context.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LimitsSnapshot {
    pub steps: u32,
    pub tokens: u64,
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

    /// Emitted when LLM token usage is reported to the bridge — via
    /// `nanny::report_usage` (Rust) or `nanny.instrument()` (Python).
    ///
    /// `input`/`output` are the measured token counts debited from the active
    /// budget — enforcement always sums these two, regardless of what
    /// `cache_read`/`cache_write` say, so a provider that doesn't report
    /// cache usage behaves exactly as before. `model`/`provider` are optional
    /// attribution labels: identifiers only, never prompt or response
    /// content, and never pricing — cost is a hosted-layer concern, never the
    /// engine's.
    ///
    /// `cache_read`/`cache_write` are an optional finer split of `input`
    /// (never additional tokens beyond it — `input` is always the true
    /// total, cache_read/cache_write always a genuine subset of it), present
    /// only for providers that report prompt-caching usage at all (OpenAI,
    /// Anthropic, DeepSeek, Gemini — see `nanny_sdk.instrument`'s
    /// per-provider extraction), absent (not zero) for providers that don't.
    /// This is a promise about Nanny's own wire format, not a fact about
    /// every provider's raw API: some providers (DeepSeek, OpenAI) report a
    /// base count that's already inclusive of cache hits, matching this
    /// directly; others (Anthropic) report a base count that's exclusive of
    /// cache usage, so the SDK adds cache_read/cache_write into `input`
    /// before reporting it, specifically so this invariant holds
    /// universally and no downstream consumer needs per-provider knowledge
    /// to interpret it correctly. Reporting only, same as `model`/`provider`:
    /// no pricing logic reads these in the engine, they exist so a
    /// downstream cost calculator (Nanny Cloud) can price cache-hit tokens
    /// at their real, much cheaper rate instead of treating all input as one
    /// undifferentiated price.
    LlmUsageRecorded {
        ts: u64,
        input: u64,
        output: u64,
        #[serde(skip_serializing_if = "Option::is_none")]
        model: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        provider: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        cache_read: Option<u64>,
        #[serde(skip_serializing_if = "Option::is_none")]
        cache_write: Option<u64>,
    },

    /// Emitted once when the agent declares the agentic harness that ran it —
    /// via `nanny::set_harness` (Rust) or the SDK.
    ///
    /// `name` is the harness identifier (e.g. `"opencode"`, `"langgraph"`);
    /// `version` is optional. This is our equivalent of OpenRouter's "app"
    /// column — an attribution label only, never content and never pricing.
    /// Distinct from `AgentScopeEntered`, which names a `@nanny::agent` limits
    /// scope, not the harness.
    HarnessIdentified {
        ts: u64,
        name: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        version: Option<String>,
    },

    /// Emitted once when a process declares which app it is, from the
    /// committed `.nanny/app.json`.
    ///
    /// `app_id` is the permanent, generated identity written by `nanny init`
    /// and never regenerated; `name` is the human-chosen display label and may
    /// change without the app becoming a different app.
    ///
    /// This rides in the payload rather than being derived from the API key,
    /// which is what lets one governor holding one credential serve many apps
    /// and still have each attributed separately, the same reason OpenTelemetry
    /// makes `service.name` a resource attribute instead of a transport concern.
    /// A process joining a governor declares its own identity here; one that has
    /// none inherits the governor's.
    ///
    /// Attribution label only, exactly like `HarnessIdentified`: never content,
    /// never pricing, never affects a stop.
    AppIdentified {
        ts: u64,
        app_id: String,
        name: String,
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
        tokens_spent: u64,
        elapsed_ms: u64,
    },
}
