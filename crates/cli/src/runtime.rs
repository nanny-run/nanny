// runtime.rs — Wires NannyConfig into the runtime components.
//
// This is the only place in the codebase where config meets the runtime.
// NannyConfig is the source of truth. Every runtime piece is built from it.
// Same config in → same components out. Always. No hidden state.

use nanny_bridge::BridgeComponents;
use nanny_config::NannyConfig;
use nanny_runtime::ToolRegistry;
use std::collections::HashMap;

// ── RuntimeComponents ─────────────────────────────────────────────────────────

/// The fully wired runtime — the tool registry — ready to run.
///
/// Every field is derived directly from `NannyConfig`.
/// Nothing is hardcoded. Nothing comes from ambient state.
/// Policy enforcement is owned by the bridge (`BridgeComponents`).
pub struct RuntimeComponents {
    /// All registered built-in tools. The policy controls which are permitted.
    pub registry: ToolRegistry,
}

// ── build_from_config ─────────────────────────────────────────────────────────

/// Build all runtime components from a validated `NannyConfig`.
///
/// The mapping is intentionally explicit — every field traces back to config:
///
/// ```text
/// config.tools.*       → ToolRegistry (built-in tool set + cost overrides)
/// ```
pub fn build_from_config(config: &NannyConfig) -> RuntimeComponents {
    // Registry with tokens_per_call overrides from [tools.<name>].
    let mut registry = nanny_runtime::default_registry();
    for (tool_name, tool_cfg) in &config.tools.per_tool {
        if let Some(cost) = tool_cfg.tokens_per_call {
            registry.set_cost_override(tool_name, cost);
        }
    }

    RuntimeComponents { registry }
}

// ── build_bridge_components ───────────────────────────────────────────────────

/// Build the enforcement inputs the bridge needs: the tool allowlist, the
/// per-tool call caps, and the registry of built-in tools.
pub fn build_bridge_components(config: &NannyConfig) -> BridgeComponents {
    let per_tool_max_calls: HashMap<String, u32> = config
        .tools
        .per_tool
        .iter()
        .filter_map(|(name, cfg)| cfg.max_calls.map(|n| (name.clone(), n)))
        .collect();

    let mut registry = nanny_runtime::default_registry();
    for (tool_name, tool_cfg) in &config.tools.per_tool {
        if let Some(cost) = tool_cfg.tokens_per_call {
            registry.set_cost_override(tool_name, cost);
        }
    }

    BridgeComponents {
        registry,
        allowed_tools: config.tools.allowed.clone(),
        per_tool_max_calls,
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use nanny_config::{LimitsConfig, NannyConfig, ObservabilityConfig, ToolsConfig};
    use std::collections::HashMap;

    fn test_config() -> NannyConfig {
        NannyConfig {
            start: None,
            limits: LimitsConfig {
                max_steps: 42,
                max_tokens: 500,
                timeout_ms: 15_000,
                named: HashMap::new(),
            },
            tools: ToolsConfig {
                allowed: vec!["http_get".to_string()],
                per_tool: HashMap::new(),
            },
            observability: ObservabilityConfig::default(),
            proxy: None,
        }
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
    fn bridge_components_carry_the_allowlist() {
        let components = build_bridge_components(&test_config());

        assert_eq!(components.allowed_tools, vec!["http_get".to_string()]);
    }

    #[test]
    fn empty_allowlist_is_valid() {
        let mut config = test_config();
        config.tools.allowed = vec![];

        let components = build_bridge_components(&config);
        assert!(components.allowed_tools.is_empty());
    }

    #[test]
    fn max_calls_reaches_the_bridge() {
        let mut config = test_config();
        config.tools.per_tool.insert(
            "http_get".to_string(),
            nanny_config::ToolConfig { tokens_per_call: None, max_calls: Some(3) },
        );

        let components = build_bridge_components(&config);
        assert_eq!(components.per_tool_max_calls.get("http_get"), Some(&3));
    }
}
