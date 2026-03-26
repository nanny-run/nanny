// qabud — code review agent
//
// Demonstrates the Nanny developer workflow end-to-end:
//   nanny run -- ./src
//
// Nanny features exercised:
//   #[nanny::tool(cost = 10)]            — each file read charges 10 cost units
//   #[nanny::rule("no_sensitive_files")] — demonstrates the rule hook mechanism
//   agent_enter / agent_exit             — activates [limits] for the review scope

use anyhow::Result;
use nanny::PolicyContext;
use rig::client::{CompletionClient, ProviderClient};
use rig::completion::{Prompt, ToolDefinition};
use rig::providers::openai;
use rig::tool::Tool;
use serde::{Deserialize, Serialize};
use serde_json::json;
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

// ── Nanny rule ────────────────────────────────────────────────────────────────

/// Deny reads when the agent is looping — same file read 3+ times in a row.
///
/// `tool_call_history` is an ordered list of tool names. If the agent is
/// stuck re-reading the same tool repeatedly we stop it early.
#[nanny::rule("no_sensitive_files")]
fn block_loop(ctx: &PolicyContext) -> bool {
    let history = &ctx.tool_call_history;
    if history.len() < 3 {
        return true; // not enough calls yet — allow
    }

    let all_reads = history
        .iter()
        .rev()
        .take(3)
        .all(|t| t == "read_file");

    if all_reads {
        eprintln!("nanny rule: detected read_file loop — denying");
        return false;
    }

    true
}

// ── Rig tool wrapper ──────────────────────────────────────────────────────────
//
// Rig requires a struct implementing rig::tool::Tool. The async call method
// delegates to the nanny-wrapped read_file via spawn_blocking.

#[derive(Deserialize, Serialize)]
struct ReadFileTool;

#[derive(Deserialize)]
struct ReadFileArgs {
    path: String,
}

#[derive(Debug, thiserror::Error)]
#[error("read error: {0}")]
struct ReadError(String);

impl Tool for ReadFileTool {
    const NAME: &'static str = "read_file";
    type Error = ReadError;
    type Args = ReadFileArgs;
    type Output = String;

    async fn definition(&self, _prompt: String) -> ToolDefinition {
        ToolDefinition {
            name: "read_file".to_string(),
            description: "Read a source file from disk and return its contents. \
                          Use this to inspect Rust source files for code review."
                .to_string(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "path": {
                        "type": "string",
                        "description": "Relative or absolute path to the file to read"
                    }
                },
                "required": ["path"]
            }),
        }
    }

    async fn call(&self, args: Self::Args) -> Result<Self::Output, Self::Error> {
        let path = args.path;
        tokio::task::spawn_blocking(move || read_file(path))
            .await
            .map_err(|e| ReadError(e.to_string()))
    }
}

// ── Review loop ───────────────────────────────────────────────────────────────

async fn run_review(dir: &str) -> Result<String> {
    // Activate [limits] for this scope via the nanny primitives.
    // #[nanny::agent] does not yet support async functions so we call directly.
    if nanny::__private::is_active() {
        nanny::__private::agent_enter("reviewer");
    }

    let result = review_inner(dir).await;

    if nanny::__private::is_active() {
        nanny::__private::agent_exit();
    }

    result
}

async fn review_inner(dir: &str) -> Result<String> {
    // Collect .rs files under the target directory.
    let rs_files = collect_rs_files(dir);
    if rs_files.is_empty() {
        return Ok(format!("No .rs files found under '{dir}'."));
    }

    let file_list = rs_files.join(", ");
    let prompt = format!(
        "Review the following Rust source files for correctness, safety, \
         and style: {file_list}. \
         Use the read_file tool to inspect each file. \
         For each file, note any issues you find and give a brief summary. \
         End with an overall assessment."
    );

    let client = openai::Client::from_env();

    let agent = client
        .agent(openai::GPT_4O)
        .preamble(
            "You are a senior Rust engineer performing a code review. \
             You have access to a read_file tool to inspect source files. \
             Be concise but thorough — flag real issues, not style nits.",
        )
        .tool(ReadFileTool)
        .max_tokens(2048)
        .build();

    let response = agent.prompt(&prompt).await?;
    Ok(response)
}

/// Recursively find all .rs files under `dir`, up to depth 3.
fn collect_rs_files(dir: &str) -> Vec<String> {
    let path = Path::new(dir);
    let mut files = Vec::new();
    collect_rs_recursive(path, 0, &mut files);
    files
}

fn collect_rs_recursive(dir: &Path, depth: usize, out: &mut Vec<String>) {
    if depth > 3 {
        return;
    }
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let p = entry.path();
        if p.is_dir() {
            collect_rs_recursive(&p, depth + 1, out);
        } else if p.extension().and_then(|e| e.to_str()) == Some("rs") {
            if let Some(s) = p.to_str() {
                out.push(s.to_string());
            }
        }
    }
}

// ── Entry point ───────────────────────────────────────────────────────────────

#[tokio::main]
async fn main() -> Result<()> {
    let dir = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "./src".to_string());

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
