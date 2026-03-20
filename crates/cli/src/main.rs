// Nanny CLI — the only surface humans touch.
//
// Two commands exist:
//   nanny init          — write a starter nanny.toml in the current directory
//   nanny run -- <cmd>  — run a command under nanny enforcement
//
// No logic lives here. The CLI loads config and hands off to the runtime.
// All enforcement happens in nanny-core, not here.

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
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
    /// Creates a starter config with safe default limits.
    /// Does not modify any existing files or source code.
    Init,

    /// Run a command under nanny enforcement.
    ///
    /// Example: nanny run -- python agent.py
    /// Example: nanny run -- node index.js
    Run {
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
        Command::Run { command } => cmd_run(&cli.config, command),
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

    println!("Created nanny.toml with safe defaults.");
    println!();
    println!("Next steps:");
    println!("  1. Edit nanny.toml to match your agent's requirements");
    println!("  2. Run your agent:  nanny run -- <your command>");

    Ok(())
}

// ── nanny run ─────────────────────────────────────────────────────────────────

fn cmd_run(config_path: &Path, command: Vec<String>) -> Result<()> {
    // Load and validate config — fail immediately if anything is wrong.
    let config = nanny_config::load(config_path)
        .with_context(|| format!("failed to load config from '{}'", config_path.display()))?;

    // Confirm what limits are in effect before running anything.
    println!("nanny: config loaded from '{}'", config_path.display());
    println!("nanny: limits — max_steps={} max_cost_units={} timeout_ms={}",
        config.limits.max_steps,
        config.limits.max_cost_units,
        config.limits.timeout_ms,
    );
    println!("nanny: tools allowed — {:?}", config.tools.allowed);
    println!("nanny: mode — {:?}", config.mode);
    println!();

    // Execution loop is wired here on Day 11.
    // For now we confirm the command that would be run under enforcement.
    println!("nanny: would run — {}", command.join(" "));
    println!("nanny: execution loop not yet implemented (Day 11)");

    Ok(())
}
