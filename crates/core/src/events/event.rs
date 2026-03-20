use crate::agent::{limits::Limits, state::StopReason};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// A unique identifier for a single agent execution.
pub type ExecutionId = Uuid;

/// Structured, append-only facts emitted during execution.
///
/// Every variant carries an `execution_id` and `timestamp` so events are
/// fully self-contained — they can be read, stored, or replayed without
/// any surrounding context.
///
/// These are facts, not errors, not suggestions, not opinions.
/// The log is append-only. Events are never modified or deleted.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ExecutionEvent {
    /// Emitted exactly once when execution begins.
    /// Records the full limits so any reader can reconstruct enforcement context.
    ExecutionStarted {
        execution_id: ExecutionId,
        limits: Limits,
        #[serde(with = "chrono::serde::ts_milliseconds")]
        timestamp: DateTime<Utc>,
    },

    /// Emitted at the entry of every step, before any work is done.
    StepStarted {
        execution_id: ExecutionId,
        step: u32,
        #[serde(with = "chrono::serde::ts_milliseconds")]
        timestamp: DateTime<Utc>,
    },

    /// Emitted when a step completes without triggering a stop condition.
    StepCompleted {
        execution_id: ExecutionId,
        step: u32,
        #[serde(with = "chrono::serde::ts_milliseconds")]
        timestamp: DateTime<Utc>,
    },

    /// Emitted by the ledger when cost is debited for an action.
    /// Records the amount spent and the balance remaining after the debit.
    CostDebited {
        execution_id: ExecutionId,
        amount: u64,
        balance_remaining: u64,
        #[serde(with = "chrono::serde::ts_milliseconds")]
        timestamp: DateTime<Utc>,
    },

    /// Emitted as the final event when execution stops for any reason.
    ///
    /// This event is always the last one in any complete execution log.
    /// If this event is missing from a log, the process crashed — that
    /// itself is an auditable fact.
    ExecutionStopped {
        execution_id: ExecutionId,
        reason: StopReason,
        total_steps: u32,
        #[serde(with = "chrono::serde::ts_milliseconds")]
        timestamp: DateTime<Utc>,
    },

    /// Emitted when a tool is called and completes successfully.
    ///
    /// Cost is charged after this event is emitted — the charge only
    /// happens when the tool actually did work.
    ToolCalled {
        execution_id: ExecutionId,

        /// The name of the tool that was called.
        tool_name: String,

        /// Cost units charged for this tool call.
        cost: u64,

        #[serde(with = "chrono::serde::ts_milliseconds")]
        timestamp: DateTime<Utc>,
    },

    /// Emitted when a permitted tool fails during execution.
    ///
    /// Distinct from a policy denial — the tool was allowed but encountered
    /// an error (network failure, bad args, timeout).
    /// No cost is charged on tool failure.
    ToolFailed {
        execution_id: ExecutionId,

        /// The name of the tool that failed.
        tool_name: String,

        /// Human-readable description of what went wrong.
        error: String,

        #[serde(with = "chrono::serde::ts_milliseconds")]
        timestamp: DateTime<Utc>,
    },

    /// Emitted when an internal, unrecoverable error terminates execution.
    /// Distinct from ExecutionStopped — this is abnormal termination, not a
    /// policy decision.
    ExecutionFailed {
        execution_id: ExecutionId,
        error: String,
        #[serde(with = "chrono::serde::ts_milliseconds")]
        timestamp: DateTime<Utc>,
    },
}
