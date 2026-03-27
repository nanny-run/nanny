// qabud — code review agent
//
// Demonstrates the Nanny developer workflow end-to-end:
//   nanny run           # reviews current directory (finds .rs + sensitive files)
//   nanny run -- ./src  # reviews ./src only
//
// Nanny features exercised:
//   #[nanny::tool(cost = 10)]            — each file read charges 10 cost units
//   #[nanny::rule("no_read_loop")]       — guard against runaway reads (history-based)
//   #[nanny::rule("no_sensitive_files")] — block .env / secret paths (args-based)
//   #[nanny::agent("reviewer")]          — activates [limits.reviewer] for the review scope
//
// Architecture: Rust code drives file reading directly — nanny governs each read.
// The LLM receives file contents and handles review only. Enforcement is model-agnostic:
// nanny events fire regardless of whether the model supports function-calling.

use anyhow::Result;
use nanny::PolicyContext;
use rig::client::{CompletionClient, Nothing, ProviderClient};
use rig::completion::Prompt;
use rig::providers::ollama;
use std::path::Path;

// ── Nanny-governed tool ───────────────────────────────────────────────────────

/// Read a file from disk and return its contents.
///
/// Decorated with #[nanny::tool(cost = 10)]:
///   - contacts the bridge before each call
///   - charges 10 cost units on each successful call
///   - panics with "nanny: stopped — ..." when budget, steps, or a rule fires
///
/// Must be synchronous — the macro generates a sync inner wrapper.
/// Called from async context via tokio::task::spawn_blocking.
#[nanny::tool(cost = 10)]
fn read_file(path: String) -> String {
    std::fs::read_to_string(&path).unwrap_or_default()
}

// ── Nanny rules ───────────────────────────────────────────────────────────────

/// Deny reads when the agent has issued too many consecutive reads.
///
/// Guards against runaway file reading — if the last 8 calls were all
/// read_file, something is wrong. Stop early before cost ceiling is hit.
#[nanny::rule("no_read_loop")]
fn block_loop(ctx: &PolicyContext) -> bool {
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
fn block_sensitive(ctx: &PolicyContext) -> bool {
    ctx.last_tool_args
        .get("path")
        .map(|p| !p.contains(".env") && !p.contains("secret"))
        .unwrap_or(true)
}

// ── Review loop ───────────────────────────────────────────────────────────────
//
// Rust code reads each file directly via the nanny-governed read_file function.
// The LLM receives file contents and produces the review — it never dispatches
// tool calls. This makes enforcement model-agnostic.

#[nanny::agent("reviewer")]
async fn run_review(dir: &str) -> Result<String> {
    let files = collect_files(dir);
    if files.is_empty() {
        return Ok(format!("No reviewable files found under '{dir}'."));
    }

    eprintln!("qabud: found {} file(s) to attempt", files.len());

    let mut reviewed = Vec::new();

    for path in &files {
        // Read directly — nanny governs each call before disk access.
        //   no_sensitive_files fires for .env/secret paths (file never opened).
        //   no_read_loop fires if consecutive reads exceed threshold.
        //   Step and cost limits fire when the budget is exhausted.
        let content = match tokio::task::spawn_blocking({
            let p = path.clone();
            move || read_file(p)
        })
        .await
        {
            Ok(c) => c,
            Err(e) => {
                // JoinError wraps the panic from a nanny rule or limit stop.
                eprintln!("nanny: skipped '{}' — {}", path, e);
                continue;
            }
        };

        eprintln!("qabud: read '{}' ({} chars)", path, content.len());
        reviewed.push(format!("// {path}\n{content}"));
    }

    if reviewed.is_empty() {
        return Ok("All files were blocked by nanny policy.".to_string());
    }

    let combined = reviewed.join("\n\n---\n\n");

    let client = ollama::Client::from_val(Nothing);
    let agent = client
        .agent(ollama::MISTRAL)
        .preamble(
            "You are a senior Rust engineer performing a code review. \
             You will be given the full contents of one or more source files. \
             For each file, note any real issues and give a brief summary. \
             End with an overall assessment. Be concise but thorough.",
        )
        .max_tokens(2048)
        .build();

    let prompt = format!(
        "Review the following source files:\n\n{combined}\n\n\
         For each file, note any issues you find and give a brief summary. \
         End with an overall assessment."
    );

    agent.prompt(&prompt).await.map_err(Into::into)
}

// ── File collection ───────────────────────────────────────────────────────────

/// Collect files to review under `dir`.
///
/// Includes `.rs` source files and sensitive file patterns (`.env`, `secret*`)
/// so the no_sensitive_files rule can demonstrate blocking them.
fn collect_files(dir: &str) -> Vec<String> {
    let path = Path::new(dir);
    let mut files = Vec::new();
    collect_recursive(path, 0, &mut files);
    files.sort();
    files
}

fn collect_recursive(dir: &Path, depth: usize, out: &mut Vec<String>) {
    if depth > 3 {
        return;
    }
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let p = entry.path();
        if p.is_dir() {
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
fn should_collect(path: &Path) -> bool {
    let name = path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("");
    let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("");

    ext == "rs" || name == ".env" || name.ends_with(".env") || name.starts_with("secret")
}

// ── Entry point ───────────────────────────────────────────────────────────────

#[tokio::main]
async fn main() -> Result<()> {
    dotenvy::dotenv().ok();

    // Default to "." so `nanny run` reviews the whole project directory,
    // including any .env file at root — triggering no_sensitive_files.
    let dir = std::env::args()
        .nth(1)
        .unwrap_or_else(|| ".".to_string());

    eprintln!("qabud: reviewing '{dir}'");
    eprintln!("qabud: NDJSON event log → stdout");
    eprintln!();

    match run_review(&dir).await {
        Ok(report) => {
            eprintln!();
            eprintln!("── Code review ──────────────────────────────────");
            println!("{report}");
        }
        Err(e) => {
            // Nanny panics are caught by Rig and surface as errors here.
            eprintln!("qabud: stopped — {e}");
            std::process::exit(1);
        }
    }

    Ok(())
}

// ── Tests ─────────────────────────────────────────────────────────────────────

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

    // should_collect ──────────────────────────────────────────────────────────

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
}
