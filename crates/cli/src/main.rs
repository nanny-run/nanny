// Nanny CLI — the only surface humans touch.
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
use nanny_core::ledger::Ledger;
use std::path::{Path, PathBuf};

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

    /// Run a command under nanny enforcement.
    ///
    /// Example: nanny run system.py
    /// Example: nanny run --limits=researcher system.py
    /// Example: nanny run -- python agent.py --verbose
    Run {
        /// Named limits set to activate from nanny.toml [limits.<name>].
        /// Inherits from [limits] defaults and overrides only declared fields.
        /// Example: --limits=researcher activates [limits.researcher]
        #[arg(long)]
        limits: Option<String>,

        /// The command and arguments to execute under enforcement.
        #[arg(trailing_var_arg = true, required = true)]
        command: Vec<String>,
    },
}

// ── Entry point ───────────────────────────────────────────────────────────────

fn main() {
    let cli = Cli::parse();

    let result = match cli.command {
        Command::Init => cmd_init(),
        Command::Run { limits, command } => cmd_run(&cli.config, limits.as_deref(), command),
    };

    if let Err(e) = result {
        eprintln!("error: {e}");
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
    println!("Rust integration:");
    println!("    let config = nanny_config::load(Path::new(\"nanny.toml\"))?;");
    println!("    let components = runtime::build_from_config(&config);");
    println!("    // wire components into your Executor");
    println!();
    println!("Then run:");
    println!("    nanny run system.py");
    println!("    nanny run --limits=researcher system.py");

    Ok(())
}

// ── nanny run ─────────────────────────────────────────────────────────────────

fn cmd_run(config_path: &Path, limits_name: Option<&str>, command: Vec<String>) -> Result<()> {
    // Load and validate config — fail immediately if anything is wrong.
    let config = nanny_config::load(config_path)
        .with_context(|| format!("failed to load config from '{}'", config_path.display()))?;

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

    // Process execution implemented on Day 12.
    println!("nanny: would run — {}", command.join(" "));
    println!("nanny: process execution not yet implemented (Day 12)");

    Ok(())
}
