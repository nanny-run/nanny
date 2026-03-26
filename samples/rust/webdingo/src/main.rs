// webdingo — web research agent
//
// Demonstrates the Nanny developer workflow end-to-end:
//   nanny run -- "best Rust HTTP clients"
//
// Nanny features exercised:
//   #[nanny::tool(cost = 20)]   — each fetch charges 20 cost units
//   #[nanny::rule("no_loop")]   — stops the agent if it loops on the same domain
//   agent_enter / agent_exit    — activates [limits.researcher] for the research scope
//
// Stop reasons you may see:
//   BudgetExhausted             — hit the 300-unit cost ceiling
//   RuleDenied: no_loop         — agent was looping on a single domain
//   ToolDenied                  — agent tried a tool not in the allowlist
//   AgentCompleted              — research finished within limits

use anyhow::Result;
use nanny::PolicyContext;
use rig::client::{CompletionClient, ProviderClient};
use rig::completion::{Prompt, ToolDefinition};
use rig::providers::openai;
use rig::tool::Tool;
use serde::{Deserialize, Serialize};
use serde_json::json;

// ── Nanny-governed tool ───────────────────────────────────────────────────────

/// Fetch a URL and return the first 2 000 characters of the response body.
///
/// Decorated with #[nanny::tool(cost = 20)]:
///   - contacts the bridge before each call
///   - charges 20 cost units on each successful call
///   - panics with "nanny: stopped — ..." when budget, steps, or a rule fires
///
/// Must be synchronous — the macro generates a sync inner wrapper.
/// Called from async context via tokio::task::spawn_blocking.
#[nanny::tool(cost = 20)]
fn fetch_url(url: String) -> String {
    reqwest::blocking::get(&url)
        .and_then(|r| r.text())
        .unwrap_or_default()
        .chars()
        .take(2_000)
        .collect()
}

// ── Nanny rule ────────────────────────────────────────────────────────────────

/// Deny if the last 5 tool calls were all fetch_url — the classic spiral pattern.
///
/// `tool_call_history` is an ordered list of tool names. If the last 5 entries
/// are all "fetch_url" the agent is in a fetch loop and we stop it.
#[nanny::rule("no_loop")]
fn detect_loop(ctx: &PolicyContext) -> bool {
    let history = &ctx.tool_call_history;
    if history.len() < 5 {
        return true; // not enough calls yet — allow
    }

    let all_fetches = history
        .iter()
        .rev()
        .take(5)
        .all(|t| t == "fetch_url");

    if all_fetches {
        eprintln!("nanny rule: detected fetch_url loop — denying");
        return false;
    }

    true
}

// ── Rig tool wrapper ──────────────────────────────────────────────────────────
//
// Rig requires a struct implementing rig::tool::Tool. The async call method
// delegates to the nanny-wrapped fetch_url via spawn_blocking.

#[derive(Deserialize, Serialize)]
struct FetchUrlTool;

#[derive(Deserialize)]
struct FetchUrlArgs {
    url: String,
}

#[derive(Debug, thiserror::Error)]
#[error("fetch error: {0}")]
struct FetchError(String);

impl Tool for FetchUrlTool {
    const NAME: &'static str = "fetch_url";
    type Error = FetchError;
    type Args = FetchUrlArgs;
    type Output = String;

    async fn definition(&self, _prompt: String) -> ToolDefinition {
        ToolDefinition {
            name: "fetch_url".to_string(),
            description: "Fetch a URL and return its text content. \
                          Use this to retrieve web pages for research."
                .to_string(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "url": {
                        "type": "string",
                        "description": "The URL to fetch (must start with http:// or https://)"
                    }
                },
                "required": ["url"]
            }),
        }
    }

    async fn call(&self, args: Self::Args) -> Result<Self::Output, Self::Error> {
        let url = args.url;
        tokio::task::spawn_blocking(move || fetch_url(url))
            .await
            .map_err(|e| FetchError(e.to_string()))
    }
}

// ── Research loop ─────────────────────────────────────────────────────────────

async fn run_research(topic: &str) -> Result<String> {
    // Activate [limits.researcher] for this scope.
    // This is equivalent to #[nanny::agent("researcher")] — the macro does not
    // yet support async functions, so we call the primitives directly.
    if nanny::__private::is_active() {
        nanny::__private::agent_enter("researcher");
    }

    let result = research_inner(topic).await;

    if nanny::__private::is_active() {
        nanny::__private::agent_exit();
    }

    result
}

async fn research_inner(topic: &str) -> Result<String> {
    let client = openai::Client::from_env();

    let agent = client
        .agent(openai::GPT_4O)
        .preamble(
            "You are a research assistant. Given a topic, search for relevant URLs, \
             fetch their content using the fetch_url tool, and synthesize a concise report. \
             Start by fetching a search engine results page or a known authoritative URL. \
             Stop when you have enough information to write a useful summary.",
        )
        .tool(FetchUrlTool)
        .max_tokens(1024)
        .build();

    let response = agent.prompt(topic).await?;
    Ok(response)
}

// ── Entry point ───────────────────────────────────────────────────────────────

#[tokio::main]
async fn main() -> Result<()> {
    dotenvy::dotenv().ok();

    let topic = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "best Rust HTTP client crates 2024".to_string());

    eprintln!("webdingo: researching '{topic}'");
    eprintln!("webdingo: NDJSON event log → stdout");
    eprintln!();

    match run_research(&topic).await {
        Ok(report) => {
            eprintln!();
            eprintln!("── Research report ─────────────────────────────");
            println!("{report}");
        }
        Err(e) => {
            // Nanny panics are caught by Rig and surface as errors here.
            eprintln!("webdingo: stopped — {e}");
            std::process::exit(1);
        }
    }

    Ok(())
}
