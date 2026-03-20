use serde::{Deserialize, Serialize};

/// Every possible reason execution was stopped.
///
/// This enum is exhaustive by design. Adding a new stop condition requires
/// adding a variant here first — the compiler will then force every match
/// site to handle it. That is the point.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum StopReason {
    /// The agent reached the maximum number of allowed steps.
    MaxStepsReached,

    /// The agent exhausted its cost budget before completing.
    BudgetExhausted,

    /// The wall-clock timeout expired.
    TimeoutExpired,

    /// The agent attempted to call a tool not on the allowlist.
    /// Carries the name of the denied tool for audit purposes.
    ToolDenied { tool_name: String },

    /// Execution was stopped explicitly by the caller.
    ManualStop,

    /// The agent declared itself complete within the allowed limits.
    /// This is a successful, normal termination — not a constraint violation.
    AgentCompleted,
}

/// The state machine for a single agent execution.
///
/// Transitions are strictly one-way:
///   Initialized → Running → Stopped
///                         → Failed
///
/// Once in a terminal state (Stopped or Failed), execution cannot
/// resume or transition further. Attempting to do so is always a bug.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExecutionState {
    /// Execution has been configured but not yet started.
    Initialized,

    /// Execution is actively running.
    Running,

    /// Execution ended with an explicit, typed reason.
    /// This is the normal terminal state.
    Stopped { reason: StopReason },

    /// Execution ended due to an unrecoverable internal error.
    /// Distinct from Stopped — this is abnormal termination.
    Failed { error: String },
}

impl ExecutionState {
    /// Returns true if this state is terminal.
    ///
    /// A terminal state must never transition to any other state.
    /// If the execution loop checks this and finds true, it must stop immediately.
    pub fn is_terminal(&self) -> bool {
        matches!(self, ExecutionState::Stopped { .. } | ExecutionState::Failed { .. })
    }
}
