use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

// ── now_ms ────────────────────────────────────────────────────────────────────

/// Current time as milliseconds since the Unix epoch.
pub fn now_ms() -> u64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

// ── RuleDecl ──────────────────────────────────────────────────────────────────

/// One registered rule, as declared by the process that holds it.
///
/// A name alone cannot answer "which control was this". Two runs six months
/// apart can both declare `no_send_after_read` while running different code, so
/// evidence produced under one is not comparable with evidence produced under
/// the other. `version` and `pack` make the declaration specific.
///
/// Both are `None` for a rule the developer wrote themselves, which is the
/// honest answer: a hand-written rule has no version and inventing one would
/// imply a provenance it does not have. They are populated by the pack loader
/// from the pack's manifest, never typed by the person writing the rule.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct RuleDecl {
    pub name: String,
    /// The pack version this rule came from. `None` for hand-written rules.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
    /// The pack this rule came from. `None` for hand-written rules.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pack: Option<String>,
}

impl RuleDecl {
    /// A rule the developer wrote, carrying no pack provenance.
    pub fn local(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            version: None,
            pack: None,
        }
    }
}

// ── ExecutionEvent ────────────────────────────────────────────────────────────

/// The canonical event type for every event the nanny ecosystem emits.
///
/// Used by both the bridge (per-tool and per-step events) and the CLI
/// (bookend events). Every event carries a `ts` (ms since epoch) for
/// ordering and correlation.
///
/// The log is append-only. Events are never modified or deleted.
/// If `ExecutionStopped` is missing from a log, the process crashed,
/// that absence is itself an auditable fact.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "event")]
pub enum ExecutionEvent {
    /// Emitted exactly once when execution begins.
    ///
    /// Records the command and the **declared authority** of this run: exactly
    /// what the agent was permitted to do before it did anything. Refusals are
    /// recorded as they happen, but a log of refusals alone cannot answer "what
    /// was this agent allowed to do", which is the question an auditor actually
    /// asks. Writing the grant at the start makes the answer a fact in the log
    /// rather than an inference from absence.
    ///
    /// This carries the half of the grant the governor knows: the config. The
    /// rules half lives in the agent's own process and arrives separately, as
    /// [`ExecutionEvent::RulesDeclared`].
    ExecutionStarted {
        ts: u64,
        command: String,
        /// The tool allowlist, as declared in `[tools] allowed`.
        allowed_tools: Vec<String>,
        /// Operator-declared labels per tool, for every allowlisted tool.
        tool_labels: BTreeMap<String, Vec<String>>,
        /// Fingerprint of the parsed config that produced this grant.
        ///
        /// The join key between a run and the policy that governed it. The
        /// allowlist and labels above say what was permitted; this says *which
        /// revision* of the operator's intent that was, so two runs can be
        /// compared without reproducing their configs. Hashed over the parsed
        /// config, so reformatting does not mint a new policy.
        config_hash: String,
        /// This runtime's own version (`CARGO_PKG_VERSION`), e.g. `"0.6.0"`.
        ///
        /// Before this, nothing in the event log carried the runtime's
        /// own version: only the rule pack and the harness had one. Without
        /// it, "which of my machines are still on an old runtime" is
        /// unanswerable the day after publishing a fix, since every run looks
        /// the same regardless of which binary produced it.
        runtime_version: String,
    },

    /// Emitted when a tool call is evaluated and allowed by policy.
    ///
    /// `cleared_by` names the rules that evaluated this call and allowed it, in
    /// evaluation order. Without it a rule that ran clean nine thousand times
    /// and a rule that was never reached produce identical logs, so the healthy
    /// state, which is the normal state for a good control, is unprovable. The
    /// engine is required to log every verdict, allow and refuse alike; a rule
    /// returning allow is a verdict.
    ///
    /// Empty when no rule governed the call.
    ToolAllowed {
        ts: u64,
        tool: String,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        cleared_by: Vec<String>,
    },

    /// Emitted when a tool call is blocked by the allowlist ([tools] allowed).
    ///
    /// The tool was not in the permitted set: execution stops immediately.
    /// Distinct from `RuleDenied`: this fires from `ToolPermissionPolicy`,
    /// before any rule evaluation.
    ToolDenied { ts: u64, tool: String },

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
        /// Rules that evaluated and allowed this call *before* the one that
        /// fired, in evaluation order.
        ///
        /// Evaluation short-circuits on the first denial, so rules after
        /// `rule_name` never produced a verdict and listing them would be a
        /// fabrication. This is the boundary between what was checked and what
        /// merely existed.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        cleared_by: Vec<String>,
    },

    /// Emitted when a permitted tool fails during execution.
    ///
    /// Distinct from a policy denial: the tool was allowed but encountered
    /// an error (network failure, bad args, timeout).
    /// No cost is charged on tool failure.
    ToolFailed {
        ts: u64,
        tool: String,
        error: String,
    },

    /// Emitted when LLM token usage is reported to the bridge: via
    /// `nanny::report_usage` (Rust) or `nanny.instrument()` (Python).
    ///
    /// `input`/`output` are the measured token counts debited from the active
    /// budget: enforcement always sums these two, regardless of what
    /// `cache_read`/`cache_write` say, so a provider that doesn't report
    /// cache usage behaves exactly as before. `model`/`provider` are optional
    /// attribution labels: identifiers only, never prompt or response
    /// content, and never pricing: cost is a hosted-layer concern, never the
    /// engine's.
    ///
    /// `cache_read`/`cache_write` are an optional finer split of `input`
    /// (never additional tokens beyond it: `input` is always the true
    /// total, cache_read/cache_write always a genuine subset of it), present
    /// only for providers that report prompt-caching usage at all (OpenAI,
    /// Anthropic, DeepSeek, Gemini: see `nanny_sdk.instrument`'s
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

    /// Emitted once when the agent declares the agentic harness that ran it,
    /// via `nanny::set_harness` (Rust) or the SDK.
    ///
    /// `name` is the harness identifier (e.g. `"opencode"`, `"langgraph"`);
    /// `version` is optional. This is our equivalent of OpenRouter's "app"
    /// column: an attribution label only, never content and never pricing.
    /// Distinct from `AgentScopeEntered`, which names a `@nanny::agent`
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

    /// Emitted once when a `--serve` governance server starts.
    ///
    /// the cloud currently derives a governor handle by HMAC of the
    /// server secret. That groups a governor's runs correctly, but gives no
    /// name, address or version, and rotates whenever the secret does: every
    /// process restart, since the secret is a fresh value each time. This is
    /// the stable identity to show instead, parallel to `AppIdentified`:
    /// attribution only, never affects enforcement or a stop.
    ///
    /// Not emitted by a plain (non-`--serve`) `nanny run`: there is no
    /// governor to identify; that run's `governorId` stays absent, as today.
    GovernorIdentified {
        ts: u64,
        /// The host it's running on. Best-effort; falls back to `"unknown"`
        /// rather than failing the run over a label.
        name: String,
        /// Where other processes and machines reach it (`--addr`).
        address: String,
        /// This runtime's own version (`CARGO_PKG_VERSION`).
        version: String,
    },

    /// Emitted once when a process declares the rules it has registered.
    ///
    /// The second half of declared authority. `ExecutionStarted` records the
    /// config-side grant (allowlist and labels), which the governor reads from
    /// nanny.toml; rules are compiled into the agent's own process and the
    /// governor cannot see them, so the agent declares them here.
    ///
    /// Split across two events on purpose rather than guessed at in one: the
    /// governor emitting `rules: []` because it cannot see them would read as
    /// "no rules registered", which is worse than saying nothing.
    ///
    /// Deduped bridge-side, so a caller may safely redeclare.
    RulesDeclared { ts: u64, rules: Vec<RuleDecl> },

    /// Emitted when the agent enters a named scope via `agent_enter`.
    ///
    /// Records which phase of the run the following events belong to, so the
    /// audit log can attribute every verdict to the scope that produced it.
    AgentScopeEntered { ts: u64, name: String },

    /// Emitted when the agent exits a named scope via `agent_exit`.
    ///
    /// Paired with `AgentScopeEntered`: together they bracket the governed scope.
    AgentScopeExited { ts: u64, name: String },

    /// Emitted as the final event when execution stops for any reason.
    ///
    /// This event is always the last one in any complete execution log.
    /// The CLI is the sole owner of this event.
    ExecutionStopped {
        ts: u64,
        reason: String,
        tokens_spent: u64,
        elapsed_ms: u64,
    },
}

// ── LoggedEvent ───────────────────────────────────────────────────────────────

/// An [`ExecutionEvent`] stamped with the run it belongs to and its position in
/// that run's stream.
///
/// Every event written to the log or forwarded to a sink goes through this.
/// Without it the log is not self-describing, which is a correctness problem
/// rather than a convenience one: under `nanny run --serve` a single governor
/// drains many concurrent runs into one shared file, so lines from different
/// runs interleave with nothing to tell them apart. The drain loop holds the
/// run id at the moment it writes and used to discard it.
///
/// Attribution cannot be recovered afterwards either. Draining is per-run and
/// batched, so a run drained later can append older timestamps after a run
/// drained earlier appended newer ones, which means sorting by `ts` does not
/// reconstruct the interleaving. Pairing `ExecutionStarted` with
/// `ExecutionStopped` does not work either, because a missing stop is a real,
/// documented outcome (the process crashed) rather than a parse error.
///
/// `seq` is monotonic **per run**, assigned where the event is appended, never
/// where it is drained. Assigned at drain time it would number across
/// interleaved runs and manufacture gaps in the one field whose purpose is
/// making genuine gaps detectable.
///
/// Flattened on purpose: the JSON keeps the shape every existing consumer
/// already parses and gains exactly two keys.
///
/// ```text
/// {"run_id":"a1b2","seq":3,"event":"ToolAllowed","ts":1756100000000,"tool":"web_search"}
///  └────────── envelope ─────────┘└──────────── ExecutionEvent ────────────────────────┘
/// ```
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LoggedEvent {
    /// Which run this event belongs to. Under `--serve` one governor serves
    /// many; under a local run there is exactly one.
    pub run_id: String,
    /// Position in this run's stream, from 0. A gap means an event is missing.
    pub seq: u64,
    #[serde(flatten)]
    pub event: ExecutionEvent,
}

impl LoggedEvent {
    pub fn new(run_id: impl Into<String>, seq: u64, event: ExecutionEvent) -> Self {
        Self {
            run_id: run_id.into(),
            seq,
            event,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tool_allowed() -> ExecutionEvent {
        ExecutionEvent::ToolAllowed {
            ts: 1_756_100_000_000,
            tool: "web_search".into(),
            cleared_by: Vec::new(),
        }
    }

    #[test]
    fn envelope_flattens_into_the_event() {
        let line = serde_json::to_string(&LoggedEvent::new("a1b2", 3, tool_allowed())).unwrap();
        let v: serde_json::Value = serde_json::from_str(&line).unwrap();

        // The envelope adds two keys and moves nothing.
        assert_eq!(v["run_id"], "a1b2");
        assert_eq!(v["seq"], 3);
        assert_eq!(v["event"], "ToolAllowed");
        assert_eq!(v["tool"], "web_search");
        assert_eq!(v["ts"], 1_756_100_000_000u64);
    }

    #[test]
    fn envelope_round_trips() {
        let line = serde_json::to_string(&LoggedEvent::new("r", 0, tool_allowed())).unwrap();
        let back: LoggedEvent = serde_json::from_str(&line).unwrap();
        assert_eq!(back.run_id, "r");
        assert_eq!(back.seq, 0);
        assert!(matches!(back.event, ExecutionEvent::ToolAllowed { .. }));
    }

    #[test]
    fn every_variant_survives_the_envelope() {
        // A thirteenth variant added later must not silently lose the envelope.
        // Flattening is what buys that: the fields live on the wrapper, so a new
        // variant inherits them without being edited.
        let events = vec![
            ExecutionEvent::ExecutionStarted {
                ts: 1,
                command: "run".into(),
                allowed_tools: vec!["a".into()],
                tool_labels: BTreeMap::new(),
                config_hash: "deadbeef".into(),
                runtime_version: "0.6.0".into(),
            },
            ExecutionEvent::RulesDeclared {
                ts: 2,
                rules: vec![RuleDecl::local("r")],
            },
            ExecutionEvent::ExecutionStopped {
                ts: 3,
                reason: "AgentCompleted".into(),
                tokens_spent: 0,
                elapsed_ms: 1,
            },
        ];
        for (i, e) in events.into_iter().enumerate() {
            let v: serde_json::Value = serde_json::from_str(
                &serde_json::to_string(&LoggedEvent::new("r", i as u64, e)).unwrap(),
            )
            .unwrap();
            assert_eq!(v["run_id"], "r");
            assert_eq!(v["seq"], i as u64);
            assert!(v.get("event").is_some());
        }
    }
}
