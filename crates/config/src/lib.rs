// nanny.toml schema, parsing, and strict validation.
//
// This crate owns one job: turn a static file into a trusted, validated config.
// If the file is missing, malformed, or contains illegal values — we fail immediately.
// No silent defaults. No guessing. No recovery.
//
// TOML field naming vs Rust field naming:
//   The gap is bridged by #[serde(rename = "...")] on each field.
//   This means the Rust code is clear, and the config file is concise.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
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
}

// ── Top-level config ──────────────────────────────────────────────────────────

/// The full contents of a nanny.toml file.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NannyConfig {
    /// How to launch the project. `nanny run` always reads this — extra args
    /// passed after `--` are appended to `cmd`.
    #[serde(default)]
    pub start: Option<StartConfig>,

    /// Tool permission policy.
    #[serde(default)]
    pub tools: ToolsConfig,

    /// Event log output settings.
    #[serde(default)]
    pub observability: ObservabilityConfig,
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

/// Per-tool configuration.
///
/// Beyond `max_calls`, this is where the operator declares what a tool *is*.
/// Rules reference these labels rather than tool names, which is what lets a
/// rule written elsewhere govern an app whose tools it has never heard of:
/// the rule holds the logic, the config holds the facts about this app.
///
/// `deny_unknown_fields` is load-bearing, not tidiness: a misspelled label
/// would otherwise parse silently and the operator would believe they had
/// declared a control they do not have. Fail closed on the typo instead.
#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct ToolConfig {
    /// Maximum number of times this tool may be called in one execution.
    pub max_calls: Option<u32>,

    /// This tool ingests content the operator does not control.
    #[serde(default)]
    pub reads_untrusted: bool,

    /// This tool acts on the outside world.
    #[serde(default)]
    pub external_effect: bool,

    /// This tool's effect is irreversible.
    #[serde(default)]
    pub destructive: bool,

    /// This tool moves money.
    #[serde(default)]
    pub moves_money: bool,

    /// This tool touches secrets or personal data.
    #[serde(default)]
    pub reads_sensitive: bool,
}

/// The classification labels an operator may declare on a tool.
///
/// A closed set on purpose: rules are written against these five names, so
/// adding a sixth is a deliberate, versioned decision rather than something
/// a config file can invent.
pub const TOOL_LABELS: [&str; 5] = [
    "reads_untrusted",
    "external_effect",
    "destructive",
    "moves_money",
    "reads_sensitive",
];

impl ToolConfig {
    /// The labels declared on this tool, in [`TOOL_LABELS`] order.
    ///
    /// Order is fixed rather than incidental so the audit log and `/status`
    /// stay byte-comparable across runs.
    pub fn labels(&self) -> Vec<&'static str> {
        let set = [
            (self.reads_untrusted, "reads_untrusted"),
            (self.external_effect, "external_effect"),
            (self.destructive,     "destructive"),
            (self.moves_money,     "moves_money"),
            (self.reads_sensitive, "reads_sensitive"),
        ];
        set.iter().filter(|(on, _)| *on).map(|(_, name)| *name).collect()
    }
}

// ── ObservabilityConfig ───────────────────────────────────────────────────────

/// Controls where the structured event log is written.
///
/// Pipe stdout to your own storage if persistence is required in "stdout" mode.
/// Phase 2 cloud ingests this log and makes it durable and queryable — and can
/// backfill from the local file below even for a run that happened before this
/// machine was ever logged in, since the file always lives in the same place.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ObservabilityConfig {
    /// Where to write the NDJSON event log.
    #[serde(default)]
    pub log: LogTarget,

    /// Optional override for the log's name when log = "file". Defaults to
    /// "log" if not set. A bare name only — no extension, no path
    /// separators: Nanny always appends `.ndjson` and always owns the
    /// directory (`.nanny/logs/`, created automatically). See
    /// `resolve_log_path`.
    pub file: Option<String>,
}

impl Default for ObservabilityConfig {
    fn default() -> Self {
        Self {
            log: LogTarget::Stdout,
            file: None,
        }
    }
}

impl ObservabilityConfig {
    /// The name used when `file` is not set, before `.ndjson` is appended.
    pub const DEFAULT_NAME: &'static str = "log";

    /// Resolve this config into an actual log file path, anchored under
    /// `base_dir` (the directory nanny.toml lives in). Returns `None` for
    /// `LogTarget::Stdout`.
    ///
    /// The directory is always `base_dir/.nanny/logs/` — never configurable,
    /// created here if it doesn't exist yet, exactly like `.nanny/servers/`
    /// already auto-creates itself for governor state. `file`, if set, must
    /// be a bare name: no path separators (the directory isn't the
    /// developer's to choose) and no `.` (the `.ndjson` extension is always
    /// appended by Nanny, never spelled out in config).
    pub fn resolve_log_path(&self, base_dir: &Path) -> Result<Option<PathBuf>, ConfigError> {
        match self.log {
            LogTarget::Stdout => Ok(None),
            LogTarget::File => {
                let name = self.file.as_deref().unwrap_or(Self::DEFAULT_NAME);
                if name.contains('/') || name.contains('\\') {
                    return Err(ConfigError::Parse(format!(
                        "observability.file = '{name}' must be a bare name, not a path — \
                         the directory is always .nanny/logs/, owned by nanny"
                    )));
                }
                if name.contains('.') {
                    return Err(ConfigError::Parse(format!(
                        "observability.file = '{name}' must not include an extension — \
                         nanny always appends .ndjson, e.g. file = \"events\""
                    )));
                }
                let dir = base_dir.join(".nanny").join("logs");
                std::fs::create_dir_all(&dir)?;
                ensure_logs_gitignored(base_dir);
                Ok(Some(dir.join(format!("{name}.ndjson"))))
            }
        }
    }
}

/// Best-effort: append `.nanny/logs/` to `.gitignore` if it isn't already
/// covered. Never fails the caller — a missed gitignore entry is a nudge,
/// not a hard requirement. These are audit-trail logs, not source: they
/// belong on disk (where they are also the durable buffer that lets a run
/// back-sync history to Cloud after an outage) but never in git.
fn ensure_logs_gitignored(base_dir: &Path) {
    const GITIGNORE_LINE: &str = ".nanny/logs/";
    let path = base_dir.join(".gitignore");
    let existing = std::fs::read_to_string(&path).unwrap_or_default();
    let already_covered = existing.lines().any(|l| {
        let t = l.trim();
        t == GITIGNORE_LINE || t == ".nanny/logs" || t == ".nanny/" || t == ".nanny"
    });
    if already_covered {
        return;
    }
    let mut updated = existing;
    if !updated.is_empty() && !updated.ends_with('\n') {
        updated.push('\n');
    }
    updated.push_str(GITIGNORE_LINE);
    updated.push('\n');
    let _ = std::fs::write(&path, updated);
}

/// Where the event log is written.
#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum LogTarget {
    /// Write events to stdout as NDJSON. Default.
    #[default]
    Stdout,

    /// Write events to a file under `.nanny/logs/`. See
    /// `ObservabilityConfig::resolve_log_path`.
    File,
}

// ── Cloud sync ─────────────────────────────────────────────────────────────────

/// Environment variable holding the cloud API key. **The single input that
/// decides whether a run syncs**: set it and events forward, leave it unset and
/// the run is local-only. Nothing else turns sync on: no config field, no
/// credential file, no login command.
///
/// A secret must never live in the committable nanny.toml, so it is injected
/// through the environment, matching both the bridge's own pattern
/// (`NANNY_BRIDGE_CERT`, `NANNY_BRIDGE_KEY`, `NANNY_SESSION_TOKEN`) and how every
/// comparable product ships a telemetry credential (`DD_API_KEY`, `SENTRY_DSN`):
/// a durable secret handed to the process by its platform, identical across
/// every replica, with nothing written to disk to make it work.
pub const API_KEY_ENV: &str = "NANNY_API_KEY";

/// Whether a nanny.toml still carries a `[managed]` section. That block
/// (endpoint / api_key) was retired in favor of the `NANNY_API_KEY` environment
/// variable; it is now ignored, so the CLI warns rather than silently doing
/// nothing. A plain line scan is enough, because the section is a top-level
/// `[managed]` or `[managed.*]` table.
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

// ── Default TOML template ─────────────────────────────────────────────────────

/// The canonical starter nanny.toml written by `nanny init`.
///
/// This is a static string — not generated from structs — so the comments
/// and formatting are preserved exactly as the user will see them.
pub fn default_toml() -> &'static str {
    r#"# Generated by `nanny init`. Edit to match your agent's requirements.
# Full reference: https://docs.nanny.run/v0.5/reference/nanny-toml
#
# There is no [runtime]/mode setting. Enforcement is always fully local, and
# it never depends on Nanny Cloud, an account, or a network connection.
#
# Whether this app ALSO syncs its event log to Nanny Cloud is decided by one
# thing and one thing only: the NANNY_API_KEY environment variable. Set it
# (create a key in the dashboard, then add it to your shell, .env, or your
# host's secrets: Fly, AWS, Azure, Coolify, Kubernetes) and runs sync. Leave
# it unset and everything still works, offline, with the event log written
# locally under .nanny/logs/.
#
# Nothing is written to disk to make sync work, so this behaves identically
# on a laptop, in CI, and across twenty replicas sharing one key. Every run
# prints whether it is syncing, so it can never stop reporting silently.
#
# The cloud never gates a stop; it only adds dashboards, cost, and history.
# Skip one run with `nanny run --no-sync`, or a whole machine with
# NANNY_NO_SYNC=1.

[start]
# How to launch your agent. `nanny run` always reads this command.
# Replace with however you normally start your agent:
#   Python:  cmd = "python agent.py"
#   Rust:    cmd = "cargo run"
#   Node:    cmd = "node agent.js"
cmd = "python agent.py"

[tools]
# Explicit allowlist of tools the agent is permitted to call.
# Any tool not listed here causes an immediate ToolDenied stop.
# An empty list denies all tools. Names must match the function decorated
# with @tool (Python) or #[tool] (Rust).
# http_get is a built-in Rust SDK tool. Replace or extend with your own names.
allowed = ["http_get"]

# Per-tool configuration. Keys must match an entry in the allowed list above.
#
# max_calls caps how many times one tool may be called in a single run.
#
# The five labels below describe what a tool IS. Rules reference labels, never
# tool names, so a rule written for any app can govern yours once its tools are
# labelled. Declare only the ones that are true; all default to false.
#
#   reads_untrusted  ingests content you do not control
#   external_effect  acts on the outside world
#   destructive      irreversible
#   moves_money      a financial transaction
#   reads_sensitive  touches secrets or personal data
#
# [tools.http_get]
# max_calls       = 10
# reads_untrusted = true

[observability]
# Where to write the structured NDJSON event log.
# "stdout" — stream events to the terminal in real time (default).
# "file"   — write events to .nanny/logs/log.ndjson (auto-created).
log = "stdout"

# Uncomment to write events to a file instead:
# log = "file"

# Optional — only set this if you want a name other than the default
# ("log"). A bare name, no extension: nanny always appends .ndjson, and
# the directory is always .nanny/logs/, owned by nanny, never
# configurable here. file = "events" writes .nanny/logs/events.ndjson.
# file = "events"
"#
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn full_config_toml() -> &'static str {
        r#"
[tools]
allowed = ["http_get", "send_email"]

[tools.http_get]
max_calls = 10

[tools.send_email]
max_calls = 2

[observability]
log = "stdout"
"#
    }


    #[test]
    fn default_toml_is_valid() {
        let config: NannyConfig =
            toml::from_str(default_toml()).expect("default_toml() must always be valid TOML");
        assert_eq!(config.tools.allowed, vec!["http_get".to_string()]);
    }

    /// A config carrying nothing but [start] parses. Nothing is mandatory now
    /// that [limits] is gone, and an undeclared allowlist denies every tool
    /// rather than failing to load.
    #[test]
    fn a_config_with_only_start_is_valid() {
        let config: NannyConfig = toml::from_str(r#"
[start]
cmd = "true"
"#).expect("a [start]-only config must parse");
        assert!(config.tools.allowed.is_empty(), "an undeclared allowlist denies everything");
    }

    #[test]
    fn tool_labels_are_parsed() {
        let config: NannyConfig = toml::from_str(r#"
[tools]
allowed = ["web_search", "send_outreach"]

[tools.web_search]
reads_untrusted = true

[tools.send_outreach]
external_effect = true
moves_money     = true
"#).expect("labelled tools must parse");

        let search = &config.tools.per_tool["web_search"];
        assert_eq!(search.labels(), vec!["reads_untrusted"]);

        let outreach = &config.tools.per_tool["send_outreach"];
        assert_eq!(outreach.labels(), vec!["external_effect", "moves_money"]);
    }

    /// Labels default to false, so an unlabelled tool carries none rather than
    /// failing to parse. Silence means "not declared", never "unknown".
    #[test]
    fn unlabelled_tool_has_no_labels() {
        let config: NannyConfig = toml::from_str(r#"
[tools]
allowed = ["http_get"]

[tools.http_get]
max_calls = 3
"#).expect("must parse");

        assert!(config.tools.per_tool["http_get"].labels().is_empty());
    }

    /// Label order follows TOOL_LABELS, not declaration order, so the audit
    /// log and /status stay byte-comparable across runs.
    #[test]
    fn label_order_is_fixed_not_declaration_order() {
        let config: NannyConfig = toml::from_str(r#"
[tools]
allowed = ["t"]

[tools.t]
reads_sensitive = true
moves_money     = true
reads_untrusted = true
"#).expect("must parse");

        assert_eq!(
            config.tools.per_tool["t"].labels(),
            vec!["reads_untrusted", "moves_money", "reads_sensitive"],
        );
    }

    /// An unknown key in [tools.<name>] is rejected rather than ignored. A
    /// misspelled label that parses silently is a control the operator thinks
    /// they declared and does not have.
    #[test]
    fn a_misspelled_label_is_rejected() {
        let result: Result<NannyConfig, _> = toml::from_str(r#"
[tools]
allowed = ["t"]

[tools.t]
reads_untrused = true
"#);
        assert!(result.is_err(), "a misspelled label must not parse silently");
    }

    #[test]
    fn per_tool_max_calls_is_parsed() {
        let config: NannyConfig =
            toml::from_str(full_config_toml()).expect("fixture must parse");

        assert_eq!(config.tools.per_tool.len(), 2);
        assert_eq!(config.tools.per_tool["http_get"].max_calls, Some(10));
        assert_eq!(config.tools.per_tool["send_email"].max_calls, Some(2));
    }






    #[test]
    fn observability_defaults_to_stdout() {
        let config: NannyConfig = toml::from_str(
            r#"
"#,
        )
        .expect("must parse");

        assert_eq!(config.observability.log, LogTarget::Stdout);
        assert!(config.observability.file.is_none());
    }

    #[test]
    fn resolve_log_path_defaults_to_log_ndjson_under_nanny_logs() {
        let dir = std::env::temp_dir().join("nanny_test_resolve_default");
        let _ = std::fs::remove_dir_all(&dir);

        let config = ObservabilityConfig { log: LogTarget::File, file: None };
        let path = config.resolve_log_path(&dir).unwrap().unwrap();
        assert_eq!(path, dir.join(".nanny").join("logs").join("log.ndjson"));
        assert!(dir.join(".nanny").join("logs").is_dir(), "directory must be auto-created");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn resolve_log_path_honors_name_override_and_appends_ndjson() {
        let dir = std::env::temp_dir().join("nanny_test_resolve_override");
        let _ = std::fs::remove_dir_all(&dir);

        let config = ObservabilityConfig { log: LogTarget::File, file: Some("events".to_string()) };
        let path = config.resolve_log_path(&dir).unwrap().unwrap();
        assert_eq!(path, dir.join(".nanny").join("logs").join("events.ndjson"));

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn resolve_log_path_rejects_path_separators_in_name() {
        let dir = std::env::temp_dir().join("nanny_test_resolve_reject_sep");
        let config = ObservabilityConfig { log: LogTarget::File, file: Some("sub/dir".to_string()) };
        assert!(config.resolve_log_path(&dir).is_err(), "a name with a separator must be rejected");
    }

    #[test]
    fn resolve_log_path_rejects_an_extension_in_name() {
        let dir = std::env::temp_dir().join("nanny_test_resolve_reject_ext");
        let config = ObservabilityConfig { log: LogTarget::File, file: Some("events.ndjson".to_string()) };
        assert!(
            config.resolve_log_path(&dir).is_err(),
            "a name with an extension must be rejected — nanny always appends .ndjson itself"
        );
    }

    #[test]
    fn resolve_log_path_is_none_for_stdout() {
        let dir = std::env::temp_dir().join("nanny_test_resolve_stdout");
        let config = ObservabilityConfig { log: LogTarget::Stdout, file: None };
        assert_eq!(config.resolve_log_path(&dir).unwrap(), None);
    }

    #[test]
    fn start_section_is_parsed() {
        let config: NannyConfig = toml::from_str(
            r#"
[start]
cmd = "cargo run --release"

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
    fn legacy_managed_section_is_ignored_but_detectable() {
        // A stale [managed] block no longer breaks parsing (serde ignores
        // unknown keys), and has_managed_section flags it so the CLI can warn.
        // The [runtime] table here is just arbitrary unknown TOML at this
        // point (the field was removed outright, not deprecated), included
        // only to confirm unrelated unknown tables don't break parsing either.
        let contents = r#"
[runtime]
mode = "managed"


[managed]
endpoint = "https://api.nanny.run/v1"
"#;
        let _config: NannyConfig =
            toml::from_str(contents).expect("unknown tables must not break parsing");
        assert!(has_managed_section(contents), "the stale [managed] section must be detectable");
    }

    #[test]
    fn has_managed_section_matches_only_the_managed_table() {
        assert!(has_managed_section("[managed]\nendpoint = \"x\""));
        assert!(has_managed_section("  [managed.sub]\n"));
        assert!(!has_managed_section("[tools]\nallowed = []"));
        assert!(!has_managed_section("# [managed] just a comment"), "a comment is not a section");
    }
}
