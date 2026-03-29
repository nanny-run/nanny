// nanny.toml schema, parsing, and strict validation.
//
// This crate owns one job: turn a static file into a trusted, validated config.
// If the file is missing, malformed, or contains illegal values — we fail immediately.
// No silent defaults. No guessing. No recovery.
//
// TOML field naming vs Rust field naming:
//   TOML uses short human-facing names: steps, cost, timeout
//   Rust uses descriptive names:        max_steps, max_cost_units, timeout_ms
//   The gap is bridged by #[serde(rename = "...")] on each field.
//   This means the Rust code is clear, and the config file is concise.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::Path;
use thiserror::Error;

// ── Error type ────────────────────────────────────────────────────────────────

/// Every way config loading can fail. All failures are final — there is no fallback.
#[derive(Debug, Error)]
pub enum ConfigError {
    #[error("config file not found at '{path}' — run `nanny init` to create one")]
    NotFound { path: String },

    #[error("could not read config file: {0}")]
    Io(#[from] std::io::Error),

    #[error("invalid config: {0}")]
    Parse(String),

    #[error("named limits '{name}' not found in config — available: {available:?}")]
    NamedLimitsNotFound { name: String, available: Vec<String> },
}

// ── Top-level config ──────────────────────────────────────────────────────────

/// The full contents of a nanny.toml file.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NannyConfig {
    /// Runtime mode and execution settings.
    #[serde(default)]
    pub runtime: RuntimeConfig,

    /// How to launch the project. `nanny run` always reads this — extra args
    /// passed after `--` are appended to `cmd`.
    #[serde(default)]
    pub start: Option<StartConfig>,

    /// Hard limits that govern every execution under this config.
    pub limits: LimitsConfig,

    /// Tool permission policy.
    #[serde(default)]
    pub tools: ToolsConfig,

    /// Event log output settings.
    #[serde(default)]
    pub observability: ObservabilityConfig,

    /// Cloud orchestrator connection. Only read when runtime.mode = "managed".
    #[serde(default)]
    pub managed: Option<ManagedConfig>,
}

// ── RuntimeConfig ─────────────────────────────────────────────────────────────

/// Top-level runtime settings. Controls execution mode.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct RuntimeConfig {
    /// Whether the runtime operates standalone or reports to an orchestrator.
    /// "local" (default) or "managed".
    #[serde(default)]
    pub mode: Mode,
}

// ── Mode ──────────────────────────────────────────────────────────────────────

/// Whether the runtime operates standalone or reports facts to a hosted orchestrator.
#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum Mode {
    /// Local-only. No network calls. No external dependencies. Default.
    #[default]
    Local,

    /// Managed mode. Runtime still enforces locally but sends facts to the orchestrator.
    Managed,
}

// ── StartConfig ───────────────────────────────────────────────────────────────

/// Project start configuration — how to launch the agent under nanny enforcement.
///
/// ```toml
/// [start]
/// cmd = "python agent.py"
/// ```
///
/// `nanny run` always reads `cmd`, splits it by whitespace, then appends any
/// extra args passed after `--`. There is no inline command form.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StartConfig {
    /// The command to run. Split by whitespace into program + args.
    /// Example: "cargo run --release" → ["cargo", "run", "--release"]
    pub cmd: String,
}

// ── LimitsConfig ─────────────────────────────────────────────────────────────

/// Global execution limits — applied to all runs unless a named set is selected.
///
/// TOML field names are short: steps, cost, timeout.
/// Rust field names are descriptive: max_steps, max_cost_units, timeout_ms.
///
/// Named limit sets live as subtables: [limits.researcher], [limits.writer], etc.
/// A named set inherits all fields from [limits] and overrides only what it declares.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LimitsConfig {
    /// Maximum number of steps before the agent is stopped.
    /// TOML key: steps
    #[serde(rename = "steps")]
    pub max_steps: u32,

    /// Maximum cost units before the agent is stopped.
    /// Abstract in local mode. Maps to real currency in managed mode.
    /// TOML key: cost
    #[serde(rename = "cost")]
    pub max_cost_units: u64,

    /// Wall-clock timeout in milliseconds.
    /// TOML key: timeout
    #[serde(rename = "timeout")]
    pub timeout_ms: u64,

    /// Named limit sets. Each key is a set name (e.g., "researcher").
    /// Each value overrides only the fields it declares — rest inherit from [limits].
    /// In TOML these appear as [limits.researcher], [limits.writer], etc.
    #[serde(flatten, default)]
    pub named: HashMap<String, PartialLimitsConfig>,
}

/// A partial limit set used in named overrides.
/// All fields are optional — only declared fields override the parent [limits] defaults.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct PartialLimitsConfig {
    /// Override for max_steps. If None, inherits from [limits].
    #[serde(rename = "steps", default)]
    pub max_steps: Option<u32>,

    /// Override for max_cost_units. If None, inherits from [limits].
    #[serde(rename = "cost", default)]
    pub max_cost_units: Option<u64>,

    /// Override for timeout_ms. If None, inherits from [limits].
    #[serde(rename = "timeout", default)]
    pub timeout_ms: Option<u64>,
}

// ── ToolsConfig ───────────────────────────────────────────────────────────────

/// Tool permission and per-tool configuration.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ToolsConfig {
    /// Explicit allowlist of permitted tool names.
    /// Any tool not listed here causes an immediate hard stop.
    #[serde(default)]
    pub allowed: Vec<String>,

    /// Per-tool configuration. Keys are tool names (e.g., "http_get").
    /// In TOML these appear as [tools.http_get], [tools.send_email], etc.
    #[serde(flatten, default)]
    pub per_tool: HashMap<String, ToolConfig>,
}

/// Per-tool execution limits.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ToolConfig {
    /// Maximum number of times this tool may be called in one execution.
    pub max_calls: Option<u32>,

    /// Cost units charged per call to this tool.
    pub cost_per_call: Option<u64>,
}

// ── ObservabilityConfig ───────────────────────────────────────────────────────

/// Controls where the structured event log is written.
///
/// The event log is ephemeral in v0.1.0 — it lives only as long as the process.
/// Pipe stdout to your own storage if persistence is required.
/// Phase 2 cloud ingests this log and makes it durable and queryable.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ObservabilityConfig {
    /// Where to write the NDJSON event log.
    #[serde(default)]
    pub log: LogTarget,

    /// Log file path. Only used when log = "file".
    pub log_file: Option<std::path::PathBuf>,
}

impl Default for ObservabilityConfig {
    fn default() -> Self {
        Self {
            log: LogTarget::Stdout,
            log_file: None,
        }
    }
}

/// Where the event log is written.
#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum LogTarget {
    /// Write events to stdout as NDJSON. Default.
    #[default]
    Stdout,

    /// Write events to the file specified in log_file.
    File,
}

// ── ManagedConfig ─────────────────────────────────────────────────────────────

/// Cloud orchestrator connection settings.
///
/// Only active when [runtime] mode = "managed".
/// This section is config, not a switch — the switch is runtime.mode.
#[derive(Clone, Serialize, Deserialize)]
pub struct ManagedConfig {
    /// Cloud API endpoint.
    pub endpoint: String,

    /// Your organization ID.
    pub org_id: String,

    /// Your API key. Keep this out of version control — never log or print it.
    ///
    /// Intentionally excluded from serialization so a round-trip through
    /// `serde_json` / `toml` does not accidentally re-emit the key.
    #[serde(skip_serializing)]
    pub api_key: String,
}

/// Redacts `api_key` so it never appears in logs, panic messages, or test output.
impl std::fmt::Debug for ManagedConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ManagedConfig")
            .field("endpoint", &self.endpoint)
            .field("org_id",   &self.org_id)
            .field("api_key",  &"[redacted]")
            .finish()
    }
}

// ── Load ──────────────────────────────────────────────────────────────────────

/// Load and parse a nanny.toml from disk.
///
/// Fails immediately if:
/// - The file does not exist
/// - The file cannot be read
/// - The TOML is malformed
/// - Required fields are missing
///
/// There is no fallback. No defaults are applied for missing required fields.
pub fn load(path: &Path) -> Result<NannyConfig, ConfigError> {
    if !path.exists() {
        return Err(ConfigError::NotFound {
            path: path.display().to_string(),
        });
    }

    let contents = std::fs::read_to_string(path)?;

    toml::from_str(&contents).map_err(|e| {
        let msg = e.to_string();
        // Surface actionable hints for the most common config mistakes.
        let hint = if msg.contains("missing field `cmd`") {
            " — add `cmd = \"<your command>\"` under [start]"
        } else if msg.contains("missing field") && msg.contains("start") {
            " — add a [start] section with `cmd = \"<your command>\"`"
        } else {
            ""
        };
        ConfigError::Parse(format!("{msg}{hint}"))
    })
}

// ── Named limits resolution ───────────────────────────────────────────────────

/// Resolve a named limit set from config, inheriting from [limits] defaults.
///
/// Returns `Err(ConfigError::NamedLimitsNotFound)` if the name does not exist.
/// Returns the fully resolved limits with inheritance applied.
pub fn resolve_named_limits(
    config: &NannyConfig,
    name: &str,
) -> Result<ResolvedLimits, ConfigError> {
    let partial = config.limits.named.get(name).ok_or_else(|| {
        let available: Vec<String> = config.limits.named.keys().cloned().collect();
        ConfigError::NamedLimitsNotFound {
            name: name.to_string(),
            available,
        }
    })?;

    Ok(ResolvedLimits {
        max_steps: partial.max_steps.unwrap_or(config.limits.max_steps),
        max_cost_units: partial.max_cost_units.unwrap_or(config.limits.max_cost_units),
        timeout_ms: partial.timeout_ms.unwrap_or(config.limits.timeout_ms),
    })
}

/// A fully resolved limit set — no Option fields, no inheritance needed.
/// Returned by `resolve_named_limits`. Safe to hand directly to the runtime.
#[derive(Debug, Clone, PartialEq)]
pub struct ResolvedLimits {
    pub max_steps: u32,
    pub max_cost_units: u64,
    pub timeout_ms: u64,
}

// ── Default TOML template ─────────────────────────────────────────────────────

/// The canonical starter nanny.toml written by `nanny init`.
///
/// This is a static string — not generated from structs — so the comments
/// and formatting are preserved exactly as the user will see them.
pub fn default_toml() -> &'static str {
    r#"# nanny.toml — Execution boundary configuration
# Generated by `nanny init`. Edit to match your agent's requirements.

[runtime]
# Execution mode: "local" (default) or "managed" (requires [managed] config).
# In local mode, all enforcement happens on this machine with no network calls.
mode = "local"

# Cloud orchestrator config — only read when mode = "managed".
#
# [managed]
# endpoint = "https://api.nanny.run"
# org_id   = "org_123"
# api_key  = "nny_live_xxx"

[start]
# How to launch your agent. nanny run reads this command.
cmd = "python agent.py"

[limits]
# Maximum number of steps before the agent is stopped.
steps = 100

# Maximum cost units before the agent is stopped.
# In local mode these are abstract units you define.
# In managed mode they map to real currency via your orchestrator config.
cost = 1000

# Wall-clock timeout in milliseconds. 30000 = 30 seconds.
timeout = 30000

# Named limit sets inherit from [limits] and override only what they declare.
# Activate with: nanny run --limits=researcher
#
# [limits.researcher]
# steps   = 500
# cost    = 5000
# timeout = 600000

[tools]
# Tools the agent is permitted to call.
# Any tool not listed here causes an immediate hard stop.
allowed = ["http_get"]

# Per-tool limits. Override cost and call count per tool.
#
# [tools.http_get]
# max_calls     = 10
# cost_per_call = 10

[observability]
# Where to write the structured NDJSON event log.
# "stdout" (default) or "file"
log = "stdout"

# Uncomment to write events to a file instead:
# log      = "file"
# log_file = "nanny.log"
"#
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn full_config_toml() -> &'static str {
        r#"
[runtime]
mode = "local"

[limits]
steps   = 100
cost    = 1000
timeout = 30000

[limits.researcher]
steps   = 500
cost    = 5000
timeout = 600000

[limits.writer]
cost = 2000

[tools]
allowed = ["http_get", "send_email"]

[tools.http_get]
max_calls     = 10
cost_per_call = 10

[tools.send_email]
max_calls     = 2
cost_per_call = 50

[observability]
log = "stdout"
"#
    }

    #[test]
    fn default_toml_is_valid() {
        let config: NannyConfig =
            toml::from_str(default_toml()).expect("default_toml() must always be valid TOML");

        assert_eq!(config.limits.max_steps, 100);
        assert_eq!(config.limits.max_cost_units, 1000);
        assert_eq!(config.limits.timeout_ms, 30000);
        assert_eq!(config.runtime.mode, Mode::Local);
        assert_eq!(config.tools.allowed, vec!["http_get"]);
        assert_eq!(config.observability.log, LogTarget::Stdout);
    }

    #[test]
    fn missing_limits_is_rejected() {
        let bad = r#"
[runtime]
mode = "local"
"#;
        assert!(
            toml::from_str::<NannyConfig>(bad).is_err(),
            "config without [limits] must be rejected"
        );
    }

    #[test]
    fn unknown_mode_is_rejected() {
        let bad = r#"
[runtime]
mode = "cloud"

[limits]
steps   = 10
cost    = 100
timeout = 5000
"#;
        assert!(
            toml::from_str::<NannyConfig>(bad).is_err(),
            "unknown mode must be rejected"
        );
    }

    #[test]
    fn named_limits_are_parsed() {
        let config: NannyConfig = toml::from_str(full_config_toml()).expect("must parse");

        assert!(
            config.limits.named.contains_key("researcher"),
            "researcher limits must be parsed"
        );
        let r = &config.limits.named["researcher"];
        assert_eq!(r.max_steps, Some(500));
        assert_eq!(r.max_cost_units, Some(5000));
        assert_eq!(r.timeout_ms, Some(600_000));
    }

    #[test]
    fn named_limits_partial_override() {
        // [limits.writer] only overrides cost — steps and timeout should be None
        let config: NannyConfig = toml::from_str(full_config_toml()).expect("must parse");

        let writer = &config.limits.named["writer"];
        assert_eq!(writer.max_cost_units, Some(2000));
        assert_eq!(writer.max_steps, None, "writer does not override steps");
        assert_eq!(writer.timeout_ms, None, "writer does not override timeout");
    }

    #[test]
    fn resolve_named_limits_inherits_correctly() {
        let config: NannyConfig = toml::from_str(full_config_toml()).expect("must parse");

        // researcher overrides all three
        let r = resolve_named_limits(&config, "researcher").expect("must resolve");
        assert_eq!(r.max_steps, 500);
        assert_eq!(r.max_cost_units, 5000);
        assert_eq!(r.timeout_ms, 600_000);

        // writer only overrides cost — steps and timeout inherit from [limits]
        let w = resolve_named_limits(&config, "writer").expect("must resolve");
        assert_eq!(w.max_steps, 100, "inherits from [limits]");
        assert_eq!(w.max_cost_units, 2000, "overridden by [limits.writer]");
        assert_eq!(w.timeout_ms, 30000, "inherits from [limits]");
    }

    #[test]
    fn resolve_named_limits_not_found_errors() {
        let config: NannyConfig = toml::from_str(full_config_toml()).expect("must parse");

        let result = resolve_named_limits(&config, "nonexistent");
        assert!(
            matches!(result, Err(ConfigError::NamedLimitsNotFound { .. })),
            "missing named set must return NamedLimitsNotFound"
        );
    }

    #[test]
    fn per_tool_config_is_parsed() {
        let config: NannyConfig = toml::from_str(full_config_toml()).expect("must parse");

        let http = config.tools.per_tool.get("http_get").expect("http_get must be present");
        assert_eq!(http.max_calls, Some(10));
        assert_eq!(http.cost_per_call, Some(10));

        let email = config.tools.per_tool.get("send_email").expect("send_email must be present");
        assert_eq!(email.max_calls, Some(2));
        assert_eq!(email.cost_per_call, Some(50));
    }

    #[test]
    fn observability_defaults_to_stdout() {
        let config: NannyConfig = toml::from_str(
            r#"
[limits]
steps   = 10
cost    = 100
timeout = 5000
"#,
        )
        .expect("must parse");

        assert_eq!(config.observability.log, LogTarget::Stdout);
        assert!(config.observability.log_file.is_none());
    }

    #[test]
    fn managed_section_is_optional() {
        let config: NannyConfig = toml::from_str(
            r#"
[limits]
steps   = 10
cost    = 100
timeout = 5000
"#,
        )
        .expect("must parse");

        assert!(config.managed.is_none());
    }

    #[test]
    fn start_section_is_parsed() {
        let config: NannyConfig = toml::from_str(
            r#"
[start]
cmd = "cargo run --release"

[limits]
steps   = 10
cost    = 100
timeout = 5000
"#,
        )
        .expect("must parse");

        let start = config.start.expect("[start] must be present");
        assert_eq!(start.cmd, "cargo run --release");
    }

    #[test]
    fn start_section_is_optional() {
        let config: NannyConfig = toml::from_str(
            r#"
[limits]
steps   = 10
cost    = 100
timeout = 5000
"#,
        )
        .expect("must parse — [start] is optional");

        assert!(config.start.is_none());
    }

    #[test]
    fn default_toml_includes_start_section() {
        let config: NannyConfig =
            toml::from_str(default_toml()).expect("default_toml() must always be valid TOML");

        let start = config.start.expect("default_toml() must include [start]");
        assert_eq!(start.cmd, "python agent.py");
    }

    #[test]
    fn managed_section_parses_when_present() {
        let config: NannyConfig = toml::from_str(
            r#"
[runtime]
mode = "managed"

[limits]
steps   = 10
cost    = 100
timeout = 5000

[managed]
endpoint = "https://api.nanny.run"
org_id   = "org_123"
api_key  = "nny_live_xxx"
"#,
        )
        .expect("must parse");

        assert_eq!(config.runtime.mode, Mode::Managed);
        let m = config.managed.expect("managed section must be present");
        assert_eq!(m.endpoint, "https://api.nanny.run");
        assert_eq!(m.org_id, "org_123");
        assert_eq!(m.api_key, "nny_live_xxx");
    }
}
