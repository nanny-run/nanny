//! `~/.nanny/credentials.toml` — the local cloud credential written by
//! `nanny auth login`.
//!
//! One credential per machine: `{ api_key, env }`. Never the repo `nanny.toml`
//! (a secret must not be committable). The key already belongs to exactly one
//! org on the cloud side, so nothing here names an org. Only `nanny auth`
//! (login/logout) and the sync gate touch this file; it is off the enforcement
//! path entirely — the engine never depends on it.

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

use crate::cloud::CloudEnv;

const FILE_NAME: &str = "credentials.toml";

/// The whole credential file. One machine, one credential.
#[derive(Clone, Serialize, Deserialize)]
pub struct Credentials {
    /// The ingest-only API key. Authenticates event forwarding; it identifies
    /// its own org on the cloud side, so no org is stored alongside it.
    pub api_key: String,
    /// Which cloud this key belongs to. The ingest URL is derived from this
    /// (`env.ingest_url()`), so no URL is ever stored or trusted.
    pub env: CloudEnv,
}

/// Redact the key so it never lands in logs, panic output, or `{:?}` dumps.
impl std::fmt::Debug for Credentials {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Credentials")
            .field("api_key", &"[redacted]")
            .field("env", &self.env)
            .finish()
    }
}

impl Credentials {
    /// Load `~/.nanny/credentials.toml`. `Ok(None)` means "not logged in" (the
    /// file is absent), which is a normal state, not an error.
    pub fn load() -> Result<Option<Self>> {
        Self::load_from(&nanny_dir()?)
    }

    /// Persist to `~/.nanny/credentials.toml` with `0600` permissions (dir `0700`).
    pub fn save(&self) -> Result<()> {
        self.save_to(&nanny_dir()?)
    }

    /// Delete the credential (log out this machine). Returns whether it existed;
    /// a missing file is not an error.
    pub fn delete() -> Result<bool> {
        Self::delete_from(&nanny_dir()?)
    }

    // ── Testable IO seam ───────────────────────────────────────────────────
    // The directory is injected so tests never read or write the real home.

    fn load_from(dir: &Path) -> Result<Option<Self>> {
        let path = dir.join(FILE_NAME);
        if !path.exists() {
            return Ok(None);
        }
        let contents = std::fs::read_to_string(&path)
            .with_context(|| format!("failed to read {}", path.display()))?;
        let creds =
            toml::from_str(&contents).with_context(|| format!("failed to parse {}", path.display()))?;
        Ok(Some(creds))
    }

    fn save_to(&self, dir: &Path) -> Result<()> {
        std::fs::create_dir_all(dir)
            .with_context(|| format!("failed to create {}", dir.display()))?;
        harden_dir(dir);
        let path = dir.join(FILE_NAME);
        let body = toml::to_string_pretty(self).context("failed to serialize credentials")?;
        std::fs::write(&path, body).with_context(|| format!("failed to write {}", path.display()))?;
        harden_file(&path);
        Ok(())
    }

    fn delete_from(dir: &Path) -> Result<bool> {
        let path = dir.join(FILE_NAME);
        if !path.exists() {
            return Ok(false);
        }
        std::fs::remove_file(&path)
            .with_context(|| format!("failed to remove {}", path.display()))?;
        Ok(true)
    }
}

/// `~/.nanny`, where the server token and certs already live.
fn nanny_dir() -> Result<PathBuf> {
    Ok(dirs::home_dir()
        .context("cannot determine home directory")?
        .join(".nanny"))
}

#[cfg(unix)]
fn harden_file(path: &Path) {
    use std::os::unix::fs::PermissionsExt;
    let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600));
}
#[cfg(unix)]
fn harden_dir(path: &Path) {
    use std::os::unix::fs::PermissionsExt;
    let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700));
}
#[cfg(not(unix))]
fn harden_file(_: &Path) {}
#[cfg(not(unix))]
fn harden_dir(_: &Path) {}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch_dir() -> PathBuf {
        std::env::temp_dir().join(format!("nanny-cred-test-{}", uuid::Uuid::new_v4()))
    }

    fn cred() -> Credentials {
        Credentials { api_key: "nny_secret".to_string(), env: CloudEnv::Staging }
    }

    #[test]
    fn missing_file_loads_none() {
        let dir = scratch_dir();
        assert!(Credentials::load_from(&dir).expect("missing is not an error").is_none());
    }

    #[test]
    fn round_trips_through_disk() {
        let dir = scratch_dir();
        cred().save_to(&dir).expect("save");
        let loaded = Credentials::load_from(&dir).expect("load").expect("present");
        assert_eq!(loaded.api_key, "nny_secret");
        assert_eq!(loaded.env, CloudEnv::Staging);
    }

    #[test]
    fn delete_removes_and_reports() {
        let dir = scratch_dir();
        cred().save_to(&dir).expect("save");
        assert!(Credentials::delete_from(&dir).expect("delete"));
        assert!(Credentials::load_from(&dir).expect("load").is_none());
        assert!(!Credentials::delete_from(&dir).expect("delete again"), "absent → false");
    }

    #[test]
    fn debug_never_leaks_the_key() {
        let rendered = format!("{:?}", cred());
        assert!(!rendered.contains("nny_secret"), "Debug must redact the key");
        assert!(rendered.contains("[redacted]"));
    }

    #[cfg(unix)]
    #[test]
    fn saved_file_is_0600() {
        use std::os::unix::fs::PermissionsExt;
        let dir = scratch_dir();
        cred().save_to(&dir).expect("save");
        let mode = std::fs::metadata(dir.join(FILE_NAME)).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o600, "a file holding a live key must not be group/world readable");
    }
}
