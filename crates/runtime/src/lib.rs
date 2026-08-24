// nanny-runtime — local implementations of the nanny-core contracts.
//
// nanny-core defines the contracts (Policy, Ledger, Tool traits).
// nanny-runtime provides the concrete implementations used in local mode.
//
// Three implementation families live here:
//   enforcement  — ToolPermissionPolicy, LimitsPolicy, RuleEvaluator, ChainPolicy
//   ledger       — FakeLedger
//   tools        — ToolRegistry, HttpGet, default_registry

pub mod enforcement;
pub mod ledger;
pub mod tools;

pub use enforcement::{ChainPolicy, LimitsPolicy, RuleEvaluator, ToolPermissionPolicy};
pub use ledger::FakeLedger;
pub use tools::{default_registry, HttpGet, ToolRegistry};
