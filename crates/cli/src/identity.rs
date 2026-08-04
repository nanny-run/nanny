//! `.nanny/app.toml` — an app's permanent identity, created once by `nanny init`.
//!
//! One app, one id, forever. `nanny init` is a per-app, once-ever action — the
//! `git init` analogy — never re-run for "the same app" in a new environment.
//! Whatever it produces here is what gets deployed everywhere that app runs
//! (laptop, VPS, CI), byte-for-byte, unmodified. There is no `--force`: identity
//! is never regenerated. A genuinely different app is a genuinely different
//! `nanny init` in a genuinely different checkout.
//!
//! Committed to git, not gitignored — an app id is not a secret (unlike a
//! session token or an ingest key), and committing it is what makes "init once,
//! deploy as-is" work without any manual id-copying step between environments.

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

const DIR_NAME: &str = ".nanny";
const FILE_NAME: &str = "app.toml";

/// An app's permanent identity. `app_id` is generated once and never changes —
/// it's the only thing ever used for addressing (`--join`, `--app`, Cloud
/// linking). `name` is purely for humans: easier to recognize than a hash in
/// `nanny status` output, never used to look anything up, and — unlike
/// `app_id` — free to edit by hand at any time.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AppIdentity {
    pub app_id: String,
    pub name: String,
}

impl AppIdentity {
    /// Look for `.nanny/app.toml` under `dir`. `Ok(None)` means this directory
    /// was never `nanny init`-ed with identity — a normal state for older
    /// projects, not an error.
    pub fn load(dir: &Path) -> Result<Option<Self>> {
        let path = app_toml_path(dir);
        if !path.exists() {
            return Ok(None);
        }
        let contents = std::fs::read_to_string(&path)
            .with_context(|| format!("failed to read {}", path.display()))?;
        let identity: AppIdentity = toml::from_str(&contents)
            .with_context(|| format!("failed to parse {}", path.display()))?;
        Ok(Some(identity))
    }

    /// Load identity from the current directory, or fail with a clear pointer
    /// to `nanny init` — used by commands that require an app id to function
    /// (`--serve`, self-minting) rather than treating it as optional.
    pub fn load_required(dir: &Path) -> Result<Self> {
        Self::load(dir)?.ok_or_else(|| {
            anyhow::anyhow!(
                "no app identity found in '{}' — run `nanny init` here first \
                 (writes .nanny/app.toml, once, permanently)",
                dir.display()
            )
        })
    }

    /// Create and persist a new identity. Fails if one already exists — the
    /// `app_id` is never regenerated; there is no `--force`. `name` is the
    /// caller's job to resolve (prompt, default, whatever) — this function
    /// just persists it, and never validates it for uniqueness, since it's
    /// purely a display label.
    pub fn create(dir: &Path, name: String) -> Result<Self> {
        let path = app_toml_path(dir);
        if path.exists() {
            anyhow::bail!(
                "{} already exists — an app's identity is set once and never \
                 regenerated. If this is genuinely a different app, run `nanny init` \
                 in a different directory instead.",
                path.display()
            );
        }
        let identity = AppIdentity { app_id: generate_id(), name };
        let dot_nanny = dir.join(DIR_NAME);
        std::fs::create_dir_all(&dot_nanny)
            .with_context(|| format!("failed to create {}", dot_nanny.display()))?;
        let body = toml::to_string_pretty(&identity).context("failed to serialize app identity")?;
        let commented = format!(
            "# Written once by `nanny init`. app_id is permanent — never edit or\n\
             # regenerate it; it's the only thing used to address this app\n\
             # (--join, --app, cloud linking). `name` is just for you — edit it\n\
             # freely, it's never used to look anything up. Commit this file: an\n\
             # app id is not a secret, and committing it is what lets `nanny init`\n\
             # stay a one-time, local action — deploy this file as-is to every\n\
             # environment this app runs in.\n\
             {body}"
        );
        std::fs::write(&path, commented).with_context(|| format!("failed to write {}", path.display()))?;
        Ok(identity)
    }
}

fn app_toml_path(dir: &Path) -> PathBuf {
    dir.join(DIR_NAME).join(FILE_NAME)
}

fn generate_id() -> String {
    format!("app_{}", uuid::Uuid::new_v4().simple())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch_dir() -> PathBuf {
        std::env::temp_dir().join(format!("nanny-identity-test-{}", uuid::Uuid::new_v4()))
    }

    #[test]
    fn missing_identity_loads_none() {
        let dir = scratch_dir();
        std::fs::create_dir_all(&dir).unwrap();
        assert!(AppIdentity::load(&dir).expect("missing is not an error").is_none());
    }

    #[test]
    fn create_then_load_round_trips() {
        let dir = scratch_dir();
        std::fs::create_dir_all(&dir).unwrap();
        let created = AppIdentity::create(&dir, "test-app".to_string()).expect("create");
        let loaded = AppIdentity::load(&dir).expect("load").expect("present");
        assert_eq!(created.app_id, loaded.app_id);
        assert_eq!(loaded.name, "test-app");
        assert!(loaded.app_id.starts_with("app_"));
    }

    #[test]
    fn create_twice_fails_no_force() {
        let dir = scratch_dir();
        std::fs::create_dir_all(&dir).unwrap();
        AppIdentity::create(&dir, "test-app".to_string()).expect("first create");
        let second = AppIdentity::create(&dir, "test-app".to_string());
        assert!(second.is_err(), "identity must never be regenerated");
    }

    #[test]
    fn load_required_errors_with_init_pointer() {
        let dir = scratch_dir();
        std::fs::create_dir_all(&dir).unwrap();
        let err = AppIdentity::load_required(&dir).unwrap_err();
        assert!(err.to_string().contains("nanny init"));
    }

    #[test]
    fn ids_are_unique_across_creates() {
        let dir1 = scratch_dir();
        let dir2 = scratch_dir();
        std::fs::create_dir_all(&dir1).unwrap();
        std::fs::create_dir_all(&dir2).unwrap();
        let a = AppIdentity::create(&dir1, "app-one".to_string()).unwrap();
        let b = AppIdentity::create(&dir2, "app-two".to_string()).unwrap();
        assert_ne!(a.app_id, b.app_id);
    }
}
