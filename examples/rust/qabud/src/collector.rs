use std::path::Path;

/// Collect files to review under `dir`.
///
/// Includes `.rs` source files and sensitive file patterns (`.env`, `secret*`)
/// so the no_sensitive_files rule can demonstrate blocking them.
///
/// `.rs` files are always ordered before sensitive patterns so that
/// budget/step limits fire cleanly on real source files, and
/// no_sensitive_files only fires after all source files have been attempted.
pub fn collect_files(dir: &str) -> Vec<String> {
    let path = Path::new(dir);
    let mut files = Vec::new();
    collect_recursive(path, 0, &mut files);
    // Order .rs files before sensitive patterns so step/budget limits
    // fire on real source before hitting .env or secret* files.
    files.sort_by(|a, b| {
        let a_rs = a.ends_with(".rs");
        let b_rs = b.ends_with(".rs");
        match (a_rs, b_rs) {
            (true, false) => std::cmp::Ordering::Less,
            (false, true) => std::cmp::Ordering::Greater,
            _ => a.cmp(b),
        }
    });
    files
}

pub fn collect_recursive(dir: &Path, depth: usize, out: &mut Vec<String>) {
    if depth > 3 {
        return;
    }
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let p = entry.path();
        if p.is_dir() {
            // Skip build artifacts — target/ can contain thousands of .rs files.
            let name = p.file_name().and_then(|n| n.to_str()).unwrap_or("");
            if name == "target" || name == "tests" {
                continue;
            }
            collect_recursive(&p, depth + 1, out);
        } else if should_collect(&p) {
            if let Some(s) = p.to_str() {
                out.push(s.to_string());
            }
        }
    }
}

/// Returns true for files that should be attempted for review.
///
/// Collects Rust source files plus sensitive file patterns so the
/// no_sensitive_files rule has an opportunity to fire on the latter.
pub fn should_collect(path: &Path) -> bool {
    let name = path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("");
    let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("");

    ext == "rs" || name == ".env" || name.ends_with(".env") || name.starts_with("secret")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn collects_rs_files() {
        assert!(should_collect(Path::new("src/main.rs")));
        assert!(should_collect(Path::new("lib.rs")));
    }

    #[test]
    fn collects_sensitive_patterns() {
        assert!(should_collect(Path::new(".env")));
        assert!(should_collect(Path::new("production.env")));
        assert!(should_collect(Path::new("secrets.toml")));
    }

    #[test]
    fn skips_non_source_files() {
        assert!(!should_collect(Path::new("Cargo.toml")));
        assert!(!should_collect(Path::new("README.md")));
        assert!(!should_collect(Path::new("nanny.toml")));
    }

    #[test]
    fn rs_files_sort_before_sensitive_patterns() {
        let mut files = vec![
            "./.env".to_string(),
            "./src/main.rs".to_string(),
            "./src/lib.rs".to_string(),
        ];
        files.sort_by(|a, b| {
            let a_rs = a.ends_with(".rs");
            let b_rs = b.ends_with(".rs");
            match (a_rs, b_rs) {
                (true, false) => std::cmp::Ordering::Less,
                (false, true) => std::cmp::Ordering::Greater,
                _ => a.cmp(b),
            }
        });
        // .rs files must come first regardless of alphabetical order
        assert!(files[0].ends_with(".rs"), "first file should be .rs, got {}", files[0]);
        assert!(files[1].ends_with(".rs"), "second file should be .rs, got {}", files[1]);
        assert!(files[2].ends_with(".env"), "last file should be .env, got {}", files[2]);
    }
}
