# webdingo — web research agent

A Rust agent that researches a topic by fetching web pages, governed by Nanny.

Demonstrates the complete Nanny developer workflow:

- `#[nanny::tool(cost = 20)]` — each HTTP fetch is metered
- `#[nanny::rule("no_loop")]` — stops the agent if it spirals on the same domain
- `agent_enter` / `agent_exit` — activates `[limits.researcher]` for the research scope

## Prerequisites

- Rust toolchain (`curl https://sh.rustup.rs -sSf | sh`)
- `nanny` CLI — macOS: `brew tap nanny-run/nanny && brew install nannyd` · Linux: `curl -fsSL https://install.nanny.run | sh` · Windows: `irm https://install.nanny.run/windows | iex` · or `cargo install nannyd`
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

| Reason                | Cause                                                       |
| --------------------- | ----------------------------------------------------------- |
| `BudgetExhausted`     | Hit the 300-unit cost ceiling (15 fetches × 20 units)       |
| `RuleDenied: no_loop` | Agent looped on the same domain 5+ times in a row           |
| `ToolDenied`          | Agent tried a tool not in the allowlist (e.g. `write_file`) |
| `AgentCompleted`      | Research finished within limits                             |

## Development

This example uses the published `nannyd = "0.1.7"` crate from crates.io.
During active development on the nanny crate itself, switch to a path dependency:

```toml
# Cargo.toml
nannyd = { path = "../../../crates/cli" }   # instead of nannyd = "0.1.7"
```
