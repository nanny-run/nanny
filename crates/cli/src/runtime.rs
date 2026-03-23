// runtime.rs — Wires NannyConfig into the runtime components.
//
// This is the only place in the codebase where config meets the runtime.
// NannyConfig is the source of truth. Every runtime piece is built from it.
// Same config in → same components out. Always. No hidden state.

use nanny_config::{resolve_named_limits, ConfigError, NannyConfig};
use nanny_core::agent::limits::Limits;
use nanny_ledger::FakeLedger;
use nanny_policy::{ChainPolicy, LimitsPolicy, RuleEvaluator};
use nanny_tools::ToolRegistry;
use std::collections::HashMap;

// ── RuntimeComponents ─────────────────────────────────────────────────────────

/// The fully wired runtime — policy, ledger, and tool registry — ready to run.
///
/// Every field is derived directly from `NannyConfig`.
/// Nothing is hardcoded. Nothing comes from ambient state.
pub struct RuntimeComponents {
    /// Hard limits passed to the Executor.
    pub limits: Limits,

    /// Evaluates every step and tool call.
    /// LimitsPolicy (hard limits) chained with RuleEvaluator (per-tool rules).
    /// Transferred to the bridge in v0.2.0.
    #[allow(dead_code)]
    pub policy: ChainPolicy<LimitsPolicy, RuleEvaluator>,

    /// In-memory budget ledger. Starts at `max_cost_units` and counts down.
    pub ledger: FakeLedger,

    /// All registered built-in tools. The policy controls which are permitted.
    pub registry: ToolRegistry,
}

// ── build_from_config ─────────────────────────────────────────────────────────

/// Build all runtime components from a validated `NannyConfig`.
///
/// Uses the global [limits] defaults. To use a named limits set, call
/// `build_from_config_named` instead.
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
    let limits = Limits {
        max_steps: config.limits.max_steps,
        max_cost_units: config.limits.max_cost_units,
        timeout_ms: config.limits.timeout_ms,
    };
    build_components(config, limits)
}

// ── build_from_config_named ───────────────────────────────────────────────────

/// Build runtime components using a named limits set from config.
///
/// The named set inherits from [limits] and overrides only what it declares.
/// Returns `Err` if the named set does not exist in config.
///
/// Example: `build_from_config_named(&config, "researcher")` uses
/// the [limits.researcher] table from nanny.toml.
pub fn build_from_config_named(
    config: &NannyConfig,
    name: &str,
) -> Result<RuntimeComponents, ConfigError> {
    let resolved = resolve_named_limits(config, name)?;

    let limits = Limits {
        max_steps: resolved.max_steps,
        max_cost_units: resolved.max_cost_units,
        timeout_ms: resolved.timeout_ms,
    };

    Ok(build_components(config, limits))
}

// ── Internal ──────────────────────────────────────────────────────────────────

/// Construct components from a resolved Limits value.
/// Shared by both build_from_config and build_from_config_named.
fn build_components(config: &NannyConfig, limits: Limits) -> RuntimeComponents {
    // LimitsPolicy: hard limits — steps, timeout, budget, tool allowlist.
    let limits_policy = LimitsPolicy::new(limits.clone(), config.tools.allowed.clone());

    // RuleEvaluator: per-tool rules from [tools.<name>] — currently max_calls.
    let max_calls: HashMap<String, u32> = config.tools.per_tool
        .iter()
        .filter_map(|(name, cfg)| cfg.max_calls.map(|n| (name.clone(), n)))
        .collect();
    let rule_evaluator = RuleEvaluator::new(max_calls);

    // Chain: LimitsPolicy runs first; RuleEvaluator only consulted if it allows.
    let policy = ChainPolicy::new(limits_policy, rule_evaluator);

    // Ledger starts at the full budget. The policy stops execution when
    // cost_units_spent >= max_cost_units — so the two must be in sync.
    let ledger = FakeLedger::new(limits.max_cost_units);

    // Registry with cost_per_call overrides from [tools.<name>].
    let mut registry = nanny_tools::default_registry();
    for (tool_name, tool_cfg) in &config.tools.per_tool {
        if let Some(cost) = tool_cfg.cost_per_call {
            registry.set_cost_override(tool_name, cost);
        }
    }

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
    use nanny_config::{
        LimitsConfig, ManagedConfig, NannyConfig, ObservabilityConfig, PartialLimitsConfig,
        RuntimeConfig, ToolsConfig,
    };
    use nanny_core::ledger::Ledger;
    use std::collections::HashMap;

    fn test_config() -> NannyConfig {
        NannyConfig {
            runtime: RuntimeConfig::default(),
            limits: LimitsConfig {
                max_steps: 42,
                max_cost_units: 500,
                timeout_ms: 15_000,
                named: HashMap::new(),
            },
            tools: ToolsConfig {
                allowed: vec!["http_get".to_string()],
                per_tool: HashMap::new(),
            },
            observability: ObservabilityConfig::default(),
            managed: None,
        }
    }

    fn config_with_named_limits() -> NannyConfig {
        let mut named = HashMap::new();
        named.insert(
            "researcher".to_string(),
            PartialLimitsConfig {
                max_steps: Some(200),
                max_cost_units: Some(2000),
                timeout_ms: None, // inherits from global
            },
        );

        NannyConfig {
            runtime: RuntimeConfig::default(),
            limits: LimitsConfig {
                max_steps: 42,
                max_cost_units: 500,
                timeout_ms: 15_000,
                named,
            },
            tools: ToolsConfig {
                allowed: vec!["http_get".to_string()],
                per_tool: HashMap::new(),
            },
            observability: ObservabilityConfig::default(),
            managed: None,
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
        let config = NannyConfig {
            runtime: RuntimeConfig::default(),
            limits: LimitsConfig {
                max_steps: 10,
                max_cost_units: 100,
                timeout_ms: 5_000,
                named: HashMap::new(),
            },
            tools: ToolsConfig {
                allowed: vec![],
                per_tool: HashMap::new(),
            },
            observability: ObservabilityConfig::default(),
            managed: None,
        };

        let components = build_from_config(&config);
        assert_eq!(components.limits.max_steps, 10);
        assert_eq!(components.ledger.balance(), 100);
    }

    #[test]
    fn named_limits_override_and_inherit() {
        let config = config_with_named_limits();
        let components =
            build_from_config_named(&config, "researcher").expect("researcher must exist");

        // Overridden fields
        assert_eq!(components.limits.max_steps, 200);
        assert_eq!(components.limits.max_cost_units, 2000);

        // Inherited field (timeout_ms is None in partial, inherits from global 15_000)
        assert_eq!(components.limits.timeout_ms, 15_000);

        // Ledger synced to named budget
        assert_eq!(components.ledger.balance(), 2000);
    }

    #[test]
    fn named_limits_not_found_returns_error() {
        let config = test_config(); // no named limits defined
        let result = build_from_config_named(&config, "nonexistent");

        assert!(
            matches!(result, Err(ConfigError::NamedLimitsNotFound { .. })),
            "missing named limits must return NamedLimitsNotFound"
        );
    }

    #[test]
    fn managed_config_compiles() {
        // Ensures ManagedConfig is importable and the managed field works.
        let config = NannyConfig {
            runtime: RuntimeConfig::default(),
            limits: LimitsConfig {
                max_steps: 10,
                max_cost_units: 100,
                timeout_ms: 5_000,
                named: HashMap::new(),
            },
            tools: ToolsConfig::default(),
            observability: ObservabilityConfig::default(),
            managed: Some(ManagedConfig {
                endpoint: "https://api.nanny.run".to_string(),
                org_id: "org_test".to_string(),
                api_key: "nny_test_key".to_string(),
            }),
        };

        // build_from_config uses runtime limits regardless of managed presence
        let components = build_from_config(&config);
        assert_eq!(components.limits.max_steps, 10);
    }

    #[test]
    fn cost_per_call_override_applied_to_registry() {
        use nanny_config::ToolConfig;
        use nanny_core::tool::ToolExecutor;

        let mut per_tool = HashMap::new();
        per_tool.insert(
            "http_get".to_string(),
            ToolConfig { max_calls: None, cost_per_call: Some(25) },
        );
        let config = NannyConfig {
            runtime: RuntimeConfig::default(),
            limits: LimitsConfig {
                max_steps: 10,
                max_cost_units: 500,
                timeout_ms: 5_000,
                named: HashMap::new(),
            },
            tools: ToolsConfig {
                allowed: vec!["http_get".to_string()],
                per_tool,
            },
            observability: ObservabilityConfig::default(),
            managed: None,
        };

        let components = build_from_config(&config);
        assert_eq!(
            components.registry.declared_cost("http_get"),
            Some(25),
            "cost_per_call from config must override tool's declared cost"
        );
    }

    #[test]
    fn max_calls_config_wired_into_policy() {
        use nanny_config::ToolConfig;
        use nanny_core::policy::{Policy, PolicyContext, PolicyDecision};
        use nanny_core::agent::state::StopReason;

        let mut per_tool = HashMap::new();
        per_tool.insert(
            "http_get".to_string(),
            ToolConfig { max_calls: Some(2), cost_per_call: None },
        );
        let config = NannyConfig {
            runtime: RuntimeConfig::default(),
            limits: LimitsConfig {
                max_steps: 100,
                max_cost_units: 1000,
                timeout_ms: 30_000,
                named: HashMap::new(),
            },
            tools: ToolsConfig {
                allowed: vec!["http_get".to_string()],
                per_tool,
            },
            observability: ObservabilityConfig::default(),
            managed: None,
        };

        let components = build_from_config(&config);

        // Two calls already made — third must be denied.
        let mut counts = HashMap::new();
        counts.insert("http_get".to_string(), 2u32);
        let ctx = PolicyContext {
            requested_tool: Some("http_get".to_string()),
            tool_call_counts: counts,
            ..PolicyContext::default()
        };

        assert!(
            matches!(
                components.policy.evaluate(&ctx),
                PolicyDecision::Deny {
                    reason: StopReason::RuleDenied { ref rule_name }
                } if rule_name == "http_get.max_calls"
            ),
            "policy must deny when tool_call_counts >= max_calls"
        );
    }
}
