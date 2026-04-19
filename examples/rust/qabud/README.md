# qabud — code review agent

A Rust agent that reviews source files in a directory, governed by Nanny.

Demonstrates the complete Nanny developer workflow:

- `#[nanny::tool(cost = 10)]` — each file read is metered
- `#[nanny::rule("no_sensitive_files")]` — stops the agent if it loops on the same tool
- `agent_enter` / `agent_exit` — activates `[limits]` for the review scope

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
# Review ./src (default)
nanny run

# Review a specific directory
nanny run -- ./src
```

## What to expect

The agent reads files and streams the NDJSON event log to stdout in real time:

```
{"event":"ExecutionStarted","ts":1700000000000,...}
{"event":"ToolCalled","tool":"read_file","cost":10,...}
{"event":"ToolAllowed","tool":"read_file",...}
...
{"event":"ExecutionStopped","reason":"AgentCompleted","steps":8,"cost_spent":80,...}
```

**Stop reasons you may see:**

| Reason                           | Cause                                                                           |
| -------------------------------- | ------------------------------------------------------------------------------- |
| `BudgetExhausted`                | Hit the 400-unit cost ceiling (10 reads × 10 units each... wait, 40 reads × 10) |
| `RuleDenied: no_sensitive_files` | Agent looped on read_file 3+ times in a row                                     |
| `ToolDenied`                     | Agent tried a tool not in the allowlist (e.g. `write_file`)                     |
| `AgentCompleted`                 | Review finished within limits                                                   |

## Development

This example uses the published `nannyd = "0.1.5"` crate from crates.io.
During active development on the nanny crate itself, switch to a path dependency:

```toml
# Cargo.toml
nannyd = { path = "../../../crates/cli" }   # instead of nannyd = "0.1.5"
```
