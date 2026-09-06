// Nanny CLI: the only surface humans touch.
mod cloud;
mod commands;
mod events;
mod identity;
mod runtime;
mod sync;
//
// Two commands exist:
//   nanny init                       : write a starter nanny.toml in the current directory
//   nanny run <cmd>: run a command under nanny governance
//
// No logic lives here. The CLI loads config and hands off to the runtime.
// All enforcement happens in nanny-core, not here.

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use nanny_bridge::{Bridge, BridgeAddress, ExecutionState};
use nanny_core::events::event::{now_ms, ExecutionEvent};
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
    /// Brings up the governor and runs [start].cmd from nanny.toml underneath
    /// it. On loopback that needs no certificates and no setup, so the same
    /// command covers a laptop and a deployment. Other processes and machines
    /// join the same governor with --join, sharing one rule set. Without
    /// [start] it stays headless, for the case where every app arrives via
    /// --join.
    ///
    /// Example: nanny run
    ///
    /// Example: nanny run --addr 0.0.0.0:62669
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
        /// `.nanny/app.json`), instead of starting a governor. Explicit and
        /// appId-only, never a name, and never auto-detected: two unrelated
        /// governors on one machine must never be able to collide by accident.
        /// Example: nanny run --join=app_3f9c2a1e...
        #[arg(long)]
        join: Option<String>,

        /// Listen address for the governance API.
        /// Loopback is plain HTTP; a non-loopback address makes mTLS mandatory.
        ///
        /// Left at the default, a busy port steps forward to the next free one
        /// and the real address is recorded for `--join`/`--app` to find.
        /// Named explicitly, a busy port is an error rather than a silent move.
        #[arg(long, default_value_t = nanny_bridge::network::default_governor_addr())]
        addr: SocketAddr,

        /// Use this app's live certificate bundle rather than
        /// its sandbox one. Ignored when --cert/--key/--ca are given.
        #[arg(long)]
        live: bool,

        /// Server certificate PEM. Defaults to the app's sandbox bundle.
        #[arg(long)]
        cert: Option<PathBuf>,

        /// Server private key PEM. Defaults to the app's sandbox bundle.
        #[arg(long)]
        key: Option<PathBuf>,

        /// CA certificate PEM to validate client certs. Defaults to the app's sandbox bundle.
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
    /// Prints its listen address and connected agents.
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

    /// Install and inspect rule packs.
    ///
    /// A pack is vendored into `.nanny/rules/` and declared in `[rules]
    /// extends`. Your source is never edited: `@rule` remains for your own
    /// private rules.
    #[command(subcommand)]
    Rules(commands::rules::RulesCommand),

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
        Command::Run {
            no_sync,
            env,
            join,
            addr,
            cert,
            key,
            ca,
            live,
            extra_args,
        } => {
            // One shape. `nanny run` starts a governor and launches
            // [start].cmd underneath it; without [start] it stays headless for
            // the shared-governor case. There is no second, quieter run path
            // to fall through to, so what a laptop exercises is what a
            // deployment runs.
            if let Some(app_id) = join {
                cmd_run_joined(&app_id, extra_args)
            } else {
                commands::server::cmd_server_start(
                    addr,
                    commands::server::TlsSource {
                        cert,
                        key,
                        ca,
                        live,
                    },
                    no_sync,
                    env,
                    extra_args,
                )
            }
        }
        Command::Uninstall => cmd_uninstall(),
        Command::Status { app } => commands::server::cmd_server_status(app),
        Command::Stop { app } => commands::server::cmd_server_stop(app),
        Command::Certs(action) => commands::certs::cmd_certs(action),
        Command::Rules(cmd) => {
            let root = std::env::current_dir().unwrap_or_else(|_| ".".into());
            commands::rules::run(cmd, &root)
        }
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
/// A project must have exactly one: this enforces that rule.
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
        std::io::stdin()
            .read_line(&mut input)
            .context("failed to read input")?;
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
        println!("Created nanny.toml, edit it to match your agent's requirements.");
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
            std::io::stdin()
                .read_line(&mut name_input)
                .context("failed to read input")?;
            let name = match name_input.trim() {
                "" => default_name,
                trimmed => trimmed.to_string(),
            };

            let created = identity::AppIdentity::create(cwd, name)?;
            println!(
                "App identity created (name: {}, appId: {}).",
                created.name, created.app_id
            );
        }
    }

    println!();
    println!("Set [start] cmd to how you normally launch your agent, then:");
    println!("    nanny run");
    println!();
    println!("Works with any language, Python, Rust, Go, Node, or any compiled binary.");

    Ok(())
}

// ── nanny uninstall ───────────────────────────────────────────────────────────

fn cmd_uninstall() -> Result<()> {
    let exe = std::env::current_exe().context("failed to determine current binary path")?;

    // Homebrew manages its own metadata: removing the binary directly leaves
    // the formula in a broken state. Redirect to `brew uninstall nannyd`.
    let path_str = exe.to_string_lossy();
    if path_str.contains("/Cellar/") || path_str.contains("/homebrew/") {
        eprintln!("This looks like a Homebrew-managed installation.");
        eprintln!("Run `brew uninstall nannyd` instead to keep Homebrew consistent.");
        std::process::exit(1);
    }

    cmd_uninstall_impl(&exe)
}

// Windows locks running executables: the process cannot delete itself while running.
// self_replace::self_delete() uses the FILE_FLAG_DELETE_ON_CLOSE + spawned-child
// pattern (the same approach rustup uses) to reliably delete the binary after
// the current process exits, without job object or quoting issues.
#[cfg(windows)]
fn cmd_uninstall_impl(exe: &Path) -> Result<()> {
    let install_dir = exe
        .parent()
        .ok_or_else(|| anyhow::anyhow!("cannot determine install directory"))?;

    // Schedule binary deletion: takes effect once this process exits.
    self_replace::self_delete().context("failed to schedule binary deletion")?;

    // Clean up the PATH registry entry and the install directory.
    // This is a plain registry write: no need for a detached process.
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

    Ok(NetworkServerInfo {
        addr: connect_addr,
        token,
    })
}

/// `nanny run --join=<appId>`: run `[start].cmd` against a governor that is
/// already up, instead of starting one.
///
/// The governor owns the rules, the event log and the cloud forwarding for
/// every run it holds, so this reads `nanny.toml` only for the command to
/// launch. Everything else about how the run is governed arrives from the
/// other end of the connection.
fn cmd_run_joined(app_id: &str, extra_args: Vec<String>) -> Result<()> {
    let cwd = std::env::current_dir().context("failed to read the current directory")?;
    let config_path = cwd.join("nanny.toml");

    let existing = nanny_tomls_in_dir(&cwd)?;
    if existing.len() > 1 {
        let mut names: Vec<String> = existing
            .iter()
            .filter_map(|p| p.file_name())
            .map(|n| n.to_string_lossy().into_owned())
            .collect();
        names.sort();
        anyhow::bail!(
            "multiple nanny configuration files found in '{}': {}\n\
             A project must have exactly one nanny.toml. Remove the extras.",
            cwd.display(),
            names.join(", ")
        );
    }

    let config = nanny_config::load(&config_path)
        .with_context(|| format!("failed to load config from '{}'", config_path.display()))?;
    let start = config.start.as_ref().ok_or_else(|| {
        anyhow::anyhow!(
            "no [start] in nanny.toml, so there is no command to run under the \
             governor you joined. Add [start] cmd = \"...\" here."
        )
    })?;
    let mut command: Vec<String> = shlex::split(&start.cmd)
        .ok_or_else(|| anyhow::anyhow!("could not parse [start].cmd: {}", start.cmd))?;
    if command.is_empty() {
        anyhow::bail!("[start].cmd is empty");
    }
    command.extend(extra_args);

    let server = detect_joined_server(app_id)?;
    cmd_run_via_network_server(command, server)
}

fn cmd_run_via_network_server(command: Vec<String>, server: NetworkServerInfo) -> Result<()> {
    println!("nanny: network server detected at {}", server.addr);
    println!("nanny: governance enforced remotely, tool permission and rules apply");
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
/// Resolve the rule packs a config declares, refusing to start without them.
///
/// A pack named in `[rules] extends` but absent from disk means the operator
/// believes controls are in force that are not, so the honest response is to
/// refuse rather than run an agent less governed than its config says.
///
/// **Shared by `nanny run` and `nanny run --serve` deliberately.** It lived
/// only in the former until 2026-08-29, which meant the fail-closed guarantee
/// held for local development and not for `--serve`: the shape every container
/// runs. An image missing its vendored pack booted and ran unguarded, silently,
/// because nothing else checks: the SDK loads whatever is on disk and carries on
/// when that is nothing. Same defect as `/rules` being registered on the socket
/// dispatch and missing from the network router, and the same fix: one
/// implementation both paths call.
pub fn resolve_declared_packs(
    config: &nanny_config::NannyConfig,
    config_dir: &Path,
) -> Result<Vec<nanny_config::pack::PackManifest>> {
    let pinned = config.rules.pinned()?;
    let packs = nanny_config::pack::load_declared_packs(config_dir, &pinned)?;
    if !packs.is_empty() {
        println!(
            "nanny: rule packs, {:?}",
            packs.iter().map(|p| p.slug()).collect::<Vec<_>>()
        );
    }
    Ok(packs)
}

fn build_governed_child(
    command: Vec<String>,
    server: &NetworkServerInfo,
) -> Result<(std::process::Command, String)> {
    let (program, args) = command.split_first().expect("command is non-empty");

    // Cert files from ~/.nanny/certs/: auto-injected if present.
    // Cross-machine deployments override these via NANNY_BRIDGE_CERT/KEY/CA.
    let certs_dir = dirs::home_dir()
        .context("cannot determine home directory")?
        .join(".nanny")
        .join("certs");

    // Each `nanny run` is its own run on the server: a stop ends this run, not
    // the server, so the host survives many sequential runs. Set
    // NANNY_RUN_ID yourself to make several processes share one run and stop
    // together (e.g. a fleet demo); otherwise each invocation gets a fresh id.
    let run_id = std::env::var("NANNY_RUN_ID")
        .ok()
        .filter(|s| !s.is_empty())
        .unwrap_or_else(nanny_config::new_run_id);

    let mut cmd = std::process::Command::new(program);
    cmd.args(args);
    cmd.env("NANNY_BRIDGE_ADDR", &server.addr);
    cmd.env("NANNY_SESSION_TOKEN", &server.token);
    cmd.env("NANNY_RUN_ID", &run_id);

    // Only inject cert paths that actually exist: agents on remote machines
    // may have already set these env vars themselves via their deployment config.
    let cert_file = certs_dir.join("client.crt");
    let key_file = certs_dir.join("client.key");
    let ca_file = certs_dir.join("ca.crt");
    if cert_file.exists() {
        cmd.env("NANNY_BRIDGE_CERT", &cert_file);
    }
    if key_file.exists() {
        cmd.env("NANNY_BRIDGE_KEY", &key_file);
    }
    if ca_file.exists() {
        cmd.env("NANNY_BRIDGE_CA", &ca_file);
    }

    Ok((cmd, run_id))
}

/// Tell the governor which app this run belongs to.
///
/// Done from the CLI rather than the child because an app that links no SDK
/// has no way to declare for itself. Borrows the SDK's own client
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


// ── Event constructors ────────────────────────────────────────────────────────


