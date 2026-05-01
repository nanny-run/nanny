# qabud — code review agent

A Rust agent that reviews source files in a directory, governed by Nanny.

Demonstrates the complete Nanny developer workflow:

- `#[nanny::tool(cost = 10)]` — each file read is metered
- `#[nanny::rule("no_sensitive_files")]` — stops the agent if it loops on the same tool
- `agent_enter` / `agent_exit` — activates `[limits]` for the review scope

## Prerequisites

- Rust toolchain (`curl https://sh.rustup.rs -sSf | sh`)
- `nanny` CLI — macOS: `brew tap nanny-run/nanny && brew install nannyd` · Linux: `curl -fsSL https://install.nanny.run | sh` · Windows: `irm https://install.nanny.run/windows | iex` · or `cargo install nannyd`
- **Groq API key** — free tier at [console.groq.com](https://console.groq.com) (no credit card required). Copy `.env.example` to `.env` and fill in `GROQ_API_KEY`.

**Offline fallback:** edit one line in `src/main.rs` to swap the Groq client for Ollama (instructions are in the comment above `run_review()`). Then `ollama pull qwen2.5:7b && ollama serve`.

## Setup

```sh
cd examples/rust/qabud
cp .env.example .env
# Edit .env and set GROQ_API_KEY=<your_key_from_console.groq.com>
# Leave API_KEY=demo-not-a-real-key as-is — it's the sentinel the no_sensitive_files rule demo detects
cargo build --release
```

Build before the first `nanny run`. Nanny's timeout starts when the process launches — if `cargo` compiles during the governed run, the timeout fires before the agent does anything. Building once upfront means `nanny run` launches the already-compiled binary immediately every time.

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
{"event":"ToolAllowed","ts":1700000001000,"tool":"read_file"}
{"event":"StepCompleted","ts":1700000001100,"step":1}
...
{"event":"ExecutionStopped","ts":1700000010000,"reason":"AgentCompleted","steps":8,"cost_spent":80,"elapsed_ms":10000}
```

**Stop reasons you may see:**

| Reason                           | Cause                                                                           |
| -------------------------------- | ------------------------------------------------------------------------------- |
| `BudgetExhausted`                | Hit the 400-unit cost ceiling (10 reads × 10 units each... wait, 40 reads × 10) |
| `RuleDenied: no_sensitive_files` | Agent looped on read_file 3+ times in a row                                     |
| `ToolDenied`                     | Agent tried a tool not in the allowlist (e.g. `write_file`)                     |
| `AgentCompleted`                 | Review finished within limits                                                   |

## Development

This example uses the published `nannyd = "<version>"` crate from crates.io.
During active development on the nanny crate itself, switch to a path dependency:

```toml
# Cargo.toml
nannyd = { path = "../../../crates/cli" }   # instead of nannyd = "0.1.8"
```

The path dep wires this example to the local SDK library. The `nanny` CLI binary (which contains Nanny’s enforcement layer) is separate — reinstall it from local source so both are in sync:

```sh
# from the workspace root (nanny/)

# If nanny was installed via Homebrew, unlink it first so the local build takes precedence:
brew unlink nannyd

cargo install --path crates/cli --force
```

To revert to the published version, restore `nannyd = "0.1.8"` in `Cargo.toml`, then:

```sh
cargo uninstall nannyd
brew link nannyd   # if originally installed via Homebrew
```
