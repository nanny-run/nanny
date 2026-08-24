// Nanny CLI — the only surface humans touch.
mod cloud;
mod commands;
mod events;
mod identity;
mod runtime;
mod sync;
//
// Two commands exist:
//   nanny init                        — write a starter nanny.toml in the current directory
//   nanny run <cmd> — run a command under nanny governance
//
// No logic lives here. The CLI loads config and hands off to the runtime.
// All enforcement happens in nanny-core, not here.

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use nanny_bridge::{Bridge, BridgeAddress, ExecutionState};
use nanny_core::events::event::{ExecutionEvent, now_ms};
use std::io::Write;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

// ── CLI shape ─────────────────────────────────────────────────────────────────

#[derive(Parser)]
#[command(
    name = "nanny",
    about = "Execution boundary for autonomous systems",
    long_about = "Nanny enforces what agents and long-running processes are allowed to do.\nIt deterministically stops execution when a policy is violated.",
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
    /// Reads [start].cmd from nanny.toml and runs it. With --serve, instead runs
    /// a headless governance server (no child of its own) that other processes
    /// and machines join, sharing one rule set.
    ///
    /// Example: nanny run
    /// Example: nanny run --serve --addr 0.0.0.0:62669
    Run {
        /// Do not forward events to Nanny Cloud for this run, even with a key set.
        /// Enforcement is unaffected. Also settable with NANNY_NO_SYNC=1.
        #[arg(long)]
        no_sync: bool,

        /// Which Nanny Cloud to forward to. Hidden: this exists for people
        /// *building* Nanny, not people building apps with it. Everyone else
        /// has exactly one cloud and never chooses. Takes a name, never a URL,
        /// so no other endpoint is expressible. See CONTRIBUTING.md.
        #[arg(long, value_enum, default_value_t = cloud::CloudEnv::Prod, hide = true)]
        env: cloud::CloudEnv,

        /// Join an existing governance server by appId (from that server's
        /// `.nanny/app.json`), instead of starting a local bridge. Explicit and
        /// appId-only, never a name, and never auto-detected: two unrelated
        /// governors on one machine must never be able to collide by accident.
        /// Example: nanny run --join=app_3f9c2a1e...
        #[arg(long)]
        join: Option<String>,

        /// Run as a headless governance server that other processes and machines
        /// join, sharing one budget. Same enforcement as `nanny run`, exposed
        /// over the network.
        #[arg(long)]
        serve: bool,

        /// (with --serve) Listen address for the governance API.
        /// Loopback is plain HTTP; a non-loopback address makes mTLS mandatory.
        ///
        /// Left at the default, a busy port steps forward to the next free one
        /// and the real address is recorded for `--join`/`--app` to find.
        /// Named explicitly, a busy port is an error rather than a silent move.
        #[arg(long, default_value_t = nanny_bridge::network::default_governor_addr())]
        addr: SocketAddr,

        /// (with --serve) Server certificate PEM. Defaults to ~/.nanny/certs/server.crt.
        #[arg(long)]
        cert: Option<PathBuf>,

        /// (with --serve) Server private key PEM. Defaults to ~/.nanny/certs/server.key.
        #[arg(long)]
        key: Option<PathBuf>,

        /// (with --serve) CA certificate PEM to validate client certs. Defaults to ~/.nanny/certs/ca.crt.
        #[arg(long)]
        ca: Option<PathBuf>,

        /// Extra arguments appended to [start].cmd.
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        extra_args: Vec<String>,
    },

    /// Remove the nanny binary from its current install location.
    Uninstall,

    /// Manage TLS certificates for the network server.
    ///
    /// Certificates live in ~/.nanny/certs/ by default.
    /// Generate once with `nanny certs generate`, then start the server.
    #[command(subcommand)]
    Certs(commands::certs::CertsCommand),

    /// Show the live status of a running governance server.
    ///
    /// Prints its listen address, connected agents, and current budget.
    Status {
        /// Which app's governor to check, by appId. Defaults to the app identified
        /// by .nanny/app.json in the current directory.
        #[arg(long)]
        app: Option<String>,
    },

    /// Stop a running governance server (SIGTERM, 10-second graceful drain).
    Stop {
        /// Which app's governor to stop, by appId. Defaults to the app identified
        /// by .nanny/app.json in the current directory.
        #[arg(long)]
        app: Option<String>,
    },

    /// Show the health of all active Nanny components.
    ///
    /// Checks: local bridge, network server, certificate expiry.
    /// Exits 0 if healthy, 1 if any active component is unhealthy.
    Health,
}

// ── Entry point ───────────────────────────────────────────────────────────────

fn main() {
    let cli = Cli::parse();

    let result = match cli.command {
        Command::Init => cmd_init(),
        Command::Run { no_sync, env, join, serve, addr, cert, key, ca, extra_args } => {
            if serve {
                if join.is_some() {
                    Err(anyhow::anyhow!(
                        "`--serve` runs the governance server and does not take --join. \
                         Its rules come from nanny.toml, and it *is* the governor, \
                         so there is nothing to join."
                    ))
                } else {
                    // Runs the full network server (mTLS, certs), plus
                    // cloud sync gated on NANNY_API_KEY, honoring --no-sync.
                    // Also launches [start].cmd underneath it when nanny.toml
                    // declares one; without [start] it stays headless for the
                    // shared-governor case. Trailing args append to that
                    // command, exactly as they do for plain `nanny run`.
                    commands::server::cmd_server_start(
                        addr, cert, key, ca, no_sync, env, extra_args,
                    )
                }
            } else {
                cmd_run(&cli.config, no_sync, env, join, extra_args)
            }
        }
        Command::Uninstall => cmd_uninstall(),
        Command::Status { app } => commands::server::cmd_server_status(app),
        Command::Stop { app } => commands::server::cmd_server_stop(app),
        Command::Certs(action) => commands::certs::cmd_certs(action),
        Command::Health => commands::health::cmd_health(),
    };

    if let Err(e) = result {
        // {e:#} prints the full anyhow error chain: "context: cause: root cause"
        eprintln!("error: {e:#}");
        std::process::exit(1);
    }
}

// ── Guard: single nanny.toml per directory ───────────────────────────────────

/// Returns all files matching `nanny*.toml` in `dir`.
/// A project must have exactly one — this enforces that rule.
fn nanny_tomls_in_dir(dir: &Path) -> Result<Vec<PathBuf>> {
    let entries = std::fs::read_dir(dir)
        .with_context(|| format!("failed to read directory '{}'", dir.display()))?;
    let matches = entries
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| {
            p.is_file()
                && p.file_name()
                    .and_then(|n| n.to_str())
                    .map(|n| n.starts_with("nanny") && n.ends_with(".toml"))
                    .unwrap_or(false)
        })
        .collect();
    Ok(matches)
}

// ── nanny init ────────────────────────────────────────────────────────────────

fn cmd_init() -> Result<()> {
    let dest = PathBuf::from("nanny.toml");
    let cwd = Path::new(".");

    let existing = nanny_tomls_in_dir(cwd)?;

    if existing.len() > 1 {
        let mut names: Vec<_> = existing
            .iter()
            .filter_map(|p| p.file_name())
            .map(|n| n.to_string_lossy().into_owned())
            .collect();
        names.sort();
        anyhow::bail!(
            "multiple nanny configuration files found: {}\n\
             A project must have exactly one nanny.toml. Remove the extras first.",
            names.join(", ")
        );
    }

    if dest.exists() {
        print!("nanny.toml already exists. Replace it with the default template?\nYour current configuration will be lost. [y/N] ");
        std::io::stdout().flush().ok();
        let mut input = String::new();
        std::io::stdin().read_line(&mut input).context("failed to read input")?;
        if !matches!(input.trim().to_lowercase().as_str(), "y" | "yes") {
            println!("Skipped, your existing nanny.toml was not changed.");
        } else {
            std::fs::write(&dest, nanny_config::default_toml())
                .context("failed to write nanny.toml")?;
            println!("Replaced existing nanny.toml with fresh defaults.");
        }
    } else {
        std::fs::write(&dest, nanny_config::default_toml())
            .context("failed to write nanny.toml")?;
        println!("Created nanny.toml — edit it to match your agent's requirements.");
    }

    // App identity: written once, ever, per app, never regenerated. This is
    // independent of whether nanny.toml was just replaced, kept, or created;
    // declining the config replace above must never skip identity creation,
    // since a project with a perfectly good hand-tuned nanny.toml (the common
    // case for re-running `nanny init` at all) still needs an id to use
    // `--serve`/`--join` or cloud sync.
    match identity::AppIdentity::load(cwd)? {
        Some(existing) => {
            println!(
                "App identity already set (name: {}, appId: {}), unchanged.",
                existing.name, existing.app_id
            );
        }
        None => {
            let default_name = std::env::current_dir()
                .ok()
                .and_then(|p| p.file_name().map(|n| n.to_string_lossy().into_owned()))
                .unwrap_or_else(|| "app".to_string());
            print!("App name [{default_name}]: ");
            std::io::stdout().flush().ok();
            let mut name_input = String::new();
            std::io::stdin().read_line(&mut name_input).context("failed to read input")?;
            let name = match name_input.trim() {
                "" => default_name,
                trimmed => trimmed.to_string(),
            };

            let created = identity::AppIdentity::create(cwd, name)?;
            println!("App identity created (name: {}, appId: {}).", created.name, created.app_id);
        }
    }

    println!();
    println!("Set [start] cmd to how you normally launch your agent, then:");
    println!("    nanny run");
    println!();
    println!("Works with any language — Python, Rust, Go, Node, or any compiled binary.");

    Ok(())
}

// ── nanny uninstall ───────────────────────────────────────────────────────────

fn cmd_uninstall() -> Result<()> {
    let exe = std::env::current_exe()
        .context("failed to determine current binary path")?;

    // Homebrew manages its own metadata — removing the binary directly leaves
    // the formula in a broken state. Redirect to `brew uninstall nannyd`.
    let path_str = exe.to_string_lossy();
    if path_str.contains("/Cellar/") || path_str.contains("/homebrew/") {
        eprintln!("This looks like a Homebrew-managed installation.");
        eprintln!("Run `brew uninstall nannyd` instead to keep Homebrew consistent.");
        std::process::exit(1);
    }

    cmd_uninstall_impl(&exe)
}

// Windows locks running executables — the process cannot delete itself while running.
// self_replace::self_delete() uses the FILE_FLAG_DELETE_ON_CLOSE + spawned-child
// pattern (the same approach rustup uses) to reliably delete the binary after
// the current process exits, without job object or quoting issues.
#[cfg(windows)]
fn cmd_uninstall_impl(exe: &Path) -> Result<()> {
    let install_dir = exe
        .parent()
        .ok_or_else(|| anyhow::anyhow!("cannot determine install directory"))?;

    // Schedule binary deletion — takes effect once this process exits.
    self_replace::self_delete().context("failed to schedule binary deletion")?;

    // Clean up the PATH registry entry and the install directory.
    // This is a plain registry write — no need for a detached process.
    let dir = install_dir.to_string_lossy();
    let _ = std::process::Command::new("powershell")
        .args([
            "-NoProfile",
            "-NonInteractive",
            "-WindowStyle",
            "Hidden",
            "-Command",
            &format!(
                "$p = [Environment]::GetEnvironmentVariable('PATH','User'); \
                 $new = ($p -split ';' | Where-Object {{ $_ -ne '{dir}' }}) -join ';'; \
                 [Environment]::SetEnvironmentVariable('PATH',$new,'User'); \
                 if (-not (Get-ChildItem '{dir}' -ErrorAction SilentlyContinue)) {{ \
                     Remove-Item -Force -Recurse '{dir}' -ErrorAction SilentlyContinue \
                 }}"
            ),
        ])
        .status();

    println!("nanny uninstalled from {}", exe.display());
    println!("Restart your terminal for PATH changes to take effect.");
    Ok(())
}

#[cfg(not(windows))]
fn cmd_uninstall_impl(exe: &Path) -> Result<()> {
    println!("Removing {}", exe.display());
    std::fs::remove_file(exe).with_context(|| {
        format!(
            "failed to remove '{}'\nIf this is a permissions issue, try: sudo rm {}",
            exe.display(),
            exe.display()
        )
    })?;
    println!("nanny uninstalled.");
    Ok(())
}

// ── nanny run ─────────────────────────────────────────────────────────────────

// ── Network server discovery, by explicit --join=<appId> only ───────────────────

/// State written to `~/.nanny/servers/<app_id>/` by `nanny run --serve`, read
/// here by `nanny run --join=<appId>`. There is no auto-detection, joining a
/// governor is always an explicit, ID-only choice, never "whatever's running
/// on this machine": that blind-join behavior was the exact collision this
/// keying scheme exists to fix.
struct NetworkServerInfo {
    /// Address to inject as NANNY_BRIDGE_ADDR (0.0.0.0 → 127.0.0.1 for local use).
    addr: String,
    /// Session token to inject as NANNY_SESSION_TOKEN. Guards every
    /// governance request.
    token: String,
}

/// Look up the governor for `app_id` and confirm it's actually reachable.
/// Fails loudly (no silent fallback to a local bridge) if the id is unknown or
/// the server isn't up, an explicit `--join` that doesn't find its target is
/// a mistake worth surfacing, not something to quietly paper over.
fn detect_joined_server(app_id: &str) -> Result<NetworkServerInfo> {
    let state_dir = commands::server::nanny_server_state_dir(app_id)?;
    let addr_raw = std::fs::read_to_string(state_dir.join("server.addr")).with_context(|| {
        format!(
            "no governance server found for app '{app_id}', is `nanny run --serve` \
             running for it? (expected state at {})",
            state_dir.display()
        )
    })?;
    let token = std::fs::read_to_string(state_dir.join("server.token"))
        .with_context(|| format!("missing session token for app '{app_id}'"))?;

    let addr_raw = addr_raw.trim().to_string();
    let token = token.trim().to_string();
    if addr_raw.is_empty() || token.is_empty() {
        anyhow::bail!("server state for app '{app_id}' is corrupt (empty addr or token)");
    }

    // Replace 0.0.0.0 (bind-all listen addr) with 127.0.0.1 for local connections.
    let connect_addr = addr_raw.replace("0.0.0.0", "127.0.0.1");

    let socket_addr: SocketAddr = connect_addr
        .parse()
        .with_context(|| format!("invalid server address for app '{app_id}': {connect_addr}"))?;

    if std::net::TcpStream::connect_timeout(&socket_addr, std::time::Duration::from_millis(500))
        .is_err()
    {
        anyhow::bail!(
            "app '{app_id}' has server state but isn't reachable at {connect_addr}, \
             it may have stopped. Check with: nanny status --app={app_id}"
        );
    }

    Ok(NetworkServerInfo { addr: connect_addr, token })
}


fn cmd_run_via_network_server(command: Vec<String>, server: NetworkServerInfo) -> Result<()> {
    println!("nanny: network server detected at {}", server.addr);
    println!("nanny: governance enforced remotely — tool permission and rules apply");
    println!();

    let (mut cmd, run_id) = build_governed_child(command, &server)?;

    // Declare this app to the governor for this run, before the child can do
    // anything attributable. A governor holds one credential but serves many
    // apps, so identity has to travel per run in the event stream; without
    // this, everything a joined process does would be filed under the
    // governor's own app.
    declare_app_to_governor(&server, Path::new("."), &run_id);

    let status = cmd
        .status()
        .with_context(|| format!("failed to run '{}'", command_program(&cmd)))?;

    if !status.success() {
        std::process::exit(status.code().unwrap_or(1));
    }

    Ok(())
}

/// The program name from a built command, for error messages.
fn command_program(cmd: &std::process::Command) -> String {
    cmd.get_program().to_string_lossy().into_owned()
}

/// Build a child process wired to a governance server: transport, credentials,
/// run id, and mTLS certs.
///
/// Shared by `--join` (joining someone else's governor) and `--serve` (running
/// the app under the governor this process just started), so the two can never
/// drift on how a governed child is wired.
fn build_governed_child(
    command: Vec<String>,
    server: &NetworkServerInfo,
) -> Result<(std::process::Command, String)> {
    let (program, args) = command.split_first().expect("command is non-empty");

    // Cert files from ~/.nanny/certs/ — auto-injected if present.
    // Cross-machine deployments override these via NANNY_BRIDGE_CERT/KEY/CA.
    let certs_dir = dirs::home_dir()
        .context("cannot determine home directory")?
        .join(".nanny")
        .join("certs");

    // Each `nanny run` is its own run on the server: a stop ends this run, not
    // the server, so the host survives many sequential runs (G3). Set
    // NANNY_RUN_ID yourself to make several processes share one budget and stop
    // together (e.g. a fleet demo); otherwise each invocation gets a fresh id.
    let run_id = std::env::var("NANNY_RUN_ID")
        .ok()
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());

    let mut cmd = std::process::Command::new(program);
    cmd.args(args);
    cmd.env("NANNY_BRIDGE_ADDR",    &server.addr);
    cmd.env("NANNY_SESSION_TOKEN",  &server.token);
    cmd.env("NANNY_RUN_ID",         &run_id);

    // Only inject cert paths that actually exist — agents on remote machines
    // may have already set these env vars themselves via their deployment config.
    let cert_file = certs_dir.join("client.crt");
    let key_file  = certs_dir.join("client.key");
    let ca_file   = certs_dir.join("ca.crt");
    if cert_file.exists() { cmd.env("NANNY_BRIDGE_CERT", &cert_file); }
    if key_file.exists()  { cmd.env("NANNY_BRIDGE_KEY",  &key_file); }
    if ca_file.exists()   { cmd.env("NANNY_BRIDGE_CA",   &ca_file); }

    Ok((cmd, run_id))
}

/// Tell the governor which app this run belongs to.
///
/// Done from the CLI rather than the child because a black-box app governed
/// only by the proxy has no SDK to declare with. Borrows the SDK's own client
/// by setting the same env vars on this process that were staged onto the
/// child, so all three transports (socket, loopback TCP, mTLS) stay one
/// implementation rather than a second copy of the logic here.
///
/// Best-effort: an app with no `.nanny/app.json` simply doesn't declare, and
/// Cloud groups its runs the way it always has.
fn declare_app_to_governor(server: &NetworkServerInfo, dir: &Path, run_id: &str) {
    let Ok(Some(app)) = identity::AppIdentity::load(dir) else {
        return;
    };
    // SAFETY: called before the child is spawned, with nothing else in this
    // process reading the environment concurrently. These are the same values
    // already staged onto the child's command, the run id especially, since
    // declaring against a different run would file the identity under a run
    // that never does any work.
    unsafe {
        std::env::set_var("NANNY_BRIDGE_ADDR", &server.addr);
        std::env::set_var("NANNY_SESSION_TOKEN", &server.token);
        std::env::set_var("NANNY_RUN_ID", run_id);
    }
    nanny::set_app(app.app_id, app.name);
}

fn cmd_run(
    config_path: &Path,
    no_sync: bool,
    env: cloud::CloudEnv,
    join: Option<String>,
    extra_args: Vec<String>,
) -> Result<()> {
    // Guard: exactly one nanny*.toml allowed per directory.
    let config_dir = config_path
        .parent()
        .map(|p| if p == Path::new("") { Path::new(".") } else { p })
        .unwrap_or(Path::new("."));
    let existing = nanny_tomls_in_dir(config_dir)?;
    if existing.len() > 1 {
        let mut names: Vec<_> = existing
            .iter()
            .filter_map(|p| p.file_name())
            .map(|n| n.to_string_lossy().into_owned())
            .collect();
        names.sort();
        anyhow::bail!(
            "multiple nanny configuration files found in '{}': {}\n\
             A project must have exactly one nanny.toml. Remove the extras.",
            config_dir.display(),
            names.join(", ")
        );
    }

    // Load and validate config — fail immediately if anything is wrong.
    let config = nanny_config::load(config_path)
        .with_context(|| format!("failed to load config from '{}'", config_path.display()))?;

    // [managed] endpoint/api_key, and [runtime]/mode, are both retired; either
    // is now silently ignored by the parser, so warn rather than silently do
    // nothing. There is no config knob to "keep" anymore: sync is decided
    // entirely by whether NANNY_API_KEY is set in the environment.
    if let Ok(raw) = std::fs::read_to_string(config_path) {
        if nanny_config::has_managed_section(&raw) {
            eprintln!(
                "nanny: [managed] in nanny.toml is deprecated and ignored. Set the \
                 NANNY_API_KEY environment variable and Cloud sync happens \
                 automatically, no config needed."
            );
        }
    }

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

    // ── Explicit governor join ────────────────────────────────────────────────
    // Only when `--join=<appId>` is given. There is no auto-detection: joining a
    // governor is always an explicit, ID-only choice, so two unrelated apps on
    // one machine can never collide by one silently absorbing the other's run.
    if let Some(app_id) = join.as_deref() {
        let server = detect_joined_server(app_id)?;
        return cmd_run_via_network_server(command, server);
    }

    // Build the wired runtime from config.
    let components = runtime::build_from_config(&config);

    println!("nanny: config loaded from '{}'", config_path.display());
    println!("nanny: tools allowed — {:?}", config.tools.allowed);

    let registered = components.registry.registered_names();
    println!("nanny: registry — {} tool(s) registered: {:?}", registered.len(), registered);
    println!();

    let started_at = Instant::now();

    // ── Open event log ────────────────────────────────────────────────────
    let mut log = events::EventWriter::from_config(&config.observability, config_dir)?;

    let started_event = execution_started_event(&command.join(" "));
    log.write(&started_event)?;

    // ── Start bridge ──────────────────────────────────────────────────────
    let bridge_components = runtime::build_bridge_components(&config);
    let bridge = Bridge::start(bridge_components)
        .context("failed to start bridge")?;

    // ── Cloud sync (off unless NANNY_API_KEY is set) ────────────────────────
    // Forwards a copy of the NDJSON event log to the cloud; enforcement stays
    // fully local. Fire-and-forget, never blocks or fails the run. The key is
    // the only input: no config field, no credential file, nothing written to
    // disk. The status line prints on every run either way, because a run that stops
    // reporting must never do so silently.
    // Declare which app this is, before anything else can be attributed to it.
    // Identity rides in the event stream rather than being derived from the API
    // key, so one credential can serve many apps and each still lands under its
    // own name. An app with no `.nanny/app.json` simply doesn't declare, and
    // Cloud groups its runs the way it always has.
    let app = identity::AppIdentity::load(config_dir).ok().flatten();
    if let Some(app) = &app {
        bridge.declare_app(&app.app_id, &app.name);
    }
    let app_name = app.map(|a| a.name);

    let target = sync::resolve_sync(env, no_sync);
    println!("{}", sync::sync_status_line(target.as_ref().map_err(|e| *e), app_name.as_deref()));
    let managed = target
        .ok()
        .and_then(|t| sync::CloudSync::start(t.endpoint, t.api_key, &bridge.session_token, config_dir));
    // ExecutionStarted was already written locally; forward it too.
    if let (Some(sender), Ok(line)) = (&managed, serde_json::to_string(&started_event)) {
        sender.enqueue(line);
    }

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

    // ── Poll until exit or a bridge-signaled stop ────────────────────────
    //
    // We poll every 50 ms. Coarse enough to avoid busy-spinning; fine enough
    // that a stop is noticed promptly. The bridge signals stop (allowlist,
    // rules) independently of the child's own exit — we must check both.
    //
    // Bridge events (ToolAllowed, RuleDenied, ToolDenied, …) are drained on every tick
    // so the NDJSON stream is written in near-real-time — `tail -f` on the
    // log file shows events as they happen, not just at execution end.
    let poll_interval = Duration::from_millis(50);
    let stop_reason: String = loop {
        // Drain any bridge events accumulated since the last tick.
        for line in bridge.drain_events() {
            let _ = log.write_raw(&line);
            if let Some(sender) = &managed {
                sender.enqueue(line);
            }
        }

        // Check bridge first — it may have stopped execution (allowlist, rules).
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
        if let Some(sender) = &managed {
            sender.enqueue(line);
        }
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

    let stopped_event = execution_stopped_event(
        &stop_reason,
        metrics.step_count,
        metrics.tokens_spent,
        elapsed_ms,
    );
    log.write(&stopped_event)?;
    if let Some(sender) = &managed {
        if let Ok(line) = serde_json::to_string(&stopped_event) {
            sender.enqueue(line);
        }
    }

    // Flush the managed forwarder before exit (bounded by its request timeout).
    if let Some(sender) = managed {
        sender.flush_and_join();
    }

    // ── Exit code ─────────────────────────────────────────────────────────
    if stop_reason != "AgentCompleted" {
        eprintln!("nanny: stopped — {stop_reason}");
        std::process::exit(1);
    }

    Ok(())
}

// ── Event constructors ────────────────────────────────────────────────────────

fn execution_started_event(command: &str) -> ExecutionEvent {
    ExecutionEvent::ExecutionStarted {
        ts: now_ms(),
        command: command.to_string(),
    }
}

fn execution_stopped_event(reason: &str, steps: u32, tokens_spent: u64, elapsed_ms: u64) -> ExecutionEvent {
    ExecutionEvent::ExecutionStopped {
        ts: now_ms(),
        reason: reason.to_string(),
        steps,
        tokens_spent,
        elapsed_ms,
    }
}
