use nanny::PolicyContext;

/// Deny reads when the agent has issued too many consecutive reads.
///
/// Guards against runaway file reading — if the last 8 calls were all
/// read_file, something is wrong. Stop early before cost ceiling is hit.
#[nanny::rule("no_read_loop")]
pub fn block_loop(ctx: &PolicyContext) -> bool {
    let history = &ctx.tool_call_history;
    if history.len() < 8 {
        return true;
    }

    let all_reads = history
        .iter()
        .rev()
        .take(8)
        .all(|t| t == "read_file");

    if all_reads {
        eprintln!("nanny rule: detected read_file loop — denying");
        return false;
    }

    true
}

/// Deny reads of sensitive files — paths containing `.env` or `secret`.
///
/// Uses `last_tool_args` to inspect the `path` argument before the call
/// reaches disk. The decision is made client-side before the tool body runs —
/// the file is never opened.
#[nanny::rule("no_sensitive_files")]
pub fn block_sensitive(ctx: &PolicyContext) -> bool {
    ctx.last_tool_args
        .get("path")
        .map(|p| !p.contains(".env") && !p.contains("secret"))
        .unwrap_or(true)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn ctx_with_path(path: &str) -> PolicyContext {
        let mut args = HashMap::new();
        args.insert("path".to_string(), path.to_string());
        PolicyContext {
            last_tool_args: args,
            ..Default::default()
        }
    }

    fn ctx_no_path() -> PolicyContext {
        PolicyContext::default()
    }

    // no_sensitive_files rule ─────────────────────────────────────────────────

    #[test]
    fn blocks_dotenv_file() {
        assert!(!block_sensitive(&ctx_with_path(".env")));
    }

    #[test]
    fn blocks_dotenv_in_subdirectory() {
        assert!(!block_sensitive(&ctx_with_path("/project/config/.env")));
    }

    #[test]
    fn blocks_env_suffix() {
        assert!(!block_sensitive(&ctx_with_path("production.env")));
    }

    #[test]
    fn blocks_path_containing_secret() {
        assert!(!block_sensitive(&ctx_with_path("secrets.toml")));
        assert!(!block_sensitive(&ctx_with_path("/etc/secret_key")));
        assert!(!block_sensitive(&ctx_with_path("my_secret.json")));
    }

    #[test]
    fn allows_normal_rs_files() {
        assert!(block_sensitive(&ctx_with_path("src/main.rs")));
        assert!(block_sensitive(&ctx_with_path("/project/src/lib.rs")));
    }

    #[test]
    fn allows_readme_and_toml() {
        assert!(block_sensitive(&ctx_with_path("README.md")));
        assert!(block_sensitive(&ctx_with_path("Cargo.toml")));
    }

    #[test]
    fn allows_when_no_path_arg() {
        assert!(block_sensitive(&ctx_no_path()));
    }

    // no_read_loop rule ───────────────────────────────────────────────────────

    #[test]
    fn allows_when_few_calls() {
        let mut ctx = PolicyContext::default();
        ctx.tool_call_history = vec!["read_file".to_string(); 7];
        assert!(block_loop(&ctx));
    }

    #[test]
    fn denies_eight_consecutive_read_file_calls() {
        let mut ctx = PolicyContext::default();
        ctx.tool_call_history = vec!["read_file".to_string(); 8];
        assert!(!block_loop(&ctx));
    }

    #[test]
    fn allows_mixed_tool_history() {
        let mut ctx = PolicyContext::default();
        ctx.tool_call_history = vec![
            "read_file".to_string(),
            "http_get".to_string(),
            "read_file".to_string(),
        ];
        assert!(block_loop(&ctx));
    }
}
