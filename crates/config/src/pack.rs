//! Rule pack format and resolution.
//!
//! A pack is a directory of rule code plus a manifest, vendored into the
//! project at `.nanny/rules/<name>@<version>/` and committed like any other
//! dependency. It is never fetched at run time: `nanny rules add` puts it on
//! disk, and from then on the engine only reads local files.
//!
//! That boundary is not a convenience. The engine must work with no network and
//! no company behind it, must not carry background updates or remote
//! dependencies, and must behave identically for identical inputs. A pack the
//! runtime downloaded would break all three, and a rule is a security control
//! whose failure is silent, so "it quietly fetched a different version" is the
//! worst available outcome.

use crate::ConfigError;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// Where packs are vendored, relative to the project root.
pub const PACK_DIR: &str = ".nanny/rules";

/// The manifest at the root of an installed pack.
///
/// Carries both language implementations rather than shipping one pack per
/// language. Two separately published packages are two implementations of the
/// same control that can drift, and a rule that means one thing in Python and
/// another in Rust is worse than no shared rule at all.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct PackManifest {
    /// Pack identifier, e.g. `nanny:owasp`.
    pub name: String,
    /// Exact version this directory contains.
    pub version: String,
    /// One-line description, shown by `nanny rules list`.
    #[serde(default)]
    pub description: String,
    /// Rule names this pack registers, for display and for verifying that what
    /// loaded matches what was declared.
    #[serde(default)]
    pub rules: Vec<String>,
    /// Relative path to the Python implementation, if the pack ships one.
    #[serde(default)]
    pub python: Option<String>,
    /// Relative path to the Rust implementation, if the pack ships one.
    #[serde(default)]
    pub rust: Option<String>,
    /// Detached signature over the pack contents, verified at install time.
    ///
    /// Absent for a pack installed from a local path during development.
    /// Verification happens in `nanny rules add`, never here: checking a
    /// signature during enforcement would need trust roots and possibly a
    /// network call on the path that must stay deterministic and offline.
    #[serde(default)]
    pub signature: Option<String>,
}

impl PackManifest {
    /// `name@version`, the form used in `[rules] extends` and on disk.
    pub fn slug(&self) -> String {
        format!("{}@{}", self.name, self.version)
    }
}

/// Directory name for a pack on disk.
///
/// `:` is legal in a pack name and hostile in a path, so it is folded to `-`.
/// The manifest inside still carries the true name, which is what everything
/// downstream reads.
pub fn pack_dir_name(name: &str, version: &str) -> String {
    format!("{}@{}", name.replace(':', "-"), version)
}

/// Resolve one declared pack to its manifest on disk.
///
/// Fails closed. A pack named in configuration but absent from disk is not a
/// warning: configuration is the source of truth, so the operator has declared
/// controls that are not present, and starting anyway would run an agent the
/// operator believes is governed when it is not.
pub fn load_pack(
    project_root: &Path,
    name: &str,
    version: &str,
) -> Result<PackManifest, ConfigError> {
    let dir = project_root
        .join(PACK_DIR)
        .join(pack_dir_name(name, version));
    let manifest_path = dir.join("pack.toml");

    let raw =
        std::fs::read_to_string(&manifest_path).map_err(|_| ConfigError::RulePackMissing {
            name: name.to_string(),
            version: version.to_string(),
            path: manifest_path.display().to_string(),
        })?;

    let manifest: PackManifest =
        toml::from_str(&raw).map_err(|e| ConfigError::Parse(format!("{manifest_path:?}: {e}")))?;

    if manifest.name != name || manifest.version != version {
        return Err(ConfigError::Parse(format!(
            "pack at '{}' declares itself as '{}' but was installed as '{}@{}'",
            dir.display(),
            manifest.slug(),
            name,
            version
        )));
    }

    Ok(manifest)
}

/// Resolve every pack named in `[rules] extends`.
pub fn load_declared_packs(
    project_root: &Path,
    extends: &[(String, String)],
) -> Result<Vec<PackManifest>, ConfigError> {
    extends
        .iter()
        .map(|(name, version)| load_pack(project_root, name, version))
        .collect()
}

/// Absolute path to an installed pack's directory.
pub fn pack_path(project_root: &Path, name: &str, version: &str) -> PathBuf {
    project_root
        .join(PACK_DIR)
        .join(pack_dir_name(name, version))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn install(root: &Path, name: &str, version: &str, body: &str) {
        let dir = root.join(PACK_DIR).join(pack_dir_name(name, version));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("pack.toml"), body).unwrap();
    }

    fn temp_root(tag: &str) -> PathBuf {
        let ts = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir = std::env::temp_dir().join(format!("nanny_pack_{tag}_{ts}"));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn an_installed_pack_loads() {
        let root = temp_root("ok");
        install(
            &root,
            "nanny:owasp",
            "2.1.0",
            r#"
name = "nanny:owasp"
version = "2.1.0"
description = "OWASP agentic top ten"
rules = ["no_send_after_read"]
python = "python/rules.py"
"#,
        );

        let m = load_pack(&root, "nanny:owasp", "2.1.0").unwrap();
        assert_eq!(m.slug(), "nanny:owasp@2.1.0");
        assert_eq!(m.rules, vec!["no_send_after_read"]);
        assert_eq!(m.python.as_deref(), Some("python/rules.py"));
    }

    #[test]
    fn a_declared_but_uninstalled_pack_fails_closed() {
        // Configuration is the source of truth. If it names a control that is
        // not present, the agent is not governed the way the operator believes,
        // and starting anyway is the one outcome worse than refusing.
        let root = temp_root("missing");
        let err = load_pack(&root, "nanny:owasp", "2.1.0").unwrap_err();
        assert!(matches!(err, ConfigError::RulePackMissing { .. }));
        assert!(err
            .to_string()
            .contains("nanny rules add nanny:owasp@2.1.0"));
    }

    #[test]
    fn a_pack_that_lies_about_its_identity_is_rejected() {
        // The directory says one version, the manifest another. Trusting either
        // would mean evidence attributed to a version that never ran.
        let root = temp_root("mismatch");
        install(
            &root,
            "nanny:owasp",
            "2.1.0",
            "name = \"nanny:owasp\"\nversion = \"9.9.9\"\n",
        );
        let err = load_pack(&root, "nanny:owasp", "2.1.0").unwrap_err();
        assert!(err.to_string().contains("declares itself as"));
    }

    #[test]
    fn a_colon_in_a_name_never_reaches_the_filesystem() {
        assert_eq!(pack_dir_name("nanny:owasp", "2.1.0"), "nanny-owasp@2.1.0");
        assert_eq!(
            pack_dir_name("acme:internal:fraud", "0.3.1"),
            "acme-internal-fraud@0.3.1"
        );
    }

    #[test]
    fn loading_several_packs_reports_the_first_missing_one() {
        let root = temp_root("several");
        install(&root, "a", "1.0.0", "name = \"a\"\nversion = \"1.0.0\"\n");
        let err = load_declared_packs(
            &root,
            &[("a".into(), "1.0.0".into()), ("b".into(), "2.0.0".into())],
        )
        .unwrap_err();
        assert!(err.to_string().contains("'b@2.0.0'"));
    }
}

#[cfg(test)]
mod manifesto_guard {
    //! The engine must not carry remote dependencies, must work with no company
    //! behind it, and must behave identically for identical inputs. Pack
    //! resolution runs on the enforcement path, so it has to be structurally
    //! incapable of reaching the network, not merely written so that it does
    //! not today.
    //!
    //! `nanny-config` owns pack resolution and links no HTTP client, no TLS
    //! stack, and no async runtime. That is the guarantee, and this test fails
    //! the moment someone adds one.

    #[test]
    fn pack_resolution_cannot_reach_the_network() {
        let manifest = include_str!("../Cargo.toml");
        for forbidden in [
            "reqwest",
            "hyper",
            "ureq",
            "curl",
            "tokio",
            "rustls",
            "async-std",
        ] {
            assert!(
                !manifest.contains(forbidden),
                "nanny-config gained a dependency on `{forbidden}`. Pack resolution \
                 runs during enforcement, and a crate that can open a socket there \
                 breaks the offline guarantee, the ban on remote dependencies, and \
                 determinism. Install-time fetching belongs in the CLI."
            );
        }
    }

    #[test]
    fn resolution_reads_only_the_project_directory() {
        // A pack outside the project root is not resolvable: the vendored copy
        // under `.nanny/rules/` is the only thing that can govern a run, so what
        // ships in the repository is exactly what enforces.
        let root = std::env::temp_dir().join("nanny_scope_guard_root");
        let _ = std::fs::create_dir_all(&root);
        let resolved = super::pack_path(&root, "nanny:owasp", "2.1.0");
        assert!(
            resolved.starts_with(&root),
            "a pack must resolve inside the project, got {}",
            resolved.display()
        );
    }
}
