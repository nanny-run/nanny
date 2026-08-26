//! `nanny rules` — install and inspect rule packs.
//!
//! Installing writes two things and edits no source file: a pinned entry in
//! `[rules] extends`, and the pack itself under `.nanny/rules/`, committed like
//! any vendored dependency. `@rule` stays what it always was, the decorator a
//! developer uses for their own private rules.
//!
//! Everything here is a human action at a terminal. The engine never installs,
//! never fetches, and never upgrades: it reads what is on disk. A control that
//! could change without someone deciding to change it is not a control.

use anyhow::{bail, Context, Result};
use nanny_config::pack::{pack_path, PackManifest, PACK_DIR};
use std::path::{Path, PathBuf};

#[derive(clap::Subcommand, Debug)]
pub enum RulesCommand {
    /// Install a rule pack and declare it in nanny.toml.
    ///
    /// Takes `name@version`, always pinned. Source is a local directory or a
    /// git checkout you already have; hosted sources arrive with the registry.
    Add {
        /// Pack to install, as `name@version`.
        pack: String,
        /// Directory holding the pack to install.
        #[arg(long)]
        from: PathBuf,
    },

    /// List the packs this project has installed.
    List,

    /// Remove a pack from disk and from nanny.toml.
    Remove {
        /// Pack to remove, as `name@version`.
        pack: String,
    },
}

/// Split `name@version`, refusing anything unpinned.
fn split_pin(pack: &str) -> Result<(String, String)> {
    match pack.trim().rsplit_once('@') {
        Some((name, version)) if !name.trim().is_empty() && !version.trim().is_empty() => {
            Ok((name.trim().to_string(), version.trim().to_string()))
        }
        _ => bail!(
            "'{pack}' is not pinned — use 'name@version'. An unpinned pack lets \
             the rules change without anyone deciding to change them."
        ),
    }
}

pub fn run(cmd: RulesCommand, project_root: &Path) -> Result<()> {
    match cmd {
        RulesCommand::Add { pack, from } => add(&pack, &from, project_root),
        RulesCommand::List => list(project_root),
        RulesCommand::Remove { pack } => remove(&pack, project_root),
    }
}

fn add(pack: &str, from: &Path, project_root: &Path) -> Result<()> {
    let (name, version) = split_pin(pack)?;

    let manifest_src = from.join("pack.toml");
    let raw = std::fs::read_to_string(&manifest_src)
        .with_context(|| format!("no pack.toml at '{}'", manifest_src.display()))?;
    let manifest: PackManifest = toml::from_str(&raw)
        .with_context(|| format!("invalid pack.toml at '{}'", manifest_src.display()))?;

    if manifest.name != name || manifest.version != version {
        bail!(
            "'{}' contains {} but you asked for {name}@{version}",
            from.display(),
            manifest.slug()
        );
    }

    // Signature verification belongs here, at install time, and nowhere else.
    // Verifying during a run would put trust roots and possibly a network call
    // on the enforcement path, which must stay offline and deterministic.
    super::rules_verify::verify(from, &manifest)?;

    let dest = pack_path(project_root, &name, &version);
    if dest.exists() {
        std::fs::remove_dir_all(&dest)
            .with_context(|| format!("could not replace '{}'", dest.display()))?;
    }
    copy_dir(from, &dest)?;

    declare_in_config(project_root, &format!("{name}@{version}"))?;

    println!(
        "nanny: installed {}@{} to {}",
        name,
        version,
        dest.display()
    );
    println!("nanny: declared in [rules] extends");
    if !manifest.rules.is_empty() {
        println!(
            "nanny: {} rule(s): {:?}",
            manifest.rules.len(),
            manifest.rules
        );
    }
    println!(
        "nanny: commit {}/ so every run of this project enforces the same controls",
        PACK_DIR
    );
    Ok(())
}

fn list(project_root: &Path) -> Result<()> {
    let dir = project_root.join(PACK_DIR);
    let Ok(entries) = std::fs::read_dir(&dir) else {
        println!("nanny: no rule packs installed");
        return Ok(());
    };

    let mut found = 0;
    for entry in entries.flatten() {
        let manifest_path = entry.path().join("pack.toml");
        let Ok(raw) = std::fs::read_to_string(&manifest_path) else {
            continue;
        };
        let Ok(m) = toml::from_str::<PackManifest>(&raw) else {
            continue;
        };
        let signed = if m.signature.is_some() {
            "signed"
        } else {
            "unsigned"
        };
        println!(
            "{:<28} {:<9} {} rule(s)  {}",
            m.slug(),
            signed,
            m.rules.len(),
            m.description
        );
        found += 1;
    }
    if found == 0 {
        println!("nanny: no rule packs installed");
    }
    Ok(())
}

fn remove(pack: &str, project_root: &Path) -> Result<()> {
    let (name, version) = split_pin(pack)?;
    let dir = pack_path(project_root, &name, &version);
    if dir.exists() {
        std::fs::remove_dir_all(&dir)
            .with_context(|| format!("could not remove '{}'", dir.display()))?;
    }
    undeclare_in_config(project_root, &format!("{name}@{version}"))?;
    println!("nanny: removed {name}@{version}");
    Ok(())
}

// ── nanny.toml editing ────────────────────────────────────────────────────────
//
// Edited as text through `toml_edit` rather than parsed and re-serialised:
// rewriting the file from a parsed struct would discard the operator's comments
// and ordering, and a governance config is a document people read.

fn config_path(project_root: &Path) -> PathBuf {
    project_root.join("nanny.toml")
}

fn declare_in_config(project_root: &Path, pin: &str) -> Result<()> {
    let path = config_path(project_root);
    let raw = std::fs::read_to_string(&path)
        .with_context(|| format!("no nanny.toml at '{}'", path.display()))?;
    let mut doc: toml_edit::DocumentMut = raw.parse().context("nanny.toml is not valid TOML")?;

    let rules = doc
        .entry("rules")
        .or_insert(toml_edit::Item::Table(toml_edit::Table::new()));
    let table = rules.as_table_mut().context("[rules] is not a table")?;
    let extends = table
        .entry("extends")
        .or_insert(toml_edit::value(toml_edit::Array::new()));
    let array = extends
        .as_array_mut()
        .context("[rules] extends is not an array")?;

    if array.iter().any(|v| v.as_str() == Some(pin)) {
        return Ok(());
    }
    array.push(pin);

    std::fs::write(&path, doc.to_string())
        .with_context(|| format!("could not write '{}'", path.display()))
}

fn undeclare_in_config(project_root: &Path, pin: &str) -> Result<()> {
    let path = config_path(project_root);
    let Ok(raw) = std::fs::read_to_string(&path) else {
        return Ok(());
    };
    let mut doc: toml_edit::DocumentMut = raw.parse().context("nanny.toml is not valid TOML")?;

    if let Some(array) = doc
        .get_mut("rules")
        .and_then(|r| r.as_table_mut())
        .and_then(|t| t.get_mut("extends"))
        .and_then(|e| e.as_array_mut())
    {
        array.retain(|v| v.as_str() != Some(pin));
    }
    std::fs::write(&path, doc.to_string())
        .with_context(|| format!("could not write '{}'", path.display()))
}

fn copy_dir(src: &Path, dest: &Path) -> Result<()> {
    std::fs::create_dir_all(dest)?;
    for entry in std::fs::read_dir(src)? {
        let entry = entry?;
        let to = dest.join(entry.file_name());
        if entry.file_type()?.is_dir() {
            copy_dir(&entry.path(), &to)?;
        } else {
            std::fs::copy(entry.path(), &to)?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_unpinned_pack_is_refused() {
        let err = split_pin("nanny:owasp").unwrap_err().to_string();
        assert!(err.contains("is not pinned"));
        assert!(
            err.contains("without anyone deciding"),
            "the message must say why pinning matters, not just that it is required"
        );
    }

    #[test]
    fn a_namespaced_name_splits_on_the_last_at() {
        assert_eq!(
            split_pin("acme:internal:fraud@0.3.1").unwrap(),
            ("acme:internal:fraud".to_string(), "0.3.1".to_string())
        );
    }
}
