// runtime.rs: Wires NannyConfig into the runtime components.
//
// This is the only place in the codebase where config meets the runtime.
// NannyConfig is the source of truth. Every runtime piece is built from it.
// Same config in → same components out. Always. No hidden state.

use nanny_bridge::BridgeComponents;
use nanny_config::NannyConfig;
use std::collections::HashMap;

// ── RuntimeComponents ─────────────────────────────────────────────────────────

// ── build_from_config ─────────────────────────────────────────────────────────


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

    // Every allowlisted tool gets an entry, including the unlabelled ones.
    // "declared with no labels" and "never declared" are different answers,
    // and `tool_has` must be able to tell them apart.
    let tool_labels: HashMap<String, Vec<String>> = config
        .tools
        .allowed
        .iter()
        .map(|name| {
            let labels = config
                .tools
                .per_tool
                .get(name)
                .map(|cfg| cfg.labels().iter().map(|l| l.to_string()).collect())
                .unwrap_or_default();
            (name.clone(), labels)
        })
        .collect();

    BridgeComponents {
        registry: nanny_runtime::default_registry(),
        allowed_tools: config.tools.allowed.clone(),
        per_tool_max_calls,
        tool_labels,
        config_hash: config.fingerprint(),
        runtime_version: env!("CARGO_PKG_VERSION").to_string(),
        // Filled in by the caller that knows whether this governor launches an
        // app of its own. A headless one, and any run arriving over `--join`,
        // leaves it empty rather than claiming a command it did not start.
        start_command: None,
    }
}


// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use nanny_config::{NannyConfig, ObservabilityConfig, ToolsConfig};
    use std::collections::HashMap;

    fn test_config() -> NannyConfig {
        NannyConfig {
            start: None,
            tools: ToolsConfig {
                allowed: vec!["http_get".to_string()],
                per_tool: HashMap::new(),
            },
            observability: ObservabilityConfig::default(),
            rules: Default::default(),
        }
    }

    #[test]
    fn registry_contains_http_get() {
        let components = build_bridge_components(&test_config());

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
            nanny_config::ToolConfig {
                max_calls: Some(3),
                ..Default::default()
            },
        );

        let components = build_bridge_components(&config);
        assert_eq!(components.per_tool_max_calls.get("http_get"), Some(&3));
    }
}
