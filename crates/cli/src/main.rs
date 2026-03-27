// Nanny CLI — the only surface humans touch.
mod events;
mod runtime;
//
// Two commands exist:
//   nanny init                        — write a starter nanny.toml in the current directory
//   nanny run [--limits=<name>] <cmd> — run a command under nanny enforcement
//
// No logic lives here. The CLI loads config and hands off to the runtime.
// All enforcement happens in nanny-core, not here.

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use nanny_bridge::{Bridge, BridgeAddress, ExecutionState};
use nanny_core::agent::limits::Limits;
use nanny_core::events::event::{ExecutionEvent, LimitsSnapshot, now_ms};
use nanny_core::ledger::Ledger;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

// ── CLI shape ─────────────────────────────────────────────────────────────────

#[derive(Parser)]
#[command(
    name = "nanny",
    about = "Execution boundary for autonomous systems",
    long_about = "Nanny enforces hard limits on agents and long-running processes.\nIt deterministically stops execution when a limit is reached.",
    version
)]
struct Cli {
    /// Path to the nanny.toml config file. Defaults to ./nanny.toml
    #[arg(long, global = true, default_value = "nanny.toml")]
    config: PathBuf,

    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Initialize a nanny.toml in the current directory.
    ///
    /// Creates a starter config with safe default limits and prints
    /// a code snippet showing how to integrate with your agent.
    Init,

    /// Run the project under nanny enforcement.
    ///
    /// Reads [start].cmd from nanny.toml and runs it.
    /// Extra arguments passed after -- are appended to [start].cmd.
    ///
    /// Example: nanny run
    /// Example: nanny run --limits=researcher
    /// Example: nanny run -- "research topic"
    Run {
        /// Named limits set to activate from nanny.toml [limits.<name>].
        /// Inherits from [limits] defaults and overrides only declared fields.
        /// Example: --limits=researcher activates [limits.researcher]
        #[arg(long)]
        limits: Option<String>,

        /// Extra arguments appended to [start].cmd.
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        extra_args: Vec<String>,
    },
}

// ── Entry point ───────────────────────────────────────────────────────────────

fn main() {
    let cli = Cli::parse();

    let result = match cli.command {
        Command::Init => cmd_init(),
        Command::Run { limits, extra_args } => cmd_run(&cli.config, limits.as_deref(), extra_args),
    };

    if let Err(e) = result {
        // {e:#} prints the full anyhow error chain: "context: cause: root cause"
        eprintln!("error: {e:#}");
        std::process::exit(1);
    }
}

// ── nanny init ────────────────────────────────────────────────────────────────

fn cmd_init() -> Result<()> {
    let dest = PathBuf::from("nanny.toml");

    if dest.exists() {
        anyhow::bail!(
            "nanny.toml already exists in this directory.\n\
             Edit it directly or delete it and run `nanny init` again."
        );
    }

    std::fs::write(&dest, nanny_config::default_toml())
        .context("failed to write nanny.toml")?;

    println!("Created nanny.toml — edit it to match your agent's requirements.");
    println!();
    println!("Set [start] cmd to how you normally launch your agent, then:");
    println!("    nanny run");
    println!("    nanny run --limits=researcher");
    println!("    nanny run -- \"my topic\"");
    println!();
    println!("Works with any language — Python, Rust, Go, Node, or any compiled binary.");

    Ok(())
}

// ── nanny run ─────────────────────────────────────────────────────────────────

fn cmd_run(config_path: &Path, limits_name: Option<&str>, extra_args: Vec<String>) -> Result<()> {
    // Load and validate config — fail immediately if anything is wrong.
    let config = nanny_config::load(config_path)
        .with_context(|| format!("failed to load config from '{}'", config_path.display()))?;

    // Require [start] — nanny run always reads the command from config.
    let start = config.start.as_ref()
        .ok_or_else(|| anyhow::anyhow!("no start config found in nanny.toml"))?;

    // Build command: parse [start].cmd with shell quoting rules, then append extra args.
    // shlex::split handles quoted paths and escaped spaces — e.g. 'python "my agent.py"'.
    let mut command: Vec<String> = shlex::split(&start.cmd)
        .ok_or_else(|| anyhow::anyhow!(
            "invalid [start].cmd in nanny.toml: unterminated quote or invalid shell syntax: {:?}",
            start.cmd
        ))?;
    if command.is_empty() {
        return Err(anyhow::anyhow!("[start].cmd in nanny.toml is empty"));
    }
    command.extend(extra_args);

    // Build the wired runtime from config.
    // If a named limits set was requested, resolve it with inheritance.
    let components = if let Some(name) = limits_name {
        runtime::build_from_config_named(&config, name)
            .with_context(|| format!("failed to activate limits set '{name}'"))?
    } else {
        runtime::build_from_config(&config)
    };

    // Print what limits are active before running anything.
    let active_set = limits_name.unwrap_or("[limits]");
    println!("nanny: config loaded from '{}'", config_path.display());
    println!("nanny: limits ({active_set}) — steps={} cost={} timeout={}ms",
        components.limits.max_steps,
        components.limits.max_cost_units,
        components.limits.timeout_ms,
    );
    println!("nanny: mode — {:?}", config.runtime.mode);
    println!("nanny: tools allowed — {:?}", config.tools.allowed);

    let registered = components.registry.registered_names();
    println!("nanny: registry — {} tool(s) registered: {:?}", registered.len(), registered);
    println!("nanny: ledger — {} units", components.ledger.balance());
    println!();

    let timeout = Duration::from_millis(components.limits.timeout_ms);
    let started_at = Instant::now();

    // ── Open event log ────────────────────────────────────────────────────
    let mut log = events::EventWriter::from_config(&config.observability)?;

    log.write(&execution_started_event(&components.limits, active_set, &command.join(" ")))?;

    // ── Start bridge ──────────────────────────────────────────────────────
    let bridge_components = runtime::build_bridge_components(&config, components.limits.clone());
    let bridge = Bridge::start(bridge_components)
        .context("failed to start bridge")?;

    // ── Spawn child process ───────────────────────────────────────────────
    let (program, args) = command.split_first()
        .expect("command is non-empty — enforced by clap");

    let mut cmd = std::process::Command::new(program);
    cmd.args(args);
    match &bridge.address {
        #[cfg(unix)]
        BridgeAddress::Unix(path) => { cmd.env("NANNY_BRIDGE_SOCKET", path); }
        BridgeAddress::Tcp(port) => { cmd.env("NANNY_BRIDGE_PORT", port.to_string()); }
    }
    cmd.env("NANNY_SESSION_TOKEN", &bridge.session_token);

    let mut child = match cmd.spawn()
    {
        Ok(c) => c,
        Err(e) => {
            // ExecutionStarted was emitted — always pair it with ExecutionStopped.
            let elapsed_ms = started_at.elapsed().as_millis() as u64;
            let _ = log.write(&execution_stopped_event("SpawnFailed", 0, 0, elapsed_ms));
            return Err(e).with_context(|| format!("failed to spawn '{}'", program));
        }
    };

    // ── Poll until exit, timeout, or bridge-signaled stop ────────────────
    //
    // We poll every 50 ms. Coarse enough to avoid busy-spinning;
    // fine enough that a 30-second timeout fires within half a tick.
    // The bridge signals stop (budget, rules, max-steps) independently
    // of the child's own exit — we must check both.
    //
    // Bridge events (ToolCalled, ToolAllowed, …) are drained on every tick
    // so the NDJSON stream is written in near-real-time — `tail -f` on the
    // log file shows events as they happen, not just at execution end.
    let poll_interval = Duration::from_millis(50);
    let stop_reason: String = loop {
        // Drain any bridge events accumulated since the last tick.
        for line in bridge.drain_events() {
            let _ = log.write_raw(&line);
        }

        // Check bridge first — it may have stopped execution (budget, rules, etc.)
        if let ExecutionState::Stopped { reason } = bridge.execution_state() {
            let _ = child.kill();
            let _ = child.wait(); // reap — avoid zombie
            break reason;
        }

        match child.try_wait() {
            Ok(Some(status)) => {
                // Use exit status as the fallback reason only.
                // The child may have called POST /stop before dying (e.g. for
                // RuleDenied or ToolFailed), in which case the bridge already
                // has the specific reason. bridge.stop() is idempotent — it
                // won't overwrite a reason the child already reported.
                let fallback = if status.success() { "AgentCompleted" } else { "ProcessCrashed" };
                bridge.stop(fallback);
                // Re-read: prefer the bridge's reason over the generic fallback.
                let reason = match bridge.execution_state() {
                    nanny_bridge::ExecutionState::Stopped { reason } => reason,
                    nanny_bridge::ExecutionState::Running => fallback.to_string(),
                };
                break reason;
            }
            Ok(None) => {
                if started_at.elapsed() >= timeout {
                    let _ = child.kill();
                    let _ = child.wait(); // reap — avoid zombie
                    bridge.stop("TimeoutExpired");
                    break "TimeoutExpired".to_string();
                }
                std::thread::sleep(poll_interval);
            }
            Err(e) => {
                // Polling failed — emit stopped before surfacing the error.
                let elapsed_ms = started_at.elapsed().as_millis() as u64;
                let _ = log.write(&execution_stopped_event("InternalError", 0, 0, elapsed_ms));
                return Err(e).context("failed to poll child process");
            }
        }
    };

    // ── Final event drain ─────────────────────────────────────────────────
    // Catch any events generated during the stop transition itself (e.g. a
    // ToolDenied that caused budget exhaustion on the very last bridge call).
    for line in bridge.drain_events() {
        let _ = log.write_raw(&line);
    }

    // ── ExecutionStopped event ────────────────────────────────────────────
    let elapsed_ms = started_at.elapsed().as_millis() as u64;
    let metrics = bridge.metrics();

    // Warn when tools are configured but the agent never called any.
    // This usually means the model ignored its tool definitions — a common
    // sign of a model that is too small or a prompt that needs improvement.
    // Suppress the warning when execution was stopped by a governance decision
    // (rule denial, tool denial, budget) — in that case 0 calls is expected.
    let is_governance_stop = matches!(
        stop_reason.as_str(),
        "RuleDenied" | "ToolDenied" | "BudgetExhausted" | "MaxStepsReached" | "TimeoutExpired"
    );
    if metrics.allowed_tool_count > 0 && metrics.tool_call_count == 0 && !is_governance_stop {
        eprintln!(
            "nanny: warning — execution completed with 0 tool calls \
             ({} tool(s) were allowed). \
             The model may have ignored its tool definitions.",
            metrics.allowed_tool_count
        );
    }

    log.write(&execution_stopped_event(
        &stop_reason,
        metrics.step_count,
        metrics.cost_units_spent,
        elapsed_ms,
    ))?;

    // ── Exit code ─────────────────────────────────────────────────────────
    if stop_reason != "AgentCompleted" {
        eprintln!("nanny: stopped — {stop_reason}");
        std::process::exit(1);
    }

    Ok(())
}

// ── Event constructors ────────────────────────────────────────────────────────

fn execution_started_event(limits: &Limits, limits_set: &str, command: &str) -> ExecutionEvent {
    ExecutionEvent::ExecutionStarted {
        ts: now_ms(),
        limits: LimitsSnapshot {
            steps: limits.max_steps,
            cost: limits.max_cost_units,
            timeout: limits.timeout_ms,
        },
        limits_set: limits_set.to_string(),
        command: command.to_string(),
    }
}

fn execution_stopped_event(reason: &str, steps: u32, cost_spent: u64, elapsed_ms: u64) -> ExecutionEvent {
    ExecutionEvent::ExecutionStopped {
        ts: now_ms(),
        reason: reason.to_string(),
        steps,
        cost_spent,
        elapsed_ms,
    }
}
