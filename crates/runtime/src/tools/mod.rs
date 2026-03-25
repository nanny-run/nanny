// nanny-runtime tools — Tool registry and built-in tool implementations.
//
// This module owns:
// - ToolRegistry: a collection of registered tools, implements ToolExecutor
// - Built-in tools: http_get, and others as they are added
//
// The executor in nanny-core programs against ToolExecutor (the trait).
// ToolRegistry is the concrete implementation of that trait.

pub mod http_get;

pub use http_get::HttpGet;

use nanny_core::tool::{Tool, ToolArgs, ToolCallError, ToolExecutor, ToolOutput};
use std::collections::HashMap;

// ── ToolRegistry ──────────────────────────────────────────────────────────────

/// A collection of registered tools.
///
/// Implements `ToolExecutor` so the executor can call tools by name
/// without knowing anything about their implementation.
///
/// Tools are registered once at startup and never mutated during execution.
/// If the tool name is not in the registry, `call()` returns `NotFound`.
pub struct ToolRegistry {
    /// Map from tool name to boxed implementation.
    ///
    /// `Box<dyn Tool>` means any type implementing `Tool` can be stored here —
    /// regardless of its concrete type. This is Rust's runtime polymorphism.
    tools: HashMap<String, Box<dyn Tool>>,

    /// Cost overrides from nanny.toml [tools.<name>] cost_per_call.
    /// When set, this value replaces the tool's own declared_cost().
    cost_overrides: HashMap<String, u64>,
}

impl ToolRegistry {
    /// Create an empty registry.
    pub fn new() -> Self {
        Self {
            tools: HashMap::new(),
            cost_overrides: HashMap::new(),
        }
    }

    /// Override the declared cost for a tool.
    ///
    /// Reads from nanny.toml `[tools.<name>] cost_per_call`.
    /// When set, `declared_cost()` returns this value instead of the
    /// tool's own declared cost.
    pub fn set_cost_override(&mut self, tool_name: &str, cost: u64) {
        self.cost_overrides.insert(tool_name.to_string(), cost);
    }

    /// Register a tool.
    ///
    /// If a tool with the same name is already registered, it is replaced.
    /// The registry takes ownership of the tool via `Box<dyn Tool>`.
    pub fn register(&mut self, tool: Box<dyn Tool>) {
        self.tools.insert(tool.name().to_string(), tool);
    }

    /// Return the names of all registered tools.
    ///
    /// Useful for debugging and `nanny init` suggestions.
    pub fn registered_names(&self) -> Vec<&str> {
        self.tools.keys().map(|s| s.as_str()).collect()
    }
}

impl Default for ToolRegistry {
    fn default() -> Self {
        Self::new()
    }
}

/// Create a registry pre-loaded with all built-in Nanny tools.
///
/// Currently includes:
/// - `http_get` — makes a single HTTP GET request
///
/// This is the standard starting point for most executions.
/// Register additional tools on top of this if needed.
pub fn default_registry() -> ToolRegistry {
    let mut registry = ToolRegistry::new();
    registry.register(Box::new(HttpGet::new()));
    registry
}

impl ToolExecutor for ToolRegistry {
    /// Call a registered tool by name.
    ///
    /// Returns `NotFound` if no tool with that name is registered.
    /// Returns `Execution` if the tool was found but failed.
    fn call(&self, name: &str, args: &ToolArgs) -> Result<ToolOutput, ToolCallError> {
        match self.tools.get(name) {
            None => Err(ToolCallError::NotFound {
                tool_name: name.to_string(),
            }),
            Some(tool) => tool.execute(args).map_err(|source| ToolCallError::Execution {
                tool_name: name.to_string(),
                source,
            }),
        }
    }

    /// Return the cost of a registered tool.
    ///
    /// If a cost override was set via `set_cost_override`, that value is used.
    /// Otherwise falls back to the tool's own declared cost.
    /// Returns `None` if the tool is not registered.
    fn declared_cost(&self, name: &str) -> Option<u64> {
        if self.tools.contains_key(name) {
            Some(self.cost_overrides.get(name).copied()
                .unwrap_or_else(|| self.tools[name].declared_cost()))
        } else {
            None
        }
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use nanny_core::tool::{ToolError, ToolOutput};

    // A minimal tool for testing — always succeeds, costs 5 units.
    struct EchoTool;
    impl Tool for EchoTool {
        fn name(&self) -> &str { "echo" }
        fn declared_cost(&self) -> u64 { 5 }
        fn execute(&self, args: &ToolArgs) -> Result<ToolOutput, ToolError> {
            let message = args.get("message").cloned().unwrap_or_default();
            Ok(ToolOutput { content: message })
        }
    }

    // A tool that always fails.
    struct FailingTool;
    impl Tool for FailingTool {
        fn name(&self) -> &str { "failing" }
        fn declared_cost(&self) -> u64 { 1 }
        fn execute(&self, _: &ToolArgs) -> Result<ToolOutput, ToolError> {
            Err(ToolError::ExecutionFailed("always fails".to_string()))
        }
    }

    #[test]
    fn calls_registered_tool() {
        let mut registry = ToolRegistry::new();
        registry.register(Box::new(EchoTool));

        let mut args = ToolArgs::new();
        args.insert("message".to_string(), "hello".to_string());

        let result = registry.call("echo", &args);
        assert!(result.is_ok());
        assert_eq!(result.unwrap().content, "hello");
    }

    #[test]
    fn returns_not_found_for_unknown_tool() {
        let registry = ToolRegistry::new();
        let result = registry.call("unknown", &ToolArgs::new());

        assert!(matches!(result, Err(ToolCallError::NotFound { .. })));
    }

    #[test]
    fn returns_execution_error_on_tool_failure() {
        let mut registry = ToolRegistry::new();
        registry.register(Box::new(FailingTool));

        let result = registry.call("failing", &ToolArgs::new());
        assert!(matches!(result, Err(ToolCallError::Execution { .. })));
    }

    #[test]
    fn declared_cost_returns_correct_value() {
        let mut registry = ToolRegistry::new();
        registry.register(Box::new(EchoTool));

        assert_eq!(registry.declared_cost("echo"), Some(5));
        assert_eq!(registry.declared_cost("unknown"), None);
    }

    #[test]
    fn cost_override_replaces_declared_cost() {
        let mut registry = ToolRegistry::new();
        registry.register(Box::new(EchoTool)); // declared_cost = 5
        registry.set_cost_override("echo", 99);

        assert_eq!(registry.declared_cost("echo"), Some(99));
    }

    #[test]
    fn cost_override_does_not_affect_unregistered_tool() {
        let mut registry = ToolRegistry::new();
        registry.set_cost_override("ghost", 50); // tool not registered
        assert_eq!(registry.declared_cost("ghost"), None);
    }

    #[test]
    fn registered_names_lists_all_tools() {
        let mut registry = ToolRegistry::new();
        registry.register(Box::new(EchoTool));
        registry.register(Box::new(FailingTool));

        let mut names = registry.registered_names();
        names.sort();
        assert_eq!(names, vec!["echo", "failing"]);
    }
}
