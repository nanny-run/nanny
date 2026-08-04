//! `.nanny/credentials.local.toml`, one app's own Cloud ingest credential.
//!
//! Unlike `.nanny/app.toml` (permanent identity, committed), this is a real
//! secret and must never be committed, gitignored, one file per app checkout.
//! This is what makes app isolation real: two unrelated apps on one machine no
//! longer share a single Cloud identity the way the old machine-wide
//! `~/.nanny/credentials.toml` did.
//!
//! Not minted by `nanny init` (that runs once, ever, on one machine, a VPS
//! that never runs `init` would never get a credential if minting lived there).
//! Minted per machine, by `nanny run`, the same category of action as
//! `nanny auth login`, see `maybe_self_mint`.

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::time::Duration;

use crate::cloud::CloudEnv;
use crate::credentials::Credentials;

const DIR_NAME: &str = ".nanny";
const FILE_NAME: &str = "credentials.local.toml";
const GITIGNORE_LINE: &str = ".nanny/credentials.local.toml";

/// One app's own ingest credential, scoped to a single app id on the cloud
/// side (once Cloud understands `app_id`; until then this degrades gracefully
/// to an org-wide key, see `maybe_self_mint`).
#[derive(Clone, Serialize, Deserialize)]
pub struct AppCredentials {
    pub api_key: String,
    pub env: CloudEnv,
}

impl std::fmt::Debug for AppCredentials {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AppCredentials")
            .field("api_key", &"[redacted]")
            .field("env", &self.env)
            .finish()
    }
}

impl AppCredentials {
    pub fn load(dir: &Path) -> Result<Option<Self>> {
        let path = creds_path(dir);
        if !path.exists() {
            return Ok(None);
        }
        let contents = std::fs::read_to_string(&path)
            .with_context(|| format!("failed to read {}", path.display()))?;
        let creds = toml::from_str(&contents)
            .with_context(|| format!("failed to parse {}", path.display()))?;
        Ok(Some(creds))
    }

    pub fn save(&self, dir: &Path) -> Result<()> {
        let dot_nanny = dir.join(DIR_NAME);
        std::fs::create_dir_all(&dot_nanny)
            .with_context(|| format!("failed to create {}", dot_nanny.display()))?;
        let path = creds_path(dir);
        let body = toml::to_string_pretty(self).context("failed to serialize app credentials")?;
        std::fs::write(&path, body).with_context(|| format!("failed to write {}", path.display()))?;
        harden_file(&path);
        ensure_gitignored(dir);
        Ok(())
    }
}

fn creds_path(dir: &Path) -> PathBuf {
    dir.join(DIR_NAME).join(FILE_NAME)
}

/// Best-effort: append the credential file to `.gitignore` if it isn't already
/// covered. Never fails the caller, a missed gitignore entry is a nudge, not
/// a hard requirement (the file's own 0600 perms are the real protection).
fn ensure_gitignored(dir: &Path) {
    let path = dir.join(".gitignore");
    let existing = std::fs::read_to_string(&path).unwrap_or_default();
    if existing.lines().any(|l| l.trim() == GITIGNORE_LINE || l.trim() == ".nanny/credentials.local.toml") {
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

#[cfg(unix)]
fn harden_file(path: &Path) {
    use std::os::unix::fs::PermissionsExt;
    let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600));
}
#[cfg(not(unix))]
fn harden_file(_: &Path) {}

// ── Self-minting ────────────────────────────────────────────────────────────

/// If no app-scoped credential exists locally yet, and this machine has a
/// login credential (`~/.nanny/credentials.toml`, from `nanny auth login`),
/// silently mint an app-scoped key using it and save it here, no browser, no
/// manual step, so there is never a window where a run syncs under an
/// unscoped, ambiguous-provenance credential once an app has identity.
///
/// Best-effort: any failure (no login, network error, non-2xx) just means no
/// app-scoped credential exists yet, the caller falls back to "don't sync
/// this run", never to a less-scoped credential. A run must never be blocked
/// or slowed meaningfully by this; the request has a short timeout.
pub fn maybe_self_mint(dir: &Path, app_id: &str) -> Option<AppCredentials> {
    if let Ok(Some(existing)) = AppCredentials::load(dir) {
        return Some(existing);
    }

    let machine = Credentials::load().ok().flatten()?;
    let environment = std::env::var("NANNY_ENVIRONMENT").ok().filter(|s| !s.is_empty());

    match mint(machine.env, &machine.api_key, app_id, environment.as_deref()) {
        Ok(minted) => {
            let creds = AppCredentials { api_key: minted, env: machine.env };
            if let Err(e) = creds.save(dir) {
                eprintln!("nanny: minted an app-scoped cloud key but failed to save it: {e:#}");
                return None;
            }
            println!("nanny: minted an app-scoped Nanny Cloud credential for this app");
            Some(creds)
        }
        Err(_) => {
            // Silent: this fires on every `nanny run` on a machine that's
            // logged in but whose Cloud doesn't yet support app-scoped
            // minting (or is briefly unreachable), not worth a warning on
            // every run. `resolve_sync`'s own "not syncing" message covers it.
            None
        }
    }
}

#[derive(Deserialize)]
struct MintResp {
    api_key: String,
}

/// Manual JSON in/out (no reqwest `json` feature enabled in this workspace,
/// matching `commands/auth.rs`'s existing pattern) rather than pulling in a new
/// Cargo feature for one call site.
fn mint(env: CloudEnv, bearer_key: &str, app_id: &str, environment: Option<&str>) -> Result<String> {
    let client = reqwest::blocking::Client::builder()
        .connect_timeout(Duration::from_secs(3))
        .timeout(Duration::from_secs(8))
        .build()
        .context("failed to build HTTP client")?;

    let url = format!("{}/v1/cli/key", env.api_base());
    let body = serde_json::to_string(&serde_json::json!({
        "app_id": app_id,
        "environment": environment,
    }))
    .context("failed to serialize mint request body")?;

    let resp = client
        .post(&url)
        .header("X-Nanny-Key", bearer_key)
        .header("Content-Type", "application/json")
        .body(body)
        .send()
        .with_context(|| format!("request to {url} failed"))?;

    let status = resp.status();
    let text = resp.text().unwrap_or_default();
    if !status.is_success() {
        anyhow::bail!("mint request returned {status}: {text}");
    }

    let parsed: MintResp =
        serde_json::from_str(&text).with_context(|| format!("unexpected response from {url}: {text}"))?;
    Ok(parsed.api_key)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch_dir() -> PathBuf {
        std::env::temp_dir().join(format!("nanny-app-credentials-test-{}", uuid::Uuid::new_v4()))
    }

    #[test]
    fn save_then_load_round_trips() {
        let dir = scratch_dir();
        std::fs::create_dir_all(&dir).unwrap();
        let creds = AppCredentials { api_key: "nny_test_key".to_string(), env: CloudEnv::Prod };
        creds.save(&dir).unwrap();

        let loaded = AppCredentials::load(&dir).unwrap().expect("just-saved credentials must load");
        assert_eq!(loaded.api_key, "nny_test_key");
        assert_eq!(loaded.env, CloudEnv::Prod);
    }

    #[test]
    fn missing_credentials_load_none() {
        let dir = scratch_dir();
        std::fs::create_dir_all(&dir).unwrap();
        assert!(AppCredentials::load(&dir).unwrap().is_none());
    }

    #[test]
    #[cfg(unix)]
    fn saved_credentials_file_is_owner_only() {
        use std::os::unix::fs::PermissionsExt;
        let dir = scratch_dir();
        std::fs::create_dir_all(&dir).unwrap();
        let creds = AppCredentials { api_key: "nny_test_key".to_string(), env: CloudEnv::Prod };
        creds.save(&dir).unwrap();

        let mode = std::fs::metadata(creds_path(&dir)).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o600, "credential file must be readable only by its owner");
    }

    #[test]
    fn save_appends_gitignore_entry_once() {
        let dir = scratch_dir();
        std::fs::create_dir_all(&dir).unwrap();
        let creds = AppCredentials { api_key: "nny_test_key".to_string(), env: CloudEnv::Prod };

        creds.save(&dir).unwrap();
        creds.save(&dir).unwrap(); // saving twice must not duplicate the line

        let gitignore = std::fs::read_to_string(dir.join(".gitignore")).unwrap();
        let count = gitignore.lines().filter(|l| l.trim() == GITIGNORE_LINE).count();
        assert_eq!(count, 1, "gitignore entry must appear exactly once, got: {gitignore:?}");
    }

    #[test]
    fn self_mint_returns_existing_credential_without_reminting() {
        // The early-return branch: an app-scoped credential already on disk
        // must short-circuit before any network call is attempted.
        let dir = scratch_dir();
        std::fs::create_dir_all(&dir).unwrap();
        let creds = AppCredentials { api_key: "nny_already_minted".to_string(), env: CloudEnv::Staging };
        creds.save(&dir).unwrap();

        let result = maybe_self_mint(&dir, "app_whatever").expect("existing credential must be returned");
        assert_eq!(result.api_key, "nny_already_minted");
    }
}
