// runtime.rs — Wires NannyConfig into the runtime components.
//
// This is the only place in the codebase where config meets the runtime.
// NannyConfig is the source of truth. Every runtime piece is built from it.
// Same config in → same components out. Always. No hidden state.

use nanny_config::NannyConfig;
use nanny_core::agent::limits::Limits;
use nanny_ledger::FakeLedger;
use nanny_policy::LimitsPolicy;
use nanny_tools::ToolRegistry;

// ── RuntimeComponents ─────────────────────────────────────────────────────────

/// The fully wired runtime — policy, ledger, and tool registry — ready to run.
///
/// Every field is derived directly from `NannyConfig`.
/// Nothing is hardcoded. Nothing comes from ambient state.
pub struct RuntimeComponents {
    /// Hard limits passed to the Executor.
    pub limits: Limits,

    /// Evaluates every step and tool call. Enforces all four hard limits.
    pub policy: LimitsPolicy,

    /// In-memory budget ledger. Starts at `max_cost_units` and counts down.
    pub ledger: FakeLedger,

    /// All registered built-in tools. The policy controls which are permitted.
    pub registry: ToolRegistry,
}

// ── build_from_config ─────────────────────────────────────────────────────────

/// Build all runtime components from a validated `NannyConfig`.
///
/// The mapping is intentionally explicit — every field traces back to config:
///
/// ```text
/// config.limits.max_steps      → Limits  → LimitsPolicy (step check)
/// config.limits.timeout_ms     → Limits  → LimitsPolicy (timeout check)
/// config.limits.max_cost_units → Limits  → LimitsPolicy (budget check)
///                                        → FakeLedger   (starting balance)
/// config.tools.allowed         →           LimitsPolicy (allowlist check)
/// ```
pub fn build_from_config(config: &NannyConfig) -> RuntimeComponents {
    // Convert config's limit fields into the core Limits type.
    // The fields are identical — the mapping is explicit by design.
    let limits = Limits {
        max_steps: config.limits.max_steps,
        max_cost_units: config.limits.max_cost_units,
        timeout_ms: config.limits.timeout_ms,
    };

    // Policy enforces all four checks: steps, timeout, budget, tool allowlist.
    let policy = LimitsPolicy::new(limits.clone(), config.tools.allowed.clone());

    // Ledger starts at the full budget. The policy stops execution when
    // cost_units_spent >= max_cost_units — so the two must be in sync.
    let ledger = FakeLedger::new(config.limits.max_cost_units);

    // Registry provides built-in tools. Policy controls which are permitted.
    // Both are required — registry = capability, policy = permission.
    let registry = nanny_tools::default_registry();

    RuntimeComponents {
        limits,
        policy,
        ledger,
        registry,
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use nanny_config::{LimitsConfig, Mode, NannyConfig, ToolsConfig};
    use nanny_core::ledger::Ledger;

    fn test_config() -> NannyConfig {
        NannyConfig {
            mode: Mode::Local,
            limits: LimitsConfig {
                max_steps: 42,
                max_cost_units: 500,
                timeout_ms: 15_000,
            },
            tools: ToolsConfig {
                allowed: vec!["http_get".to_string()],
            },
        }
    }

    #[test]
    fn limits_match_config() {
        let components = build_from_config(&test_config());

        assert_eq!(components.limits.max_steps, 42);
        assert_eq!(components.limits.max_cost_units, 500);
        assert_eq!(components.limits.timeout_ms, 15_000);
    }

    #[test]
    fn ledger_starts_at_max_cost_units() {
        let config = test_config();
        let components = build_from_config(&config);

        // Ledger balance must equal the configured budget.
        // If they diverge, BudgetExhausted fires at the wrong time.
        assert_eq!(components.ledger.balance(), config.limits.max_cost_units);
    }

    #[test]
    fn registry_contains_http_get() {
        let components = build_from_config(&test_config());

        assert!(
            components.registry.registered_names().contains(&"http_get"),
            "http_get must always be registered by default"
        );
    }

    #[test]
    fn same_config_produces_same_limits_and_balance() {
        let config = test_config();
        let c1 = build_from_config(&config);
        let c2 = build_from_config(&config);

        assert_eq!(c1.limits.max_steps, c2.limits.max_steps);
        assert_eq!(c1.limits.max_cost_units, c2.limits.max_cost_units);
        assert_eq!(c1.limits.timeout_ms, c2.limits.timeout_ms);
        assert_eq!(c1.ledger.balance(), c2.ledger.balance());
    }

    #[test]
    fn empty_allowlist_is_valid() {
        // No allowed tools is a valid config — it denies every tool call.
        let config = NannyConfig {
            mode: Mode::Local,
            limits: LimitsConfig {
                max_steps: 10,
                max_cost_units: 100,
                timeout_ms: 5_000,
            },
            tools: ToolsConfig { allowed: vec![] },
        };

        let components = build_from_config(&config);
        assert_eq!(components.limits.max_steps, 10);
        assert_eq!(components.ledger.balance(), 100);
    }
}
