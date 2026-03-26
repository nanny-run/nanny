# webdingo — web research agent

A Rust agent that researches a topic by fetching web pages, governed by Nanny.

Demonstrates the complete Nanny developer workflow:
- `#[nanny::tool(cost = 20)]` — each HTTP fetch is metered
- `#[nanny::rule("no_loop")]` — stops the agent if it spirals on the same domain
- `agent_enter` / `agent_exit` — activates `[limits.researcher]` for the research scope

## Prerequisites

- Rust toolchain (`curl https://sh.rustup.rs -sSf | sh`)
- `nanny` CLI (`cargo install nannyd` or `brew install nannyd`)
- [Ollama](https://ollama.com) running locally with `llama3.2` pulled:

```sh
brew install ollama
ollama serve          # in a separate terminal, or run as a service
ollama pull llama3.2
```

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

| Reason | Cause |
|--------|-------|
| `BudgetExhausted` | Hit the 300-unit cost ceiling (15 fetches × 20 units) |
| `RuleDenied: no_loop` | Agent looped on the same domain 5+ times in a row |
| `ToolDenied` | Agent tried a tool not in the allowlist (e.g. `write_file`) |
| `AgentCompleted` | Research finished within limits |

## Development

During development this sample uses a path dependency to the local `nannyd` crate.
To switch to the published crate after `v0.1.2` ships:

```toml
# Cargo.toml
nannyd = "0.1.2"   # instead of path = "../../../crates/cli"
```
