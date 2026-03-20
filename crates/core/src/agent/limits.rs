use serde::{Deserialize, Serialize};

/// Hard limits that govern a single execution.
///
/// All fields are required — there are no implicit, invisible limits.
/// Every constraint must be declared explicitly in configuration.
/// `Limits::default()` provides safe values used by `nanny init`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Limits {
    /// Maximum number of steps the agent may execute.
    /// Step N is never started if the counter has already reached max_steps.
    pub max_steps: u32,

    /// Maximum cost units the agent may spend across the entire execution.
    /// In local mode these are abstract units.
    /// In managed mode they map to real currency via the orchestrator ledger.
    pub max_cost_units: u64,

    /// Wall-clock timeout in milliseconds, measured from execution start.
    /// Checked at the entry of every step — not after.
    /// There is no grace period.
    pub timeout_ms: u64,
}

impl Default for Limits {
    /// Safe defaults used when generating a new nanny.toml via `nanny init`.
    /// These values are intentionally conservative — users loosen them explicitly.
    fn default() -> Self {
        Self {
            max_steps: 100,
            max_cost_units: 1_000,
            timeout_ms: 30_000, // 30 seconds
        }
    }
}
