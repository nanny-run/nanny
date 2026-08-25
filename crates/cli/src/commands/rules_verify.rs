//! Install-time integrity checking for rule packs.
//!
//! **Install time only, and that placement is the point.** A rule is a security
//! control that runs inside the agent's process; a compromised one fails
//! *silent*, returning allow forever while everything downstream stays green.
//! That is strictly worse than a library failing loud, so a pack's contents are
//! checked before they are ever on the enforcement path.
//!
//! Checking during a run instead would put trust roots, and possibly a network
//! call, on the path that must stay offline, deterministic, and free of remote
//! dependencies. Once a pack is installed it is ordinary code on disk, and what
//! guards it from there is the customer's own review and version control.

use anyhow::{bail, Result};
use nanny_config::pack::PackManifest;
use sha2::{Digest, Sha256};
use std::path::Path;

/// Digest of every file in the pack except the manifest itself.
///
/// Walked in sorted order so the digest is a property of the contents rather
/// than of the filesystem's iteration order, and paths are folded in alongside
/// bytes so that moving a file changes the result.
pub fn content_digest(dir: &Path) -> Result<String> {
    let mut files = Vec::new();
    collect(dir, dir, &mut files)?;
    files.sort();

    let mut hasher = Sha256::new();
    for rel in &files {
        hasher.update(rel.as_bytes());
        hasher.update([0u8]);
        hasher.update(std::fs::read(dir.join(rel))?);
        hasher.update([0u8]);
    }
    Ok(format!("{:x}", hasher.finalize()))
}

fn collect(root: &Path, dir: &Path, out: &mut Vec<String>) -> Result<()> {
    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        let path = entry.path();
        if entry.file_type()?.is_dir() {
            collect(root, &path, out)?;
        } else {
            let rel = path.strip_prefix(root).unwrap_or(&path).to_string_lossy().replace('\\', "/");
            if rel != "pack.toml" {
                out.push(rel);
            }
        }
    }
    Ok(())
}

/// Verify a pack before it is copied into the project.
///
/// A pack with no `signature` is accepted and reported as unsigned. That is the
/// honest outcome rather than a refusal: first-party packs are not published
/// yet, and a developer building one locally has nothing to sign with. What
/// must never happen is an unsigned pack being *described* as signed, so
/// `nanny rules list` prints the distinction on every line.
pub fn verify(dir: &Path, manifest: &PackManifest) -> Result<()> {
    let Some(declared) = manifest.signature.as_deref() else {
        println!(
            "nanny: {} is unsigned — its contents are trusted only as far as you \
             trust where you got it",
            manifest.slug()
        );
        return Ok(());
    };

    let actual = content_digest(dir)?;
    if actual != declared {
        bail!(
            "{} failed integrity check: pack.toml declares {declared} but the \
             contents hash to {actual}. Do not install it.",
            manifest.slug()
        );
    }
    println!("nanny: {} integrity verified", manifest.slug());
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn temp(tag: &str) -> PathBuf {
        let ts = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let d = std::env::temp_dir().join(format!("nanny_verify_{tag}_{ts}"));
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    fn manifest(sig: Option<&str>) -> PackManifest {
        PackManifest {
            name: "nanny:owasp".into(),
            version: "2.1.0".into(),
            description: String::new(),
            rules: vec![],
            python: None,
            rust: None,
            signature: sig.map(String::from),
        }
    }

    #[test]
    fn a_tampered_pack_is_refused() {
        let dir = temp("tampered");
        std::fs::write(dir.join("rules.py"), "def taint(ctx): return True\n").unwrap();
        let good = content_digest(&dir).unwrap();

        // A compromised rule fails silent, so this is the moment it must be caught.
        std::fs::write(dir.join("rules.py"), "def taint(ctx): return True  # always allow\n").unwrap();

        let err = verify(&dir, &manifest(Some(&good))).unwrap_err().to_string();
        assert!(err.contains("failed integrity check"));
        assert!(err.contains("Do not install it"));
    }

    #[test]
    fn an_intact_pack_verifies() {
        let dir = temp("intact");
        std::fs::write(dir.join("rules.py"), "def taint(ctx): return True\n").unwrap();
        let digest = content_digest(&dir).unwrap();
        assert!(verify(&dir, &manifest(Some(&digest))).is_ok());
    }

    #[test]
    fn an_unsigned_pack_installs_and_says_so() {
        let dir = temp("unsigned");
        std::fs::write(dir.join("rules.py"), "x = 1\n").unwrap();
        assert!(verify(&dir, &manifest(None)).is_ok());
    }

    #[test]
    fn moving_a_file_changes_the_digest() {
        // Path is folded in with the bytes, so relocating a rule is a change.
        let a = temp("path_a");
        std::fs::write(a.join("rules.py"), "x = 1\n").unwrap();
        let b = temp("path_b");
        std::fs::create_dir_all(b.join("python")).unwrap();
        std::fs::write(b.join("python/rules.py"), "x = 1\n").unwrap();
        assert_ne!(content_digest(&a).unwrap(), content_digest(&b).unwrap());
    }

    #[test]
    fn the_manifest_is_excluded_from_its_own_digest() {
        let dir = temp("selfref");
        std::fs::write(dir.join("rules.py"), "x = 1\n").unwrap();
        let before = content_digest(&dir).unwrap();
        std::fs::write(dir.join("pack.toml"), "name = \"x\"\nversion = \"1\"\n").unwrap();
        assert_eq!(before, content_digest(&dir).unwrap());
    }
}
