// The policy contract.
//
// This module defines the shapes the policy engine works with.
// Concrete implementations live in nanny-policy.
//
// The executor depends on this module — not on nanny-policy directly.
// That separation prevents a circular dependency:
//   nanny-core defines the contract
//   nanny-policy implements it
//   nanny-core's executor uses the contract

use crate::agent::state::StopReason;
use std::collections::HashMap;

// ── PolicyContext ─────────────────────────────────────────────────────────────

/// Everything the policy engine knows about the current moment in execution.
///
/// The executor builds this before every step and hands it to the policy.
/// The policy reads it and makes a decision. That is the entire interface.
#[derive(Default)]
pub struct PolicyContext {
    /// How many steps have completed so far.
    pub step_count: u32,

    /// How many milliseconds have elapsed since execution started.
    pub elapsed_ms: u64,

    /// The name of the tool being requested, if any.
    /// `None` means no tool call is being made this step.
    pub requested_tool: Option<String>,

    /// Total cost units spent across all steps so far.
    pub cost_units_spent: u64,

    /// How many times each tool has been called in this execution.
    /// Key: tool name. Value: call count. Updated by the executor after each tool call.
    /// Custom rules use this to detect spirals (e.g., same tool called 8 times in a row).
    pub tool_call_counts: HashMap<String, u32>,

    /// Ordered history of tool calls in this execution.
    /// Each entry is a tool name. Appended by the executor after each tool call.
    /// Custom rules use this to detect sequences and patterns.
    pub tool_call_history: Vec<String>,

    /// The arguments of the tool call currently being evaluated.
    /// Key: parameter name. Value: string representation of the argument.
    /// Empty when no tool call is in flight (e.g. during step evaluation).
    ///
    /// Rules use this to inspect what the agent is about to do:
    /// ```ignore
    /// #[nanny::rule("no_sensitive_files")]
    /// fn block_sensitive(ctx: &PolicyContext) -> bool {
    ///     ctx.last_tool_args.get("path")
    ///         .map(|p| !p.contains(".env") && !p.contains("secret"))
    ///         .unwrap_or(true)
    /// }
    /// ```
    pub last_tool_args: HashMap<String, String>,
}

// ── PolicyDecision ────────────────────────────────────────────────────────────

/// What the policy engine decides.
///
/// Two outcomes only. No "maybe". No "retry". No "warn".
/// Either execution is allowed to continue, or it is stopped with a reason.
pub enum PolicyDecision {
    /// The step may proceed.
    Allow,

    /// The step must not proceed. Execution stops immediately.
    Deny { reason: StopReason },
}

// ── Policy trait ──────────────────────────────────────────────────────────────

/// The policy contract.
///
/// Any type that implements this trait can make execution decisions.
/// Implementations must be pure — same context always produces same decision.
/// No side effects. No network calls. No randomness.
pub trait Policy {
    fn evaluate(&self, context: &PolicyContext) -> PolicyDecision;
}
