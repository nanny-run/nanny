// The Tool contract.
//
// nanny-core defines the shapes.
// Concrete tool implementations live in nanny-tools.
//
// The executor programs against ToolExecutor — not against any specific tool.
// This means: add a new tool, replace a tool, sandbox a tool in WASM —
// the executor never changes.

use std::collections::HashMap;
use thiserror::Error;

// ── ToolArgs ──────────────────────────────────────────────────────────────────

/// The arguments passed to a tool call.
///
/// A flat key-value map. Tools declare which keys they expect
/// and validate them inside `execute()`.
/// The executor passes whatever the agent provided — no pre-filtering.
pub type ToolArgs = HashMap<String, String>;

// ── ToolOutput ────────────────────────────────────────────────────────────────

/// The result of a successful tool execution.
///
/// Kept intentionally simple — structured output parsing belongs
/// to the agent layer above, not the executor.
#[derive(Debug, Clone)]
pub struct ToolOutput {
    /// The raw content returned by the tool.
    pub content: String,
}

// ── ToolError ─────────────────────────────────────────────────────────────────

/// Errors a tool can produce during execution.
///
/// These represent tool-level failures — bad args, network errors, timeouts.
/// They are distinct from policy denials: a tool error means the tool was
/// permitted but failed during execution. A policy denial means the tool
/// was never called at all.
#[derive(Debug, Error)]
pub enum ToolError {
    /// A required argument was missing or had an invalid value.
    #[error("invalid argument '{arg}': {reason}")]
    InvalidArgument { arg: String, reason: String },

    /// The tool ran but encountered an error during execution.
    #[error("execution failed: {0}")]
    ExecutionFailed(String),

    /// The tool did not complete within its allowed time.
    #[error("timed out after {timeout_ms}ms")]
    Timeout { timeout_ms: u64 },
}

// ── Tool trait ────────────────────────────────────────────────────────────────

/// The contract for a single tool.
///
/// Any type that implements this can be registered and called by the executor.
/// Tools declare their own cost — the executor charges that amount
/// when the tool is called successfully.
pub trait Tool: Send + Sync {
    /// The unique name used to identify this tool in config and agent output.
    /// Must be stable — changing this is a breaking change.
    fn name(&self) -> &str;

    /// Cost units charged when this tool is called successfully.
    ///
    /// No charge on failure — the budget is only spent when work is done.
    fn declared_cost(&self) -> u64;

    /// Execute the tool with the given arguments.
    ///
    /// Returns `Ok(ToolOutput)` on success — cost is then charged.
    /// Returns `Err(ToolError)` on failure — no cost is charged.
    fn execute(&self, args: &ToolArgs) -> Result<ToolOutput, ToolError>;
}

// ── ToolCallError ─────────────────────────────────────────────────────────────

/// What can go wrong when the executor calls a tool via the registry.
///
/// Two cases only:
/// - The tool name is not registered (config allows it but nobody registered it)
/// - The tool is registered but failed during execution
///
/// Policy denial is not represented here — that stops the executor
/// before `call()` is ever invoked.
#[derive(Debug, Error)]
pub enum ToolCallError {
    /// The tool name was not found in the registry.
    #[error("tool '{tool_name}' is not registered")]
    NotFound { tool_name: String },

    /// The tool was found but failed during execution.
    #[error("tool '{tool_name}' failed: {source}")]
    Execution {
        tool_name: String,
        #[source]
        source: ToolError,
    },
}

// ── ToolExecutor trait ────────────────────────────────────────────────────────

/// The contract for a collection of tools.
///
/// The executor programs against this — not against ToolRegistry directly.
/// This separation means ToolRegistry can live in nanny-tools without
/// nanny-core needing to import it.
///
/// Implementations: ToolRegistry in nanny-tools.
/// In tests: inline NoOpToolExecutor or similar test doubles.
pub trait ToolExecutor {
    /// Execute a tool by name with the given arguments.
    ///
    /// Returns `Err(ToolCallError::NotFound)` if the tool is not registered.
    /// Returns `Err(ToolCallError::Execution)` if the tool fails.
    fn call(&self, name: &str, args: &ToolArgs) -> Result<ToolOutput, ToolCallError>;

    /// Return the declared cost for a named tool, if it exists.
    ///
    /// Used by the executor to charge the ledger after a successful call.
    /// Returns `None` if the tool is not registered.
    fn declared_cost(&self, name: &str) -> Option<u64>;
}
