// Policy engine — concrete implementations of the Policy trait.
//
// This crate implements decisions. It does not define the contract.
// The contract (Policy trait, PolicyContext, PolicyDecision) lives in nanny-core.
//
// Rule: all implementations here are pure functions.
// Same context in → same decision out. Always. No exceptions.

use nanny_core::agent::{limits::Limits, state::StopReason};
use nanny_core::policy::{Policy, PolicyContext, PolicyDecision};

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
    /// The numeric limits from nanny.toml.
    limits: Limits,

    /// The tools this execution is permitted to call.
    /// Any tool not in this list is denied immediately.
    allowed_tools: Vec<String>,
}

impl LimitsPolicy {
    /// Create a new LimitsPolicy.
    ///
    /// `limits` comes from nanny.toml → [limits]
    /// `allowed_tools` comes from nanny.toml → [tools] allowed
    pub fn new(limits: Limits, allowed_tools: Vec<String>) -> Self {
        Self {
            limits,
            allowed_tools,
        }
    }
}

impl Policy for LimitsPolicy {
    fn evaluate(&self, ctx: &PolicyContext) -> PolicyDecision {
        // ── Check 1: step count ───────────────────────────────────────────────
        if ctx.step_count >= self.limits.max_steps {
            return PolicyDecision::Deny {
                reason: StopReason::MaxStepsReached,
            };
        }

        // ── Check 2: timeout ──────────────────────────────────────────────────
        if ctx.elapsed_ms >= self.limits.timeout_ms {
            return PolicyDecision::Deny {
                reason: StopReason::TimeoutExpired,
            };
        }

        // ── Check 3: budget ───────────────────────────────────────────────────
        if ctx.cost_units_spent >= self.limits.max_cost_units {
            return PolicyDecision::Deny {
                reason: StopReason::BudgetExhausted,
            };
        }

        // ── Check 4: tool allowlist ───────────────────────────────────────────
        //
        // If a tool is being requested, it must be on the allowlist.
        // If it is not listed — deny immediately. Fail closed.
        // An empty allowlist means no tools are permitted at all.
        if let Some(tool) = &ctx.requested_tool {
            if !self.allowed_tools.contains(tool) {
                return PolicyDecision::Deny {
                    reason: StopReason::ToolDenied {
                        tool_name: tool.clone(),
                    },
                };
            }
        }

        PolicyDecision::Allow
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn base_limits() -> Limits {
        Limits {
            max_steps: 10,
            max_cost_units: 500,
            timeout_ms: 10_000,
        }
    }

    fn base_context() -> PolicyContext {
        PolicyContext {
            step_count: 0,
            elapsed_ms: 0,
            requested_tool: None,
            cost_units_spent: 0,
        }
    }

    fn policy() -> LimitsPolicy {
        LimitsPolicy::new(base_limits(), vec!["http_get".to_string()])
    }

    #[test]
    fn allows_within_limits() {
        let result = policy().evaluate(&base_context());
        assert!(matches!(result, PolicyDecision::Allow));
    }

    #[test]
    fn denies_at_max_steps() {
        let ctx = PolicyContext {
            step_count: 10,
            ..base_context()
        };
        let result = policy().evaluate(&ctx);
        assert!(matches!(
            result,
            PolicyDecision::Deny { reason: StopReason::MaxStepsReached }
        ));
    }

    #[test]
    fn denies_on_timeout() {
        let ctx = PolicyContext {
            elapsed_ms: 10_001,
            ..base_context()
        };
        let result = policy().evaluate(&ctx);
        assert!(matches!(
            result,
            PolicyDecision::Deny { reason: StopReason::TimeoutExpired }
        ));
    }

    #[test]
    fn denies_on_budget_exhausted() {
        let ctx = PolicyContext {
            cost_units_spent: 500,
            ..base_context()
        };
        let result = policy().evaluate(&ctx);
        assert!(matches!(
            result,
            PolicyDecision::Deny { reason: StopReason::BudgetExhausted }
        ));
    }

    #[test]
    fn denies_unlisted_tool() {
        let ctx = PolicyContext {
            requested_tool: Some("write_file".to_string()),
            ..base_context()
        };
        let result = policy().evaluate(&ctx);
        assert!(matches!(
            result,
            PolicyDecision::Deny {
                reason: StopReason::ToolDenied { .. }
            }
        ));
    }

    #[test]
    fn allows_listed_tool() {
        let ctx = PolicyContext {
            requested_tool: Some("http_get".to_string()),
            ..base_context()
        };
        let result = policy().evaluate(&ctx);
        assert!(matches!(result, PolicyDecision::Allow));
    }

    #[test]
    fn step_limit_checked_before_timeout() {
        // Both limits exceeded simultaneously — step count wins because
        // checks run in order. Order must be deterministic.
        let ctx = PolicyContext {
            step_count: 10,
            elapsed_ms: 99_999,
            ..base_context()
        };
        let result = policy().evaluate(&ctx);
        assert!(matches!(
            result,
            PolicyDecision::Deny { reason: StopReason::MaxStepsReached }
        ));
    }
}
