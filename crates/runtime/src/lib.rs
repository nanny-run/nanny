// nanny-runtime — local implementations of the nanny-core contracts.
//
// nanny-core defines the contracts (Policy, Tool traits).
// nanny-runtime provides the concrete implementations used in local mode.
//
// Three implementation families live here:
//   enforcement  — ToolPermissionPolicy, RuleEvaluator, ChainPolicy
//   tools        — ToolRegistry, HttpGet, default_registry

pub mod enforcement;
pub mod tools;

pub use enforcement::{ChainPolicy, RuleEvaluator, ToolPermissionPolicy};
pub use tools::{default_registry, HttpGet, ToolRegistry};
