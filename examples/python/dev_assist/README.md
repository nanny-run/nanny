# dev_assist — debug agent

Imagine hiring a detective to investigate a crime scene. The detective reads evidence, follows leads, and delivers a verdict. But without any rules, this detective might spend hours re-reading the same file or chasing every reference forever — racking up an enormous bill in the process.

`dev_assist` is a LangGraph agent that reads a Python stack trace, hunts down the relevant source files, and diagnoses the bug. Nanny plays the role of the detective agency: it sets a case budget and a deadline. When either runs out, the case closes — no exceptions.

---

## What it does

Given a stack trace file, the agent runs a four-node LangGraph pipeline:

1. **Extract** — pulls file paths and symbol names from the trace (pure Python, no tools)
2. **Read files** — reads each extracted path using a governed `file_reader` tool (`@tool(cost=5)`)
3. **Search** — searches for related symbols using ripgrep (`@tool(cost=8)`)
4. **Diagnose** — asks Groq (`llama-3.3-70b-versatile`) to diagnose the bug based on what was found

Python drives every tool call directly. The LLM only reasons in the final synthesis step — enforcement is guaranteed regardless of model behavior.

---

## Prerequisites

- **`nanny` CLI** — macOS: `brew tap nanny-run/nanny && brew install nannyd` · Linux: `curl -fsSL https://install.nanny.run | sh` · Windows: `irm https://install.nanny.run/windows | iex` · or `cargo install nannyd`
- **Groq API key** — free tier at [console.groq.com](https://console.groq.com) (no credit card required). Copy `.env.example` to `.env` and fill in `GROQ_API_KEY`.

---

## Install

```bash
cd examples/python/dev_assist
cp .env.example .env
# Edit .env and set GROQ_API_KEY=<your_key_from_console.groq.com>
uv sync
```

`uv sync` installs all dependencies including `nanny-sdk`. No separate `pip install` needed.

---

## Run under enforcement

```bash
nanny run
```

Reads `[start].cmd` from `nanny.toml` and runs the agent under Nanny governance. The NDJSON event log appears on stdout; the diagnosis appears on stderr. When a limit is hit, Nanny kills the process and prints the stop reason.

To trigger a specific stop reason, edit `nanny.toml` and lower the relevant limit before running:

```bash
# Trigger BudgetExhausted — lower the cost ceiling
nanny run --limits=debugger
```

---

## Run without enforcement (passthrough)

All decorators are no-ops outside `nanny run`. The agent runs normally with no bridge required:

```bash
uv run dev debug --trace fixtures/sample_trace.txt
```

---

## Nanny features demonstrated

| Feature                          | What it does                                                               |
| -------------------------------- | -------------------------------------------------------------------------- |
| `@tool(cost=5)` on `file_reader` | Each file read charges 5 cost units; tracked against the budget            |
| `@tool(cost=8)` on `ripgrep`     | Each search charges 8 cost units                                           |
| `@rule("no_read_loop")`          | Stops the agent if the last 5 calls were all `file_reader` — loop detected |
| `@agent("debugger")`             | Activates `[limits.debugger]` from `nanny.toml` on entry; reverts on exit  |

---

## Demo

`ToolDenied` — agent attempts `write_file`, which is not in the allowlist:

![dev_assist — ToolDenied fires when the agent calls write_file](../../../assets/demo/dev-assist-tool-denied.gif)

---

## Stop reasons you may see

| Reason                     | What caused it                                               |
| -------------------------- | ------------------------------------------------------------ |
| `BudgetExhausted`          | Hit the cost ceiling before finishing the diagnosis          |
| `RuleDenied: no_read_loop` | Agent kept re-reading the same files without making progress |
| `ToolDenied`               | Agent tried `write_file` — not in the allowlist              |
| `AgentCompleted`           | Diagnosis produced within all limits                         |

## Development

This example uses the published `nanny-sdk` package from PyPI.
During active development on the nanny SDK itself, switch to a path dependency:

```toml
# pyproject.toml
[tool.uv.sources]
nanny-sdk = { path = "../../../sdks/python" }   # instead of nanny-sdk==<version>
```

Then run `uv sync` to install from the local source.

The `[tool.uv.sources]` override wires this example to the local SDK. The `nanny` CLI binary (which contains the bridge) is separate — reinstall it from local source so both are in sync:

```sh
# from the workspace root (nanny/)

# If nanny was installed via Homebrew, unlink it first so the local build takes precedence:
brew unlink nannyd

cargo install --path crates/cli --force
```

To switch back to the published version, remove the `[tool.uv.sources]` block and pin the version in `dependencies`:

```toml
# pyproject.toml
dependencies = [
    ...
    "nanny-sdk==0.1.8",   # pin to the published release
]
```

Then run `uv sync` again. Also restore the published `nanny` CLI:

```sh
cargo uninstall nannyd
brew link nannyd   # if originally installed via Homebrew
```
