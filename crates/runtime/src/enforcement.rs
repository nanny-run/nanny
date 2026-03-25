// enforcement.rs — Concrete policy implementations.
//
// These are the enforcement decisions. The contract (Policy trait, PolicyContext,
// PolicyDecision) lives in nanny-core.
//
// Rule: all implementations here are pure functions.
// Same context in → same decision out. Always. No exceptions.

use nanny_core::agent::{limits::Limits, state::StopReason};
use nanny_core::policy::{Policy, PolicyContext, PolicyDecision};
use std::collections::HashMap;

// ── LimitsPolicy ──────────────────────────────────────────────────────────────

/// The standard policy for a single execution.
///
/// Enforces all four hard limits:
///   1. Maximum step count
///   2. Wall-clock timeout
///   3. Budget (cost units)
///   4. Tool allowlist
///
/// Checks are evaluated in order. The first failing check stops execution.
/// All checks are pure — no state is mutated, no network calls are made.
pub struct LimitsPolicy {
    limits: Limits,
    allowed_tools: Vec<String>,
}

impl LimitsPolicy {
    pub fn new(limits: Limits, allowed_tools: Vec<String>) -> Self {
        Self { limits, allowed_tools }
    }
}

impl Policy for LimitsPolicy {
    fn evaluate(&self, ctx: &PolicyContext) -> PolicyDecision {
        if ctx.step_count >= self.limits.max_steps {
            return PolicyDecision::Deny { reason: StopReason::MaxStepsReached };
        }
        if ctx.elapsed_ms >= self.limits.timeout_ms {
            return PolicyDecision::Deny { reason: StopReason::TimeoutExpired };
        }
        if ctx.cost_units_spent >= self.limits.max_cost_units {
            return PolicyDecision::Deny { reason: StopReason::BudgetExhausted };
        }
        if let Some(tool) = &ctx.requested_tool {
            if !self.allowed_tools.contains(tool) {
                return PolicyDecision::Deny {
                    reason: StopReason::ToolDenied { tool_name: tool.clone() },
                };
            }
        }
        PolicyDecision::Allow
    }
}

// ── RuleEvaluator ─────────────────────────────────────────────────────────────

/// Enforces per-tool rules declared in nanny.toml under [tools.<name>].
///
/// Currently enforces:
///   - `max_calls`: deny once a tool has been called max_calls times
///
/// Always runs after LimitsPolicy — compose them with ChainPolicy.
pub struct RuleEvaluator {
    max_calls: HashMap<String, u32>,
}

impl RuleEvaluator {
    pub fn new(max_calls: HashMap<String, u32>) -> Self {
        Self { max_calls }
    }
}

impl Policy for RuleEvaluator {
    fn evaluate(&self, ctx: &PolicyContext) -> PolicyDecision {
        let tool = match &ctx.requested_tool {
            Some(t) => t,
            None => return PolicyDecision::Allow,
        };
        if let Some(&max) = self.max_calls.get(tool) {
            let calls_so_far = ctx.tool_call_counts.get(tool).copied().unwrap_or(0);
            if calls_so_far >= max {
                return PolicyDecision::Deny {
                    reason: StopReason::RuleDenied {
                        rule_name: format!("{tool}.max_calls"),
                    },
                };
            }
        }
        PolicyDecision::Allow
    }
}

// ── ChainPolicy ───────────────────────────────────────────────────────────────

/// Composes two policies in sequence. First denial wins.
pub struct ChainPolicy<A, B> {
    first: A,
    second: B,
}

impl<A, B> ChainPolicy<A, B> {
    pub fn new(first: A, second: B) -> Self {
        Self { first, second }
    }
}

impl<A: Policy, B: Policy> Policy for ChainPolicy<A, B> {
    fn evaluate(&self, ctx: &PolicyContext) -> PolicyDecision {
        match self.first.evaluate(ctx) {
            PolicyDecision::Allow => self.second.evaluate(ctx),
            deny => deny,
        }
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn base_limits() -> Limits {
        Limits { max_steps: 10, max_cost_units: 500, timeout_ms: 10_000 }
    }

    fn base_context() -> PolicyContext {
        PolicyContext::default()
    }

    fn policy() -> LimitsPolicy {
        LimitsPolicy::new(base_limits(), vec!["http_get".to_string()])
    }

    #[test]
    fn allows_within_limits() {
        assert!(matches!(policy().evaluate(&base_context()), PolicyDecision::Allow));
    }

    #[test]
    fn denies_at_max_steps() {
        let ctx = PolicyContext { step_count: 10, ..base_context() };
        assert!(matches!(
            policy().evaluate(&ctx),
            PolicyDecision::Deny { reason: StopReason::MaxStepsReached }
        ));
    }

    #[test]
    fn denies_on_timeout() {
        let ctx = PolicyContext { elapsed_ms: 10_001, ..base_context() };
        assert!(matches!(
            policy().evaluate(&ctx),
            PolicyDecision::Deny { reason: StopReason::TimeoutExpired }
        ));
    }

    #[test]
    fn denies_on_budget_exhausted() {
        let ctx = PolicyContext { cost_units_spent: 500, ..base_context() };
        assert!(matches!(
            policy().evaluate(&ctx),
            PolicyDecision::Deny { reason: StopReason::BudgetExhausted }
        ));
    }

    #[test]
    fn denies_unlisted_tool() {
        let ctx = PolicyContext {
            requested_tool: Some("write_file".to_string()),
            ..base_context()
        };
        assert!(matches!(
            policy().evaluate(&ctx),
            PolicyDecision::Deny { reason: StopReason::ToolDenied { .. } }
        ));
    }

    #[test]
    fn allows_listed_tool() {
        let ctx = PolicyContext {
            requested_tool: Some("http_get".to_string()),
            ..base_context()
        };
        assert!(matches!(policy().evaluate(&ctx), PolicyDecision::Allow));
    }

    #[test]
    fn step_limit_checked_before_timeout() {
        let ctx = PolicyContext { step_count: 10, elapsed_ms: 99_999, ..base_context() };
        assert!(matches!(
            policy().evaluate(&ctx),
            PolicyDecision::Deny { reason: StopReason::MaxStepsReached }
        ));
    }

    fn rule_evaluator_with_http_get_limit(max: u32) -> RuleEvaluator {
        let mut map = HashMap::new();
        map.insert("http_get".to_string(), max);
        RuleEvaluator::new(map)
    }

    #[test]
    fn rule_evaluator_allows_when_under_limit() {
        let re = rule_evaluator_with_http_get_limit(3);
        let mut counts = HashMap::new();
        counts.insert("http_get".to_string(), 2u32);
        let ctx = PolicyContext {
            requested_tool: Some("http_get".to_string()),
            tool_call_counts: counts,
            ..base_context()
        };
        assert!(matches!(re.evaluate(&ctx), PolicyDecision::Allow));
    }

    #[test]
    fn rule_evaluator_denies_at_max_calls() {
        let re = rule_evaluator_with_http_get_limit(3);
        let mut counts = HashMap::new();
        counts.insert("http_get".to_string(), 3u32);
        let ctx = PolicyContext {
            requested_tool: Some("http_get".to_string()),
            tool_call_counts: counts,
            ..base_context()
        };
        assert!(matches!(
            re.evaluate(&ctx),
            PolicyDecision::Deny {
                reason: StopReason::RuleDenied { ref rule_name }
            } if rule_name == "http_get.max_calls"
        ));
    }

    #[test]
    fn rule_evaluator_ignores_unconfigured_tools() {
        let re = rule_evaluator_with_http_get_limit(1);
        let ctx = PolicyContext {
            requested_tool: Some("write_file".to_string()),
            ..base_context()
        };
        assert!(matches!(re.evaluate(&ctx), PolicyDecision::Allow));
    }

    #[test]
    fn rule_evaluator_allows_when_no_tool_requested() {
        let re = rule_evaluator_with_http_get_limit(1);
        assert!(matches!(re.evaluate(&base_context()), PolicyDecision::Allow));
    }

    #[test]
    fn chain_allows_when_both_allow() {
        let chain = ChainPolicy::new(
            RuleEvaluator::new(HashMap::new()),
            RuleEvaluator::new(HashMap::new()),
        );
        assert!(matches!(chain.evaluate(&base_context()), PolicyDecision::Allow));
    }

    #[test]
    fn chain_denies_when_first_denies() {
        let first = LimitsPolicy::new(
            Limits { max_steps: 0, max_cost_units: 999, timeout_ms: 99_999 },
            vec![],
        );
        let second = RuleEvaluator::new(HashMap::new());
        let chain = ChainPolicy::new(first, second);
        assert!(matches!(
            chain.evaluate(&base_context()),
            PolicyDecision::Deny { reason: StopReason::MaxStepsReached }
        ));
    }

    #[test]
    fn chain_denies_when_second_denies() {
        let first = RuleEvaluator::new(HashMap::new());
        let re = rule_evaluator_with_http_get_limit(1);
        let chain = ChainPolicy::new(first, re);
        let mut counts = HashMap::new();
        counts.insert("http_get".to_string(), 1u32);
        let ctx = PolicyContext {
            requested_tool: Some("http_get".to_string()),
            tool_call_counts: counts,
            ..base_context()
        };
        assert!(matches!(
            chain.evaluate(&ctx),
            PolicyDecision::Deny { reason: StopReason::RuleDenied { .. } }
        ));
    }

    #[test]
    fn chain_first_denial_wins_over_second() {
        let first = LimitsPolicy::new(
            Limits { max_steps: 0, max_cost_units: 999, timeout_ms: 99_999 },
            vec![],
        );
        let mut max_calls = HashMap::new();
        max_calls.insert("http_get".to_string(), 0u32);
        let second = RuleEvaluator::new(max_calls);
        let chain = ChainPolicy::new(first, second);
        let ctx = PolicyContext {
            requested_tool: Some("http_get".to_string()),
            ..base_context()
        };
        assert!(matches!(
            chain.evaluate(&ctx),
            PolicyDecision::Deny { reason: StopReason::MaxStepsReached }
        ));
    }
}
