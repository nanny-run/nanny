// nanny.toml schema, parsing, and strict validation.
//
// This crate owns one job: turn a static file into a trusted, validated config.
// If the file is missing, malformed, or contains illegal values — we fail immediately.
// No silent defaults. No guessing. No recovery.
//
// TOML field naming vs Rust field naming:
//   TOML uses short human-facing names: steps, cost, timeout
//   Rust uses descriptive names:        max_steps, max_tokens, timeout_ms
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

    /// HTTP CONNECT proxy settings for the network server.
    /// Only active on `nanny run --serve` when `[proxy] allowed_hosts` is set.
    #[serde(default)]
    pub proxy: Option<ProxyConfig>,

    /// Opt-in dollars-denominated budget, resolved into `[limits].tokens` by
    /// the CLI before the engine runs. See `BudgetConfig` — this crate never
    /// resolves it, only parses it.
    #[serde(default)]
    pub budget: Option<BudgetConfig>,
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
/// TOML field names are short: steps, tokens, timeout.
/// Rust field names are descriptive: max_steps, max_tokens, timeout_ms.
///
/// Named limit sets live as subtables: [limits.researcher], [limits.writer], etc.
/// A named set inherits all fields from [limits] and overrides only what it declares.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LimitsConfig {
    /// Maximum number of steps before the agent is stopped.
    /// TOML key: steps
    #[serde(rename = "steps")]
    pub max_steps: u32,

    /// Maximum LLM tokens before the agent is stopped.
    /// Charged per tool call via `tokens_per_call` or measured via `nanny.instrument`.
    /// TOML key: tokens
    #[serde(rename = "tokens")]
    pub max_tokens: u64,

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

    /// Override for max_tokens. If None, inherits from [limits].
    #[serde(rename = "tokens", default)]
    pub max_tokens: Option<u64>,

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

    /// Tokens charged per call to this tool.
    pub tokens_per_call: Option<u64>,
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

// ── ProxyConfig ───────────────────────────────────────────────────────────────

/// HTTP CONNECT proxy configuration for the network server.
///
/// Only active on `nanny run --serve` when `[proxy] allowed_hosts` is set.
/// The proxy forwards HTTPS traffic from agents to allowed hosts,
/// intercepting all outbound HTTP regardless of `#[nanny::tool]` decoration.
///
/// ```toml
/// [proxy]
/// allowed_hosts = ["api.openai.com", "api.groq.com", "*.anthropic.com"]
/// ```
///
/// Proxy is opt-in. If `allowed_hosts` is missing or empty, the proxy is treated as not configured.
/// Configure at least one host to activate it.
/// Loopback (127.x.x.x, ::1) and RFC-1918 private ranges are always
/// blocked in code regardless of this list.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ProxyConfig {
    /// Hostnames the proxy may forward to.
    ///
    /// Supports exact names (`"api.openai.com"`) and glob patterns (`"*.anthropic.com"`).
    /// Loopback and RFC-1918 private ranges are blocked regardless of this list.
    #[serde(default)]
    pub allowed_hosts: Vec<String>,
}

// ── BudgetConfig ─────────────────────────────────────────────────────────────

/// Opt-in, dollars-denominated budget — resolved into `[limits].tokens` (and
/// `[limits.<name>].tokens` for each named override below) by the CLI, once,
/// immediately after this file is loaded and before the engine is ever built.
/// `nanny-core`/`nanny-bridge` never see this section at all, only the final
/// resolved `tokens = N` — the resolution itself (reading a local pricing
/// cache, doing the dollars÷price arithmetic) lives in `crates/cli`, never
/// here. This struct is purely inert: parsed and validated like everything
/// else in this crate, nothing more — no HTTP, no arithmetic, matching this
/// crate's "turn a static file into a trusted config, nothing else" charter.
///
/// Absent by default. When absent, nothing changes — every existing
/// `nanny.toml` (raw `tokens = N`) behaves exactly as it always has.
///
/// ```toml
/// [budget]
/// model   = "deepseek-v4-flash"
/// dollars = 10
///
/// [budget.reviewer]
/// dollars = 3
/// ```
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BudgetConfig {
    /// Which model's pricing to resolve against — must match a model name in
    /// the local pricing cache (`~/.nanny/pricing.json`). Resolution fails
    /// loudly, not silently, if the model isn't cached.
    pub model: String,

    /// The overall dollar budget for the run. Resolved into `[limits].tokens`.
    pub dollars: f64,

    /// Named per-scope dollar overrides. In TOML: [budget.researcher],
    /// [budget.writer], etc. — mirrors `[limits.<name>]`. Each resolves into
    /// the matching `[limits.<name>].tokens`.
    #[serde(flatten, default)]
    pub named: HashMap<String, PartialBudgetConfig>,
}

/// A partial per-scope budget override. `dollars: None` means that named
/// scope has no budget override and keeps whatever `[limits.<name>].tokens`
/// already resolved to (from `[limits]`, or its own hand-written value).
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct PartialBudgetConfig {
    #[serde(default)]
    pub dollars: Option<f64>,
}

// ── Cloud sync ─────────────────────────────────────────────────────────────────

/// Environment variable holding the cloud API key, read by the machine login
/// path (`nanny auth login --token`) for CI and headless boxes that cannot run
/// the browser device flow. A secret must never live in the committable
/// nanny.toml, so it is injected through the environment — matching the bridge's
/// pattern (`NANNY_BRIDGE_CERT`, `NANNY_BRIDGE_KEY`, `NANNY_SESSION_TOKEN`).
pub const API_KEY_ENV: &str = "NANNY_API_KEY";

/// Whether a nanny.toml still carries a `[managed]` section. That block
/// (endpoint / api_key) was retired in favor of `nanny auth login`; it is now
/// ignored, so the CLI warns rather than silently doing nothing. A plain line
/// scan is enough — the section is a top-level `[managed]` or `[managed.*]` table.
pub fn has_managed_section(contents: &str) -> bool {
    contents.lines().any(|line| {
        let t = line.trim_start();
        t.starts_with("[managed]") || t.starts_with("[managed.")
    })
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
        max_tokens: partial.max_tokens.unwrap_or(config.limits.max_tokens),
        timeout_ms: partial.timeout_ms.unwrap_or(config.limits.timeout_ms),
    })
}

/// A fully resolved limit set — no Option fields, no inheritance needed.
/// Returned by `resolve_named_limits`. Safe to hand directly to the runtime.
#[derive(Debug, Clone, PartialEq)]
pub struct ResolvedLimits {
    pub max_steps: u32,
    pub max_tokens: u64,
    pub timeout_ms: u64,
}

// ── Default TOML template ─────────────────────────────────────────────────────

/// The canonical starter nanny.toml written by `nanny init`.
///
/// This is a static string — not generated from structs — so the comments
/// and formatting are preserved exactly as the user will see them.
pub fn default_toml() -> &'static str {
    r#"# Generated by `nanny init`. Edit to match your agent's requirements.
# Full reference: https://docs.nanny.run/v0.4/reference/nanny-toml

[runtime]
# Execution mode: "local" (default) or "managed".
#   local   — all enforcement happens on your machine, no network calls.
#   managed — enforcement is still fully local, but if your machine is logged in
#             (run `nanny auth login`) a copy of the event log is synced to Nanny
#             Cloud. The cloud never gates a stop; it only adds dashboards and
#             history. Skip a single run's sync with `nanny run --no-sync`.
mode = "local"

[start]
# How to launch your agent. `nanny run` always reads this command.
# Replace with however you normally start your agent:
#   Python:  cmd = "python agent.py"
#   Rust:    cmd = "cargo run"
#   Node:    cmd = "node agent.js"
cmd = "python agent.py"

[limits]
# Hard ceilings applied to every run. When any limit is crossed, nanny stops
# the agent immediately and emits a structured ExecutionStopped event.

# Maximum number of tool calls before the agent is stopped.
steps = 100

# Maximum LLM tokens before the agent is stopped.
# Tokens are charged per tool call via @tool(tokens=N) or #[tool(tokens = N)],
# or measured automatically via nanny.instrument(client).
tokens = 50000

# Wall-clock timeout in milliseconds.
timeout = 30000

# Named limit sets inherit from [limits] and override only what they declare.
# Useful for giving specific roles or workloads their own ceilings.
# Activate with: nanny run --limits=researcher
# [limits.researcher]
# steps   = 500
# tokens  = 200000
# timeout = 600000

# Prefer thinking in dollars instead of hand-computing tokens? Uncomment
# [budget] — it resolves into the tokens value above automatically, using a
# local pricing cache refreshed by `nanny init` and friends (no cloud account
# needed). Leave [budget] out entirely to keep authoring tokens directly.
# [budget]
# model   = "deepseek-v4-flash"
# dollars = 10
#
# [budget.researcher]
# dollars = 3

[tools]
# Explicit allowlist of tools the agent is permitted to call.
# Any tool not listed here causes an immediate ToolDenied stop.
# An empty list denies all tools. Names must match the function decorated
# with @tool (Python) or #[tool] (Rust).
# http_get is a built-in Rust SDK tool. Replace or extend with your own names.
allowed = ["http_get"]

# Per-tool limits. Override token cost and max calls per tool.
# Keys must match an entry in the allowed list above.
#
# [tools.http_get]
# max_calls      = 10
# tokens_per_call = 200

[observability]
# Where to write the structured NDJSON event log.
# "stdout" — stream events to the terminal in real time (default).
# "file"   — write events to log_file instead.
log = "stdout"

[proxy]
# HTTP CONNECT proxy allowlist for the network governance server.
# Uncomment and add hosts to activate the proxy:
# allowed_hosts = ["api.openai.com", "api.groq.com"]

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
tokens  = 50000
timeout = 30000

[limits.researcher]
steps   = 500
tokens  = 200000
timeout = 600000

[limits.writer]
tokens = 80000

[tools]
allowed = ["http_get", "send_email"]

[tools.http_get]
max_calls       = 10
tokens_per_call = 200

[tools.send_email]
max_calls       = 2
tokens_per_call = 500

[observability]
log = "stdout"
"#
    }

    #[test]
    fn default_toml_is_valid() {
        let config: NannyConfig =
            toml::from_str(default_toml()).expect("default_toml() must always be valid TOML");

        assert_eq!(config.limits.max_steps, 100);
        assert_eq!(config.limits.max_tokens, 50000);
        assert_eq!(config.limits.timeout_ms, 30000);
        assert_eq!(config.runtime.mode, Mode::Local);
        assert_eq!(config.tools.allowed, vec!["http_get"]);
        assert_eq!(config.observability.log, LogTarget::Stdout);
        assert!(
            config.proxy.is_some(),
            "default_toml() must include a [proxy] section as a discoverable template"
        );
        assert!(
            config.proxy.unwrap().allowed_hosts.is_empty(),
            "default proxy allowlist must default to empty (opt-in)"
        );
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
tokens  = 5000
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
        assert_eq!(r.max_tokens, Some(200_000));
        assert_eq!(r.timeout_ms, Some(600_000));
    }

    #[test]
    fn named_limits_partial_override() {
        // [limits.writer] only overrides cost — steps and timeout should be None
        let config: NannyConfig = toml::from_str(full_config_toml()).expect("must parse");

        let writer = &config.limits.named["writer"];
        assert_eq!(writer.max_tokens, Some(80_000));
        assert_eq!(writer.max_steps, None, "writer does not override steps");
        assert_eq!(writer.timeout_ms, None, "writer does not override timeout");
    }

    #[test]
    fn resolve_named_limits_inherits_correctly() {
        let config: NannyConfig = toml::from_str(full_config_toml()).expect("must parse");

        // researcher overrides all three
        let r = resolve_named_limits(&config, "researcher").expect("must resolve");
        assert_eq!(r.max_steps, 500);
        assert_eq!(r.max_tokens, 200_000);
        assert_eq!(r.timeout_ms, 600_000);

        // writer only overrides tokens — steps and timeout inherit from [limits]
        let w = resolve_named_limits(&config, "writer").expect("must resolve");
        assert_eq!(w.max_steps, 100, "inherits from [limits]");
        assert_eq!(w.max_tokens, 80_000, "overridden by [limits.writer]");
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
        assert_eq!(http.tokens_per_call, Some(200));

        let email = config.tools.per_tool.get("send_email").expect("send_email must be present");
        assert_eq!(email.max_calls, Some(2));
        assert_eq!(email.tokens_per_call, Some(500));
    }

    #[test]
    fn observability_defaults_to_stdout() {
        let config: NannyConfig = toml::from_str(
            r#"
[limits]
steps   = 10
tokens  = 5000
timeout = 5000
"#,
        )
        .expect("must parse");

        assert_eq!(config.observability.log, LogTarget::Stdout);
        assert!(config.observability.log_file.is_none());
    }

    #[test]
    fn start_section_is_parsed() {
        let config: NannyConfig = toml::from_str(
            r#"
[start]
cmd = "cargo run --release"

[limits]
steps   = 10
tokens  = 5000
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
tokens  = 5000
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
    fn proxy_config_is_optional() {
        let config: NannyConfig = toml::from_str(
            r#"
[limits]
steps   = 10
tokens  = 5000
timeout = 5000
"#,
        )
        .expect("must parse");

        assert!(config.proxy.is_none());
    }

    #[test]
    fn proxy_config_parses_allowed_hosts() {
        let config: NannyConfig = toml::from_str(
            r#"
[limits]
steps   = 10
tokens  = 5000
timeout = 5000

[proxy]
allowed_hosts = ["api.openai.com", "*.anthropic.com"]
"#,
        )
        .expect("must parse");

        let proxy = config.proxy.expect("[proxy] must be present");
        assert_eq!(proxy.allowed_hosts, vec!["api.openai.com", "*.anthropic.com"]);
    }

    #[test]
    fn proxy_config_empty_allowed_hosts_parses() {
        // Empty list is valid TOML — startup validates it at runtime, not parse time.
        let config: NannyConfig = toml::from_str(
            r#"
[limits]
steps   = 10
tokens  = 5000
timeout = 5000

[proxy]
allowed_hosts = []
"#,
        )
        .expect("must parse — empty allowed_hosts is rejected at server startup, not config parse");

        let proxy = config.proxy.expect("[proxy] must be present");
        assert!(proxy.allowed_hosts.is_empty());
    }

    #[test]
    fn proxy_config_without_allowed_hosts_defaults_to_empty() {
        // [proxy] section present but allowed_hosts omitted → defaults to empty vec.
        let config: NannyConfig = toml::from_str(
            r#"
[limits]
steps   = 10
tokens  = 5000
timeout = 5000

[proxy]
"#,
        )
        .expect("must parse");

        let proxy = config.proxy.expect("[proxy] must be present");
        assert!(proxy.allowed_hosts.is_empty());
    }

    #[test]
    fn budget_config_is_optional() {
        // Absent [budget] must change nothing — full backward compatibility
        // for every existing nanny.toml authored with raw tokens = N.
        let config: NannyConfig = toml::from_str(
            r#"
[limits]
steps   = 10
tokens  = 5000
timeout = 5000
"#,
        )
        .expect("must parse");

        assert!(config.budget.is_none());
    }

    #[test]
    fn budget_config_parses_dollars_and_named_overrides() {
        let config: NannyConfig = toml::from_str(
            r#"
[limits]
steps   = 10
tokens  = 5000
timeout = 5000

[budget]
model   = "deepseek-v4-flash"
dollars = 10

[budget.reviewer]
dollars = 3
"#,
        )
        .expect("must parse");

        let budget = config.budget.expect("[budget] must be present");
        assert_eq!(budget.model, "deepseek-v4-flash");
        assert_eq!(budget.dollars, 10.0);

        let reviewer = budget.named.get("reviewer").expect("[budget.reviewer] must be present");
        assert_eq!(reviewer.dollars, Some(3.0));
    }

    #[test]
    fn budget_config_named_override_without_dollars_is_none() {
        // A named [budget.<name>] table with no dollars key is valid TOML —
        // it just means that scope has no override, same spirit as
        // PartialLimitsConfig's optional fields.
        let config: NannyConfig = toml::from_str(
            r#"
[limits]
steps   = 10
tokens  = 5000
timeout = 5000

[budget]
model   = "deepseek-v4-flash"
dollars = 10

[budget.scout]
"#,
        )
        .expect("must parse");

        let budget = config.budget.expect("[budget] must be present");
        let scout = budget.named.get("scout").expect("[budget.scout] must be present");
        assert_eq!(scout.dollars, None);
    }

    #[test]
    fn managed_mode_parses_without_a_managed_section() {
        // The [managed] block is retired; `mode = "managed"` alone is valid — the
        // key now comes from `nanny auth login`, not the file.
        let config: NannyConfig = toml::from_str(
            r#"
[runtime]
mode = "managed"

[limits]
steps   = 10
tokens  = 5000
timeout = 5000
"#,
        )
        .expect("must parse");
        assert_eq!(config.runtime.mode, Mode::Managed);
    }

    #[test]
    fn legacy_managed_section_is_ignored_but_detectable() {
        // A stale [managed] block no longer breaks parsing (serde ignores unknown
        // keys), but has_managed_section flags it so the CLI can warn.
        let contents = r#"
[runtime]
mode = "managed"

[limits]
steps   = 10
tokens  = 5000
timeout = 5000

[managed]
endpoint = "https://api.nanny.run/v1"
"#;
        let config: NannyConfig =
            toml::from_str(contents).expect("legacy [managed] must not break parsing");
        assert_eq!(config.runtime.mode, Mode::Managed);
        assert!(has_managed_section(contents), "the stale section must be detectable");
    }

    #[test]
    fn has_managed_section_matches_only_the_managed_table() {
        assert!(has_managed_section("[managed]\nendpoint = \"x\""));
        assert!(has_managed_section("  [managed.sub]\n"));
        assert!(!has_managed_section("[limits]\nsteps = 1"));
        assert!(!has_managed_section("# [managed] just a comment"), "a comment is not a section");
    }
}
