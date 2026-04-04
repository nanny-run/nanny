// webdingo — multi-agent web research pipeline
//
// Three specialised agents collaborate under nanny governance:
//
//   planner    → [limits.planner]      decides which URLs to investigate
//   researcher → [limits.researcher]   fetches and extracts content from each URL
//   synthesizer → [limits.synthesizer] writes the final structured report
//
// Data flow:
//   topic ──► plan() ──► urls ──► research() ──► sources ──► synthesize() ──► report
//
// HTTP fetching uses nanny's built-in http_get tool, executed bridge-side.
// The child process never opens a network connection — nanny enforces all
// fetch policy (allowlist, cost, step limits) before making the request.
//
// Run:
//   nanny run                            # researches a default topic
//   nanny run -- "async runtimes Rust"   # custom topic

use anyhow::Result;
use nanny::PolicyContext;
use rig::client::{CompletionClient, Nothing, ProviderClient};
use rig::completion::Prompt;
use rig::providers::ollama;

// ── Nanny rule ────────────────────────────────────────────────────────────────

/// Stop the researcher if the last 5 tool calls were all http_get.
///
/// Guards against a model stuck re-fetching the same page in a loop.
#[nanny::rule("no_loop")]
fn detect_loop(ctx: &PolicyContext) -> bool {
    let history = &ctx.tool_call_history;
    if history.len() < 5 {
        return true;
    }
    let all_fetches = history.iter().rev().take(5).all(|t| t == "http_get");
    if all_fetches {
        eprintln!("nanny rule: http_get loop detected — stopping researcher");
        return false;
    }
    true
}

// ── Agent 1: Planner ──────────────────────────────────────────────────────────
//
// Governed by [limits.planner] — tight budget, no tools.
// Asks the LLM for a list of URLs worth fetching for the given topic.

#[nanny::agent("planner")]
async fn plan(topic: &str) -> Result<Vec<String>> {
    let client = ollama::Client::from_val(Nothing);
    let agent = client
        .agent(ollama::MISTRAL)
        .preamble(
            "You are a research planner. Given a topic, respond with 3 to 5 \
             specific URLs that would provide useful information about it. \
             Return ONLY the URLs, one per line, no bullet points, no explanations.",
        )
        .max_tokens(256)
        .build();

    let response = agent.prompt(topic).await?;

    eprintln!("planner: raw response —\n{response}");

    // Extract URLs embedded anywhere in a line — Mistral often wraps them in
    // bullet points, numbers, or markdown links like `- https://...` or
    // `1. https://...`. Scan each line for the first "http" occurrence.
    let urls: Vec<String> = response
        .lines()
        .filter_map(|line| {
            let trimmed = line.trim();
            trimmed.find("http").map(|i| {
                trimmed[i..]
                    .split_whitespace()
                    .next()
                    .unwrap_or("")
                    // strip trailing markdown punctuation
                    .trim_end_matches([')', '>', '"', '\'', ',', '.'])
                    .to_string()
            })
        })
        .filter(|url| url.starts_with("http"))
        .collect();

    eprintln!("planner: identified {} URL(s) to investigate", urls.len());
    Ok(urls)
}

// ── Agent 2: Researcher ────────────────────────────────────────────────────────
//
// Governed by [limits.researcher] — most expensive stage.
//
// Fetches each URL directly via nanny::http_get (bridge-side, policy-enforced)
// then asks the LLM to summarise the content. The LLM is used only for
// summarisation — not for deciding whether to fetch. This avoids relying on
// local model tool-dispatch reliability for something already determined by
// the planner.

#[nanny::agent("researcher")]
async fn research(topic: &str, urls: Vec<String>) -> Result<Vec<String>> {
    if urls.is_empty() {
        eprintln!("researcher: no URLs to fetch — skipping");
        return Ok(vec![]);
    }

    let client = ollama::Client::from_val(Nothing);
    let summariser = client
        .agent(ollama::MISTRAL)
        .preamble(
            "You are a research assistant. Given a URL and its raw content, \
             extract the 2-3 most relevant sentences about the research topic. \
             Be concise and factual — only use what is in the content.",
        )
        .max_tokens(256)
        .build();

    let mut sources = Vec::new();

    for url in &urls {
        // Fetch directly through the bridge — always tracked, always policy-enforced.
        // Network errors (DNS failure, timeout, connection refused) are per-URL: log and
        // skip. Only propagate if the bridge itself fails (spawn error = infrastructure issue).
        let raw = match tokio::task::spawn_blocking({
            let u = url.clone();
            move || nanny::http_get(u)
        })
        .await
        .map_err(|e| anyhow::anyhow!("spawn error: {e}"))?
        {
            Ok(content) => content,
            Err(e) => {
                eprintln!("researcher: skipped {} — fetch failed: {}", url, e);
                continue;
            }
        };

        eprintln!("researcher: fetched {} ({} chars)", url, raw.len());

        // LLM summarises the fetched content only.
        let prompt = format!(
            "Research topic: {topic}\n\nURL: {url}\n\nContent:\n{raw}\n\n\
             Extract the most relevant information about the topic."
        );
        let summary = summariser.prompt(&prompt).await?;
        sources.push(format!("Source: {url}\n{summary}"));
    }

    eprintln!("researcher: summarised {} source(s)", sources.len());
    Ok(sources)
}

// ── Agent 3: Synthesizer ───────────────────────────────────────────────────────
//
// Governed by [limits.synthesizer] — no tools needed.
// Receives researcher notes and writes a structured markdown report.

#[nanny::agent("synthesizer")]
async fn synthesize(topic: &str, sources: Vec<String>) -> Result<String> {
    let notes = if sources.is_empty() {
        "No source material was collected.".to_string()
    } else {
        sources.join("\n\n---\n\n")
    };

    let client = ollama::Client::from_val(Nothing);
    let agent = client
        .agent(ollama::MISTRAL)
        .preamble(
            "You are a technical writer. Given research notes and a topic, write \
             a clear, well-structured report in markdown. Only include information \
             that appears in the provided notes — do not invent facts.",
        )
        .max_tokens(1024)
        .build();

    let prompt = format!(
        "Topic: {topic}\n\n\
         Research notes:\n{notes}\n\n\
         Write a structured markdown report based on these notes."
    );

    agent.prompt(&prompt).await.map_err(Into::into)
}

// ── Pipeline orchestrator ─────────────────────────────────────────────────────
//
// Runs at the global [limits] — no agent scope of its own.
// Coordinates the three agents and passes data between them.

async fn run_pipeline(topic: &str) -> Result<String> {
    eprintln!("pipeline: [1/3] planning research strategy...");
    let urls = plan(topic).await?;

    eprintln!("pipeline: [2/3] fetching and extracting sources...");
    let sources = research(topic, urls).await?;

    eprintln!("pipeline: [3/3] synthesizing report...");
    synthesize(topic, sources).await
}

// ── Entry point ───────────────────────────────────────────────────────────────

#[tokio::main]
async fn main() -> Result<()> {
    dotenvy::dotenv().ok();

    let topic = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "best Rust HTTP client crates".to_string());

    eprintln!("webdingo: researching '{topic}'");
    eprintln!("webdingo: pipeline — planner → researcher → synthesizer");
    eprintln!("webdingo: NDJSON event log → stdout");
    eprintln!();

    match run_pipeline(&topic).await {
        Ok(report) => {
            eprintln!();
            eprintln!("── Research report ─────────────────────────────");
            println!("{report}");
        }
        Err(e) => {
            eprintln!("webdingo: stopped — {e}");
            std::process::exit(1);
        }
    }

    Ok(())
}
