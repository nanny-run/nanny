# webdingo — web research agent

A Rust agent that researches a topic by fetching web pages, governed by Nanny.

Demonstrates the complete Nanny developer workflow:

- `#[nanny::tool(cost = 20)]` — each HTTP fetch is metered
- `#[nanny::rule("no_loop")]` — stops the agent if it spirals on the same domain
- `agent_enter` / `agent_exit` — activates `[limits.researcher]` for the research scope

## Prerequisites

- Rust toolchain (`curl https://sh.rustup.rs -sSf | sh`)
- `nanny` CLI — macOS: `brew tap nanny-run/nanny && brew install nannyd` · Linux: `curl -fsSL https://install.nanny.run | sh` · Windows: `irm https://install.nanny.run/windows | iex` · or `cargo install nannyd`
- **Groq API key** — free tier at [console.groq.com](https://console.groq.com) (no credit card required). Copy `.env.example` to `.env` and fill in `GROQ_API_KEY`.

**Offline fallback:** edit one line in `src/main.rs` to swap the Groq client for Ollama (instructions are in the comment above `groq_client()`). Then `ollama pull qwen2.5:7b && ollama serve`.

## Setup

```sh
cd examples/rust/webdingo
cp .env.example .env
# Edit .env and set GROQ_API_KEY=<your_key_from_console.groq.com>
cargo build --release
```

Build before the first `nanny run`. Nanny's timeout starts when the process launches — if `cargo` compiles during the governed run, the timeout fires before the agent does anything. Building once upfront means `nanny run` launches the already-compiled binary immediately every time.

## Run

```sh
# Default run — uses [limits] from nanny.toml
nanny run -- "best Rust HTTP clients"

# Extended run — uses [limits.deep] (more steps, higher budget)
nanny run --limits=deep -- "history of the Rust programming language"
```

## What to expect

The agent fetches pages and streams the NDJSON event log to stdout in real time:

```
{"event":"ExecutionStarted","ts":1700000000000,...}
{"event":"ToolCalled","tool":"fetch_url","cost":20,...}
{"event":"ToolAllowed","tool":"fetch_url",...}
...
{"event":"ExecutionStopped","reason":"BudgetExhausted","steps":15,"cost_spent":300,...}
nanny: stopped — BudgetExhausted
```

**Stop reasons you may see:**

| Reason                | Cause                                                       |
| --------------------- | ----------------------------------------------------------- |
| `BudgetExhausted`     | Hit the 300-unit cost ceiling (15 fetches × 20 units)       |
| `RuleDenied: no_loop` | Agent looped on the same domain 5+ times in a row           |
| `ToolDenied`          | Agent tried a tool not in the allowlist (e.g. `write_file`) |
| `AgentCompleted`      | Research finished within limits                             |

## Development

This example uses the published `nannyd = "0.1.8"` crate from crates.io.
During active development on the nanny crate itself, switch to a path dependency:

```toml
# Cargo.toml
nannyd = { path = "../../../crates/cli" }   # instead of nannyd = "0.1.8"
```
