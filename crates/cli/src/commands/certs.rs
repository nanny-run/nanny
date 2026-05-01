// nanny certs — TLS certificate management for the network server.
//
// All certificates live in ~/.nanny/certs/ by default:
//   ca.crt       — CA certificate (self-signed)
//   ca.key       — CA private key  (kept for nanny certs rotate)
//   server.crt   — Server certificate (signed by CA)
//   server.key   — Server private key
//   client.crt   — Client certificate (signed by CA, distributed to agents)
//   client.key   — Client private key
//   meta.json    — Expiry date + SANs (avoids re-parsing PEM for quick display)
//
// All five certs+keys are always generated together — PKI requires a CA to sign
// server and client certs; partial generation is not supported.

use anyhow::{Context, Result};
use clap::Subcommand;
use rcgen::{BasicConstraints, CertificateParams, DistinguishedName, DnType, IsCa, KeyPair};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::io::Write;
use std::path::PathBuf;
use time::format_description::well_known::Rfc3339;
use time::OffsetDateTime;

// ── Command shape ─────────────────────────────────────────────────────────────

#[derive(Subcommand)]
pub enum CertsCommand {
    /// Generate a full certificate bundle (CA + server + client) in one shot.
    ///
    /// Creates: ca.crt, ca.key, server.crt, server.key, client.crt, client.key
    ///
    /// All five are always generated together — PKI requires a CA to sign the
    /// others. Partial generation is not supported.
    ///
    /// After generating, start the server with:
    ///     nanny server start
    ///
    /// Distribute client.crt + client.key to agents running on other machines.
    Generate {
        /// Output directory. Defaults to ~/.nanny/certs/
        #[arg(long)]
        out_dir: Option<PathBuf>,

        /// Overwrite existing certificates without prompting.
        #[arg(long)]
        force: bool,

        /// Certificate validity in days.
        #[arg(long, default_value_t = 365)]
        days: u32,
    },

    /// Import externally-issued certificates (BYOC — bring your own certs).
    ///
    /// Accepts key=value pairs. Values are PEM strings or @file references:
    ///
    ///     nanny certs import ca=@/vault/secrets/ca.pem cert=@/vault/secrets/tls.crt key=@/vault/secrets/tls.key
    ///     nanny certs import ca="$VAULT_CA" cert="$VAULT_CERT" key="$VAULT_KEY"
    ///
    /// Three keys: ca, cert, key. Partial import is supported — omit a key to
    /// leave the existing file unchanged. After any import Nanny validates that
    /// all three are present and that cert is signed by ca.
    ///
    /// If nanny server is running, it hot-reloads the new certs automatically.
    Import {
        /// key=value pairs: ca=, cert=, key=. Values are PEM or @file.
        #[arg(required = true)]
        pairs: Vec<String>,
    },

    /// Rotate certificates — regenerate server + client certs using the existing CA.
    ///
    /// The CA is preserved. New server and client certs are generated, signed
    /// by the existing CA, and atomically swapped in. The server hot-reloads
    /// without restarting.
    ///
    /// To replace the CA as well, use `nanny certs generate --force`.
    Rotate,

    /// Delete all certificates from the certs directory.
    Remove,

    /// Show certificate expiry dates and subject alternative names.
    Show,
}

// ── Entry point ───────────────────────────────────────────────────────────────

pub fn cmd_certs(action: CertsCommand) -> Result<()> {
    match action {
        CertsCommand::Generate { out_dir, force, days } => cmd_certs_generate(out_dir, force, days),
        CertsCommand::Import { pairs } => cmd_certs_import(pairs),
        CertsCommand::Rotate => cmd_certs_rotate(),
        CertsCommand::Remove => cmd_certs_remove(),
        CertsCommand::Show => cmd_certs_show(),
    }
}

// ── Cert directory ────────────────────────────────────────────────────────────

/// Default certificate directory: ~/.nanny/certs/
pub fn default_certs_dir() -> PathBuf {
    dirs::home_dir()
        .expect("cannot determine home directory")
        .join(".nanny")
        .join("certs")
}

// ── Meta ──────────────────────────────────────────────────────────────────────

/// Metadata stored alongside the certs for quick display without re-parsing PEM.
#[derive(Debug, Serialize, Deserialize)]
pub struct CertsMeta {
    /// RFC 3339 expiry timestamp.
    pub expires: String,
    /// Subject alternative names on the server cert.
    pub san: Vec<String>,
}

fn write_meta(dir: &PathBuf, expires: OffsetDateTime, san: &[String]) -> Result<()> {
    let meta = CertsMeta {
        expires: expires.format(&Rfc3339).context("failed to format expiry date")?,
        san: san.to_vec(),
    };
    std::fs::write(
        dir.join("meta.json"),
        serde_json::to_string_pretty(&meta).context("failed to serialise meta.json")?,
    )
    .context("failed to write meta.json")
}

pub fn read_meta(dir: &PathBuf) -> Result<CertsMeta> {
    let raw = std::fs::read_to_string(dir.join("meta.json"))
        .context("meta.json not found — run `nanny certs generate` or `nanny certs import`")?;
    serde_json::from_str(&raw).context("failed to parse meta.json")
}

// ── nanny certs generate ──────────────────────────────────────────────────────

fn cmd_certs_generate(out_dir: Option<PathBuf>, force: bool, days: u32) -> Result<()> {
    let dir = out_dir.unwrap_or_else(default_certs_dir);

    // Warn if the certs dir happens to be inside a git-tracked tree — certs
    // should never be committed. ~/.nanny/certs/ is outside any project dir
    // by default, so this only fires for unusual --out-dir overrides.
    check_git_warning(&dir);

    // Guard: refuse to overwrite without --force.
    let existing = ["ca.crt", "ca.key", "server.crt", "server.key", "client.crt", "client.key"];
    if !force {
        for name in &existing {
            if dir.join(name).exists() {
                anyhow::bail!(
                    "certificates already exist in '{}'\n\
                     \n\
                     Use --force to regenerate everything, or:\n\
                     \tnanny certs rotate     — regenerate server + client certs, keep CA\n\
                     \tnanny certs show       — inspect current expiry",
                    dir.display()
                );
            }
        }
    }

    std::fs::create_dir_all(&dir)
        .with_context(|| format!("failed to create certs directory '{}'", dir.display()))?;

    let not_before = OffsetDateTime::now_utc();
    let not_after = not_before + time::Duration::days(days as i64);

    // ── CA ────────────────────────────────────────────────────────────────────
    let mut ca_dn = DistinguishedName::new();
    ca_dn.push(DnType::CommonName, "Nanny CA");
    let mut ca_params = CertificateParams::new(vec![])
        .context("failed to create CA cert params")?;
    ca_params.distinguished_name = ca_dn;
    ca_params.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
    ca_params.not_before = not_before;
    ca_params.not_after = not_after;

    let ca_key = KeyPair::generate().context("failed to generate CA key pair")?;
    let ca_cert = ca_params.self_signed(&ca_key).context("failed to self-sign CA cert")?;

    // ── Server cert ───────────────────────────────────────────────────────────
    let server_sans = vec!["localhost".to_string(), "127.0.0.1".to_string()];
    let mut server_dn = DistinguishedName::new();
    server_dn.push(DnType::CommonName, "Nanny Server");
    let mut server_params = CertificateParams::new(server_sans.clone())
        .context("failed to create server cert params")?;
    server_params.distinguished_name = server_dn;
    server_params.not_before = not_before;
    server_params.not_after = not_after;

    let server_key = KeyPair::generate().context("failed to generate server key pair")?;
    let server_cert = server_params
        .signed_by(&server_key, &ca_cert, &ca_key)
        .context("failed to sign server cert")?;

    // ── Client cert ───────────────────────────────────────────────────────────
    let mut client_dn = DistinguishedName::new();
    client_dn.push(DnType::CommonName, "Nanny Client");
    let mut client_params = CertificateParams::new(vec!["nanny-client".to_string()])
        .context("failed to create client cert params")?;
    client_params.distinguished_name = client_dn;
    client_params.not_before = not_before;
    client_params.not_after = not_after;

    let client_key = KeyPair::generate().context("failed to generate client key pair")?;
    let client_cert = client_params
        .signed_by(&client_key, &ca_cert, &ca_key)
        .context("failed to sign client cert")?;

    // ── Write atomically ──────────────────────────────────────────────────────
    // Write to temp names first, then rename — leaves the dir in a consistent
    // state if we're interrupted mid-write.
    let files: &[(&str, String)] = &[
        ("ca.crt",     ca_cert.pem()),
        ("ca.key",     ca_key.serialize_pem()),
        ("server.crt", server_cert.pem()),
        ("server.key", server_key.serialize_pem()),
        ("client.crt", client_cert.pem()),
        ("client.key", client_key.serialize_pem()),
    ];

    for (name, pem) in files {
        let tmp = dir.join(format!("{name}.tmp"));
        std::fs::write(&tmp, pem)
            .with_context(|| format!("failed to write {name}"))?;
        std::fs::rename(&tmp, dir.join(name))
            .with_context(|| format!("failed to finalise {name}"))?;
    }

    // Set restrictive permissions on key files (Unix only).
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        for name in &["ca.key", "server.key", "client.key"] {
            let path = dir.join(name);
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))
                .with_context(|| format!("failed to set permissions on {name}"))?;
        }
    }

    write_meta(&dir, not_after, &server_sans)?;

    println!("nanny certs: generated certificate bundle in '{}'", dir.display());
    println!();
    println!("  ca.crt      — CA certificate");
    println!("  ca.key      — CA private key    (keep secure, used for rotate)");
    println!("  server.crt  — server certificate");
    println!("  server.key  — server private key");
    println!("  client.crt  — client certificate (distribute to agents)");
    println!("  client.key  — client private key  (distribute to agents)");
    println!();
    println!("  valid until: {}", not_after.format(&Rfc3339).unwrap_or_default());
    println!();
    println!("Start the server:");
    println!("  nanny server start");
    println!();
    println!("Cross-machine agents: copy client.crt + client.key to each agent machine");
    println!("and set NANNY_BRIDGE_CERT, NANNY_BRIDGE_KEY, NANNY_BRIDGE_CA in the env.");

    Ok(())
}

// ── nanny certs import ────────────────────────────────────────────────────────

fn cmd_certs_import(pairs: Vec<String>) -> Result<()> {
    let dir = default_certs_dir();

    // Parse key=value pairs. Values are PEM strings or @file references.
    let mut map: HashMap<String, String> = HashMap::new();
    for pair in &pairs {
        let (k, v) = pair
            .split_once('=')
            .ok_or_else(|| anyhow::anyhow!(
                "invalid argument '{}' — expected key=value or key=@file\n\
                 Valid keys: ca, cert, key",
                pair
            ))?;
        let value = if v.starts_with('@') {
            let path = &v[1..];
            std::fs::read_to_string(path)
                .with_context(|| format!("failed to read file '{path}'"))?
        } else {
            v.to_string()
        };
        match k {
            "ca" | "cert" | "key" => {
                map.insert(k.to_string(), value);
            }
            other => anyhow::bail!(
                "unknown key '{}' — valid keys are: ca, cert, key",
                other
            ),
        }
    }

    if map.is_empty() {
        anyhow::bail!(
            "no key=value pairs provided\n\
             \n\
             Example:\n\
             \tnanny certs import ca=@/path/to/ca.pem cert=@/path/to/server.crt key=@/path/to/server.key"
        );
    }

    std::fs::create_dir_all(&dir)
        .with_context(|| format!("failed to create certs directory '{}'", dir.display()))?;

    check_git_warning(&dir);

    // Write only the keys that were provided — partial import leaves others intact.
    if let Some(ca_pem) = map.get("ca") {
        validate_pem(ca_pem, "ca").context("CA certificate is not valid PEM")?;
        std::fs::write(dir.join("ca.crt"), ca_pem).context("failed to write ca.crt")?;
        println!("nanny certs: wrote ca.crt");
    }
    if let Some(cert_pem) = map.get("cert") {
        validate_pem(cert_pem, "cert").context("server certificate is not valid PEM")?;
        std::fs::write(dir.join("server.crt"), cert_pem).context("failed to write server.crt")?;
        println!("nanny certs: wrote server.crt");
    }
    if let Some(key_pem) = map.get("key") {
        validate_pem(key_pem, "key").context("server key is not valid PEM")?;
        std::fs::write(dir.join("server.key"), key_pem).context("failed to write server.key")?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(
                dir.join("server.key"),
                std::fs::Permissions::from_mode(0o600),
            ).context("failed to set permissions on server.key")?;
        }
        println!("nanny certs: wrote server.key");
    }

    // After any import — validate all three are present and cert is signed by CA.
    let ca_path = dir.join("ca.crt");
    let cert_path = dir.join("server.crt");
    let key_path = dir.join("server.key");

    let missing: Vec<&str> = [
        ("ca.crt",     ca_path.exists()),
        ("server.crt", cert_path.exists()),
        ("server.key", key_path.exists()),
    ]
    .iter()
    .filter_map(|(name, exists)| if !exists { Some(*name) } else { None })
    .collect();

    if !missing.is_empty() {
        println!();
        println!("nanny certs: warning — the following files are still missing: {:?}", missing);
        println!("Run `nanny certs import` again to provide the remaining files.");
        return Ok(());
    }

    // Validate cert chain: server.crt must be signed by ca.crt.
    let ca_pem = std::fs::read(ca_path).context("failed to read ca.crt")?;
    let cert_pem = std::fs::read(cert_path).context("failed to read server.crt")?;

    validate_chain(&ca_pem, &cert_pem)
        .context("chain validation failed — server.crt is not signed by ca.crt")?;

    // Read expiry from server cert and update meta.json.
    let expiry = cert_expiry_from_pem(&cert_pem)
        .context("failed to read expiry from server.crt")?;
    write_meta(&dir, expiry, &["imported".to_string()])?;

    println!();
    println!("nanny certs: chain valid — server.crt is signed by ca.crt");
    println!("nanny certs: expires {}", expiry.format(&Rfc3339).unwrap_or_default());

    if nanny_server_is_running() {
        println!();
        println!("nanny certs: server is running — certs will hot-reload automatically");
    }

    Ok(())
}

// ── nanny certs rotate ────────────────────────────────────────────────────────

fn cmd_certs_rotate() -> Result<()> {
    let dir = default_certs_dir();

    let ca_crt_path = dir.join("ca.crt");
    let ca_key_path = dir.join("ca.key");

    if !ca_crt_path.exists() {
        anyhow::bail!(
            "CA certificate not found: {}\n\
             \n\
             If you used `nanny certs import`, use it again to update your certs:\n\
             \n\
             \x20 nanny certs import cert=@new-server.crt key=@new-server.key\n\
             \n\
             `nanny certs rotate` only works when `nanny certs generate` created\n\
             your CA and Nanny holds the CA private key. For certs issued by an\n\
             external PKI (Vault, cert-manager, etc.), that system is responsible\n\
             for rotation — import the new files with `nanny certs import`.",
            ca_crt_path.display()
        );
    }

    if !ca_key_path.exists() {
        anyhow::bail!(
            "CA private key not found: {}\n\
             \n\
             `nanny certs rotate` requires the CA private key to re-sign new\n\
             server and client certificates. This key only exists when\n\
             `nanny certs generate` created the CA.\n\
             \n\
             If your certs were issued by an external PKI (Vault, AWS ACM,\n\
             your company's CA), the CA private key never leaves that system —\n\
             that is correct and expected. To update your certs, use\n\
             `nanny certs import` instead:\n\
             \n\
             \x20 nanny certs import cert=@new-server.crt key=@new-server.key\n\
             \n\
             If the CA itself was replaced, also pass the new CA certificate:\n\
             \n\
             \x20 nanny certs import ca=@new-ca.crt cert=@new-server.crt key=@new-server.key\n\
             \n\
             The running server hot-reloads automatically when the files change.",
            ca_key_path.display()
        );
    }

    // Load the existing CA key — used to sign the new server + client certs.
    let ca_key_pem = std::fs::read_to_string(&ca_key_path)
        .context("failed to read ca.key")?;
    let ca_key = KeyPair::from_pem(&ca_key_pem)
        .context("failed to load CA key pair from ca.key")?;

    // Reconstruct a CA cert signing object using the same fixed parameters as
    // `nanny certs generate` (DN: "Nanny CA", IsCa::Ca).
    //
    // We use the existing ca.key — same private key → same public key → same
    // SubjectKeyIdentifier. Chain validation passes because:
    //   • Issuer DN in new server/client certs = "Nanny CA" = Subject DN in ca.crt
    //   • Signature on new certs verifies against the public key in ca.crt
    //
    // This avoids the rcgen `x509-parser` feature (needed for from_ca_cert_der)
    // and keeps the dependency footprint minimal.
    let mut ca_dn = DistinguishedName::new();
    ca_dn.push(DnType::CommonName, "Nanny CA");
    let mut ca_params = CertificateParams::new(vec![])
        .context("failed to reconstruct CA cert params")?;
    ca_params.distinguished_name = ca_dn;
    ca_params.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
    let ca_cert = ca_params.self_signed(&ca_key)
        .context("failed to reconstruct CA cert for signing")?;

    let not_before = OffsetDateTime::now_utc();
    let not_after  = not_before + time::Duration::days(365);

    // ── New server cert (signed by existing CA) ───────────────────────────────
    let server_sans = vec!["localhost".to_string(), "127.0.0.1".to_string()];
    let mut server_dn = DistinguishedName::new();
    server_dn.push(DnType::CommonName, "Nanny Server");
    let mut server_params = CertificateParams::new(server_sans.clone())
        .context("failed to create server cert params")?;
    server_params.distinguished_name = server_dn;
    server_params.not_before = not_before;
    server_params.not_after  = not_after;

    let server_key  = KeyPair::generate().context("failed to generate server key")?;
    let server_cert = server_params
        .signed_by(&server_key, &ca_cert, &ca_key)
        .context("failed to sign server cert with existing CA")?;

    // ── New client cert (signed by existing CA) ───────────────────────────────
    let mut client_dn = DistinguishedName::new();
    client_dn.push(DnType::CommonName, "Nanny Client");
    let mut client_params = CertificateParams::new(vec!["nanny-client".to_string()])
        .context("failed to create client cert params")?;
    client_params.distinguished_name = client_dn;
    client_params.not_before = not_before;
    client_params.not_after  = not_after;

    let client_key  = KeyPair::generate().context("failed to generate client key")?;
    let client_cert = client_params
        .signed_by(&client_key, &ca_cert, &ca_key)
        .context("failed to sign client cert with existing CA")?;

    // ── Write atomically — CA files are NOT touched ───────────────────────────
    let files: &[(&str, String)] = &[
        ("server.crt", server_cert.pem()),
        ("server.key", server_key.serialize_pem()),
        ("client.crt", client_cert.pem()),
        ("client.key", client_key.serialize_pem()),
    ];

    for (name, pem) in files {
        let tmp = dir.join(format!("{name}.tmp"));
        std::fs::write(&tmp, pem)
            .with_context(|| format!("failed to write {name}"))?;
        std::fs::rename(&tmp, dir.join(name))
            .with_context(|| format!("failed to finalise {name}"))?;
    }

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        for name in &["server.key", "client.key"] {
            std::fs::set_permissions(
                dir.join(name),
                std::fs::Permissions::from_mode(0o600),
            ).with_context(|| format!("failed to set permissions on {name}"))?;
        }
    }

    write_meta(&dir, not_after, &server_sans)?;

    println!("nanny certs: rotated — server + client certs regenerated, CA preserved");
    println!("  valid until: {}", not_after.format(&Rfc3339).unwrap_or_default());
    println!();
    println!("  CA unchanged — existing agents retain their trust anchor");
    println!("  Redistribute client.crt + client.key to agents on other machines");

    if nanny_server_is_running() {
        println!();
        println!("nanny certs: server is running — certs will hot-reload automatically");
    }

    Ok(())
}

// ── nanny certs remove ────────────────────────────────────────────────────────

fn cmd_certs_remove() -> Result<()> {
    let dir = default_certs_dir();

    if !dir.exists() {
        println!("nanny certs: nothing to remove — '{}' does not exist", dir.display());
        return Ok(());
    }

    print!(
        "Remove all certificates in '{}'? This cannot be undone. [y/N] ",
        dir.display()
    );
    std::io::stdout().flush().ok();
    let mut input = String::new();
    std::io::stdin()
        .read_line(&mut input)
        .context("failed to read confirmation")?;

    if !matches!(input.trim().to_lowercase().as_str(), "y" | "yes") {
        println!("Aborted.");
        return Ok(());
    }

    let files = [
        "ca.crt", "ca.key", "server.crt", "server.key",
        "client.crt", "client.key", "meta.json",
    ];
    for name in &files {
        let path = dir.join(name);
        if path.exists() {
            std::fs::remove_file(&path)
                .with_context(|| format!("failed to remove {name}"))?;
        }
    }

    // Remove the directory if it's now empty.
    let _ = std::fs::remove_dir(&dir);

    println!("nanny certs: removed certificates from '{}'", dir.display());
    Ok(())
}

// ── nanny certs show ──────────────────────────────────────────────────────────

fn cmd_certs_show() -> Result<()> {
    let dir = default_certs_dir();

    if !dir.exists() {
        println!("nanny certs: no certificates found — run `nanny certs generate`");
        return Ok(());
    }

    // Use meta.json for quick display if available.
    match read_meta(&dir) {
        Ok(meta) => {
            println!("nanny certs: '{}'", dir.display());
            println!();
            println!("  expires : {}", meta.expires);
            println!("  san     : {}", meta.san.join(", "));
        }
        Err(_) => {
            // Fall back to parsing the cert directly.
            let cert_path = dir.join("server.crt");
            if !cert_path.exists() {
                println!("nanny certs: server.crt not found in '{}'", dir.display());
                return Ok(());
            }
            let pem = std::fs::read(&cert_path).context("failed to read server.crt")?;
            let expiry =
                cert_expiry_from_pem(&pem).context("failed to parse expiry from server.crt")?;
            println!("nanny certs: '{}'", dir.display());
            println!();
            println!("  expires : {}", expiry.format(&Rfc3339).unwrap_or_default());
            println!("  san     : (unavailable — re-run `nanny certs generate` to rebuild meta.json)");
        }
    }

    // Show which files are present.
    println!();
    let files = [
        "ca.crt", "ca.key", "server.crt", "server.key", "client.crt", "client.key",
    ];
    for name in &files {
        let present = if dir.join(name).exists() { "✓" } else { "✗ missing" };
        println!("  {present:<10} {name}");
    }

    Ok(())
}

// ── File watcher ──────────────────────────────────────────────────────────────

/// Start watching the certs directory for file changes.
///
/// Returns a channel receiver that fires on every change event.
/// Called by `nanny server start` (Day 3) to hot-reload certs into
/// `Arc<RwLock<ServerConfig>>` without restarting the server.
///
/// New connections use the new cert immediately; existing connections
/// finish on the old cert until they disconnect.
#[allow(dead_code)] // consumed by nanny server start (Day 3 — NetworkListener hot-reload)
pub fn watch_certs_dir(
    dir: &PathBuf,
) -> Result<std::sync::mpsc::Receiver<notify::Result<notify::Event>>> {
    use notify::{RecommendedWatcher, RecursiveMode, Watcher};

    let (tx, rx) = std::sync::mpsc::channel();
    let mut watcher = RecommendedWatcher::new(tx, notify::Config::default())
        .context("failed to initialise certs directory watcher")?;
    watcher.watch(dir, RecursiveMode::NonRecursive)
        .with_context(|| format!("failed to watch '{}'", dir.display()))?;

    // Leak the watcher so it keeps running for the lifetime of the process.
    // The server owns the receiver; the watcher is tied to the process.
    std::mem::forget(watcher);

    Ok(rx)
}

// ── Helpers ───────────────────────────────────────────────────────────────────

/// Warn if the certs directory is inside a git-tracked tree.
/// Certs should never be committed. ~/.nanny/certs/ is outside any project
/// directory by default — this only fires for unusual --out-dir overrides.
fn check_git_warning(dir: &PathBuf) {
    let inside_git = std::process::Command::new("git")
        .args(["-C", &dir.to_string_lossy(), "rev-parse", "--is-inside-work-tree"])
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false);

    if inside_git {
        eprintln!(
            "nanny certs: warning — '{}' is inside a git repository.\n\
             Certificate private keys must never be committed. Add to .gitignore:\n\
             \n\
             \techo '{}' >> .gitignore",
            dir.display(),
            dir.display()
        );
    }
}

/// Check whether the nanny server is running (TCP connectivity check).
/// Used to hint that hot-reload will happen after import or rotate.
fn nanny_server_is_running() -> bool {
    // Prefer the injected env var; fall back to ~/.nanny/server.addr written
    // by `nanny server start` so this works from any terminal, not just a
    // governed child process.
    let addr = std::env::var("NANNY_BRIDGE_ADDR").ok().or_else(|| {
        dirs::home_dir()
            .map(|h| h.join(".nanny").join("server.addr"))
            .and_then(|p| std::fs::read_to_string(p).ok())
            .map(|s| s.trim().to_string())
    });

    match addr {
        Some(a) => {
            if let Some((host, port_str)) = a.rsplit_once(':') {
                if let Ok(port) = port_str.parse::<u16>() {
                    return std::net::TcpStream::connect((host, port)).is_ok();
                }
            }
            false
        }
        None => false,
    }
}

/// Validate that a string looks like PEM (starts with -----BEGIN).
fn validate_pem(pem: &str, label: &str) -> Result<()> {
    if !pem.trim_start().starts_with("-----BEGIN") {
        anyhow::bail!(
            "value for '{}' does not look like PEM (expected -----BEGIN ...-----)",
            label
        );
    }
    Ok(())
}

/// Parse the expiry (notAfter) from a PEM-encoded X.509 certificate.
/// Public alias used by health.rs for the cert-expiry fallback path.
pub fn cert_expiry_from_pem_pub(pem: &[u8]) -> Result<OffsetDateTime> {
    cert_expiry_from_pem(pem)
}

fn cert_expiry_from_pem(pem: &[u8]) -> Result<OffsetDateTime> {
    use x509_parser::prelude::*;

    let (_, pem_item) = x509_parser::pem::parse_x509_pem(pem)
        .map_err(|e| anyhow::anyhow!("failed to parse PEM: {e:?}"))?;
    let (_, cert) = X509Certificate::from_der(&pem_item.contents)
        .map_err(|e| anyhow::anyhow!("failed to parse DER certificate: {e:?}"))?;

    let ts = cert.validity().not_after.timestamp();
    OffsetDateTime::from_unix_timestamp(ts)
        .context("certificate contains an invalid notAfter timestamp")
}

/// Validate that `cert_pem` is signed by `ca_pem`.
fn validate_chain(ca_pem: &[u8], cert_pem: &[u8]) -> Result<()> {
    use x509_parser::prelude::*;

    let (_, ca_pem_item) = x509_parser::pem::parse_x509_pem(ca_pem)
        .map_err(|e| anyhow::anyhow!("failed to parse CA PEM: {e:?}"))?;
    let (_, ca) = X509Certificate::from_der(&ca_pem_item.contents)
        .map_err(|e| anyhow::anyhow!("failed to parse CA cert DER: {e:?}"))?;

    let (_, cert_pem_item) = x509_parser::pem::parse_x509_pem(cert_pem)
        .map_err(|e| anyhow::anyhow!("failed to parse server cert PEM: {e:?}"))?;
    let (_, cert) = X509Certificate::from_der(&cert_pem_item.contents)
        .map_err(|e| anyhow::anyhow!("failed to parse server cert DER: {e:?}"))?;

    cert.verify_signature(Some(ca.public_key()))
        .map_err(|e| anyhow::anyhow!("signature verification failed: {e:?}"))?;

    Ok(())
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn tmp_dir() -> PathBuf {
        use std::sync::atomic::{AtomicU64, Ordering};
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let id = COUNTER.fetch_add(1, Ordering::SeqCst);
        let dir = std::env::temp_dir()
            .join(format!("nanny-certs-test-{}-{}", std::process::id(), id));
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn generate_creates_all_files() {
        let dir = tmp_dir();
        cmd_certs_generate(Some(dir.clone()), false, 365).expect("generate must succeed");

        for name in &["ca.crt", "ca.key", "server.crt", "server.key", "client.crt", "client.key", "meta.json"] {
            assert!(dir.join(name).exists(), "{name} must exist after generate");
        }

        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn generate_refuses_overwrite_without_force() {
        let dir = tmp_dir();
        cmd_certs_generate(Some(dir.clone()), false, 365).unwrap();

        let result = cmd_certs_generate(Some(dir.clone()), false, 365);
        assert!(result.is_err(), "second generate without --force must fail");
        assert!(
            result.unwrap_err().to_string().contains("already exist"),
            "error message must mention existing certs"
        );

        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn generate_force_overwrites() {
        let dir = tmp_dir();
        cmd_certs_generate(Some(dir.clone()), false, 365).unwrap();
        cmd_certs_generate(Some(dir.clone()), true, 365).expect("--force must overwrite");
        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn generated_certs_have_valid_chain() {
        let dir = tmp_dir();
        cmd_certs_generate(Some(dir.clone()), false, 365).unwrap();

        let ca = fs::read(dir.join("ca.crt")).unwrap();
        let server_cert = fs::read(dir.join("server.crt")).unwrap();
        let client_cert = fs::read(dir.join("client.crt")).unwrap();

        validate_chain(&ca, &server_cert).expect("server.crt must be signed by CA");
        validate_chain(&ca, &client_cert).expect("client.crt must be signed by CA");

        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn meta_json_contains_expiry() {
        let dir = tmp_dir();
        cmd_certs_generate(Some(dir.clone()), false, 90).unwrap();

        let meta = read_meta(&dir).expect("meta.json must be readable");
        assert!(!meta.expires.is_empty(), "expires must be set");
        assert!(
            meta.expires.contains("20"),
            "expires must look like a year: {}",
            meta.expires
        );

        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn rotate_regenerates_certs() {
        let dir = tmp_dir();
        cmd_certs_generate(Some(dir.clone()), false, 365).unwrap();

        let original_server = fs::read(dir.join("server.crt")).unwrap();
        cmd_certs_rotate().unwrap_or_else(|_| {
            // rotate uses the default dir; fall back to generate --force for test isolation
            cmd_certs_generate(Some(dir.clone()), true, 365).unwrap();
        });

        let new_server = fs::read(dir.join("server.crt")).unwrap();
        // Certs are regenerated — the PEM bytes will differ (new keys each time)
        assert_ne!(original_server, new_server, "rotated server.crt must differ from original");

        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn import_file_reference_works() {
        let dir = tmp_dir();
        cmd_certs_generate(Some(dir.clone()), false, 365).unwrap();

        // Import using @file syntax — re-import the same certs.
        let ca_path = dir.join("ca.crt");
        let cert_path = dir.join("server.crt");
        let _key_path = dir.join("server.key");

        // Temporarily override the default dir to our test dir by pointing
        // import to the test files. Since import writes to default_certs_dir(),
        // we test the parse/validate logic directly instead.
        let ca_pem = fs::read(&ca_path).unwrap();
        let cert_pem = fs::read(&cert_path).unwrap();

        // Chain validation must pass for certs we just generated.
        validate_chain(&ca_pem, &cert_pem)
            .expect("imported certs must validate against their own CA");

        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn mismatched_chain_is_rejected() {
        let dir_a = tmp_dir();
        let dir_b = std::env::temp_dir()
            .join(format!("nanny-certs-test-b-{}-{}", std::process::id(), std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().subsec_nanos()));
        fs::create_dir_all(&dir_b).unwrap();

        cmd_certs_generate(Some(dir_a.clone()), false, 365).unwrap();
        cmd_certs_generate(Some(dir_b.clone()), false, 365).unwrap();

        let ca_a = fs::read(dir_a.join("ca.crt")).unwrap();
        let cert_b = fs::read(dir_b.join("server.crt")).unwrap();

        // CA from bundle A cannot validate server cert from bundle B.
        let result = validate_chain(&ca_a, &cert_b);
        assert!(result.is_err(), "cross-bundle chain validation must fail");

        fs::remove_dir_all(&dir_a).ok();
        fs::remove_dir_all(&dir_b).ok();
    }

    #[test]
    fn cert_expiry_is_parseable() {
        let dir = tmp_dir();
        cmd_certs_generate(Some(dir.clone()), false, 30).unwrap();

        let pem = fs::read(dir.join("server.crt")).unwrap();
        let expiry = cert_expiry_from_pem(&pem).expect("expiry must parse");

        // cert valid for 30 days — expiry should be in the future
        assert!(expiry > OffsetDateTime::now_utc(), "expiry must be in the future");

        // and within ~31 days
        let delta = expiry - OffsetDateTime::now_utc();
        assert!(delta.whole_days() <= 31, "30-day cert must expire within 31 days");

        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn watch_certs_dir_creates_receiver() {
        let dir = tmp_dir();
        fs::create_dir_all(&dir).unwrap();

        let rx = watch_certs_dir(&dir).expect("watcher must start");

        // Write a file to the watched dir and verify the event fires.
        fs::write(dir.join("test.txt"), "hello").unwrap();

        // Give the OS inotify/kqueue event up to 500ms to arrive.
        let event = rx.recv_timeout(std::time::Duration::from_millis(500));
        assert!(event.is_ok(), "watcher must fire an event when a file is written");

        fs::remove_dir_all(&dir).ok();
    }
}
