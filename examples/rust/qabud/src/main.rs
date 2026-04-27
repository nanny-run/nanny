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

mod collector;
mod rules;

use anyhow::Result;
use rig::client::CompletionClient;
use rig::completion::Prompt;
use rig::providers::groq;

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

// ── Review loop ───────────────────────────────────────────────────────────────
//
// Rust code reads each file directly via the nanny-governed read_file function.
// The LLM receives file contents and produces the review — it never dispatches
// tool calls. This makes enforcement model-agnostic.

#[nanny::agent("reviewer")]
async fn run_review(dir: &str) -> Result<String> {
    let files = collector::collect_files(dir);
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

    // Groq: free-tier API — reliable output, no credit card.
    // Get a key at console.groq.com, copy .env.example → .env, fill in GROQ_API_KEY.
    //
    // Offline/local fallback: swap groq::Client::new(...) for
    //   rig::providers::ollama::Client::from_val(rig::client::Nothing)
    // and change the model string to "qwen2.5:7b" or similar.
    let api_key = std::env::var("GROQ_API_KEY").unwrap_or_else(|_| {
        eprintln!("GROQ_API_KEY not set — copy .env.example to .env and add your key");
        std::process::exit(1);
    });
    let agent = groq::Client::new(&api_key)
        .expect("failed to create Groq client")
        .agent("llama-3.3-70b-versatile")
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
