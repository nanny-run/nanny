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
    /// How many milliseconds have elapsed since execution started.
    pub elapsed_ms: u64,

    /// Wall-clock time at evaluation, as milliseconds since the Unix epoch.
    ///
    /// Present so that a rule about *when* an action is permitted, "no
    /// external effects outside declared operating hours", stays a pure
    /// function of its context. A rule that read the clock itself would reach
    /// outside the context to decide, which is untestable and breaks the
    /// guarantee that identical inputs produce identical behaviour.
    ///
    /// Supplied by the caller, never sampled inside a rule.
    pub now_ms: u64,

    /// The name of the tool being requested, if any.
    /// `None` means no tool call is being made this step.
    pub requested_tool: Option<String>,

    /// Total tokens measured in this execution so far.
    pub tokens_spent: u64,

    /// How many times each tool has been called in this execution.
    /// Key: tool name. Value: call count. Updated by the executor after each tool call.
    /// Custom rules use this to detect spirals (e.g., same tool called 8 times in a row).
    pub tool_call_counts: HashMap<String, u32>,

    /// Ordered history of tool calls in this execution.
    /// Each entry is a tool name. Appended by the executor after each tool call.
    /// Custom rules use this to detect sequences and patterns.
    pub tool_call_history: Vec<String>,

    /// Operator-declared labels for **every** tool in the allowlist, not only
    /// the one being requested.
    ///
    /// Key: tool name. Value: that tool's labels, in a fixed order.
    ///
    /// Every tool, because taint rules read `tool_call_history` and need to
    /// ask what an *already-called* tool was: "did anything that reads
    /// untrusted content run before this?" cannot be answered from the
    /// pending call alone.
    ///
    /// Prefer [`PolicyContext::tool_has`] over indexing this directly.
    pub tool_labels: HashMap<String, Vec<String>>,

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

impl PolicyContext {
    /// Does `tool` carry `label`?
    ///
    /// The way rules are meant to read labels. Returns false for an unknown
    /// tool and for an unknown label, which is the correct default in both
    /// directions: a rule asking about a tool the operator never declared
    /// should not fire, and a rule asking about a label that does not exist
    /// should not silently match everything.
    ///
    /// ```ignore
    /// #[nanny::rule("no_external_effect_after_untrusted_read")]
    /// fn taint(ctx: &PolicyContext) -> bool {
    ///     let Some(pending) = ctx.requested_tool.as_deref() else { return true };
    ///     if !ctx.tool_has(pending, "external_effect") { return true; }
    ///     !ctx.tool_call_history.iter().any(|t| ctx.tool_has(t, "reads_untrusted"))
    /// }
    /// ```
    pub fn tool_has(&self, tool: &str, label: &str) -> bool {
        self.tool_labels
            .get(tool)
            .is_some_and(|labels| labels.iter().any(|l| l == label))
    }

    /// Every tool in the allowlist carrying `label`.
    ///
    /// For rules that need the set rather than a yes/no on one tool, e.g.
    /// "cap the total calls across all money-moving tools". Sorted, so a rule
    /// built on it behaves identically run to run.
    pub fn tools_with(&self, label: &str) -> Vec<&str> {
        let mut out: Vec<&str> = self
            .tool_labels
            .iter()
            .filter(|(_, labels)| labels.iter().any(|l| l == label))
            .map(|(name, _)| name.as_str())
            .collect();
        out.sort_unstable();
        out
    }
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

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn ctx_with_labels() -> PolicyContext {
        let mut tool_labels = HashMap::new();
        tool_labels.insert(
            "web_search".to_string(),
            vec!["reads_untrusted".to_string()],
        );
        tool_labels.insert(
            "send_outreach".to_string(),
            vec!["external_effect".to_string(), "moves_money".to_string()],
        );
        tool_labels.insert("save_findings".to_string(), Vec::new());
        PolicyContext { tool_labels, ..Default::default() }
    }

    #[test]
    fn tool_has_finds_a_declared_label() {
        let ctx = ctx_with_labels();
        assert!(ctx.tool_has("web_search", "reads_untrusted"));
        assert!(ctx.tool_has("send_outreach", "moves_money"));
    }

    #[test]
    fn tool_has_is_false_for_a_label_the_tool_lacks() {
        assert!(!ctx_with_labels().tool_has("web_search", "moves_money"));
    }

    /// An unlabelled tool is a real answer, not a missing one.
    #[test]
    fn tool_has_is_false_for_an_unlabelled_tool() {
        assert!(!ctx_with_labels().tool_has("save_findings", "external_effect"));
    }

    /// A rule asking about a tool the operator never declared must not fire.
    #[test]
    fn tool_has_is_false_for_an_unknown_tool() {
        assert!(!ctx_with_labels().tool_has("ghost", "reads_untrusted"));
    }

    /// A rule asking about a label that does not exist must not match
    /// everything. This is the direction that would fail open.
    #[test]
    fn tool_has_is_false_for_an_unknown_label() {
        assert!(!ctx_with_labels().tool_has("web_search", "reads_untrused"));
    }

    /// Labels are absent in passthrough mode, and a rule reading them must
    /// still evaluate rather than panic.
    #[test]
    fn tool_has_is_false_on_a_default_context() {
        assert!(!PolicyContext::default().tool_has("anything", "destructive"));
    }

    #[test]
    fn tools_with_collects_every_tool_carrying_a_label() {
        let mut ctx = ctx_with_labels();
        ctx.tool_labels
            .insert("charge_card".to_string(), vec!["moves_money".to_string()]);

        assert_eq!(ctx.tools_with("moves_money"), vec!["charge_card", "send_outreach"]);
    }

    /// Sorted, not HashMap order, so a rule built on it behaves identically
    /// run to run. Determinism is invariant 1.
    #[test]
    fn tools_with_is_sorted() {
        let mut tool_labels = HashMap::new();
        for name in ["zeta", "alpha", "mid"] {
            tool_labels.insert(name.to_string(), vec!["destructive".to_string()]);
        }
        let ctx = PolicyContext { tool_labels, ..Default::default() };

        assert_eq!(ctx.tools_with("destructive"), vec!["alpha", "mid", "zeta"]);
    }

    #[test]
    fn tools_with_is_empty_for_an_unknown_label() {
        assert!(ctx_with_labels().tools_with("nonexistent").is_empty());
    }
}
