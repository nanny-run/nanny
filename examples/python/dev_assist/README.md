# dev_assist — debug agent

Imagine hiring a detective to investigate a crime scene. The detective reads evidence, follows leads, and delivers a verdict. But without any rules, this detective might spend hours re-reading the same file or chasing every reference forever — racking up an enormous bill in the process.

`dev_assist` is a LangChain agent that reads a Python stack trace, hunts down the relevant source files, and diagnoses the bug. Nanny plays the role of the detective agency: it sets a case budget and a deadline. When either runs out, the case closes — no exceptions.

---

## What it does

Given a stack trace file, the agent:

1. Extracts file paths mentioned in the trace
2. Reads those files using a governed `file_reader` tool (`@tool(cost=5)`)
3. Searches the codebase for related symbols using ripgrep (`@tool(cost=8)`)
4. Asks a local LLM (Ollama) to diagnose the bug based on what it found
5. Prints the diagnosis to the terminal

Two modes:

- `--mode react` (default) — the agent reasons step-by-step, deciding what to read next as it goes
- `--mode plan` — the agent plans first (which files, which searches), then executes deterministically

---

## Prerequisites

- **`nanny` CLI** — macOS: `brew tap nanny-run/nanny && brew install nannyd` · Linux: `curl -fsSL https://install.nanny.run | sh` · Windows: `irm https://install.nanny.run/windows | iex` · or `cargo install nannyd`
- **Ollama** — `brew install ollama && ollama serve` (keep it running in a separate terminal)
- **`llama3.1:8b` model** — `ollama pull llama3.1:8b`

---

## Install

```bash
cd examples/python/dev_assist
pip install nanny-sdk
uv sync
```

---

## Run under enforcement

```bash
nanny run
```

Reads `[start].cmd` from `nanny.toml` and runs the agent under Nanny governance. The NDJSON event log appears on stdout; the diagnosis appears on stderr. When a limit is hit, Nanny kills the process and prints the stop reason.

To trigger a specific stop reason, edit `nanny.toml` and lower the relevant limit before running:

```bash
# Trigger BudgetExhausted — lower the cost ceiling
nanny run --limits=budget-demo

# Trigger RuleDenied — run against a file that makes the agent loop
nanny run --limits=loop-demo
```

---

## Run without enforcement (passthrough)

All decorators are no-ops outside `nanny run`. The agent runs normally with no bridge required:

```bash
uv run dev debug --trace fixtures/sample_trace.txt
uv run dev debug --trace fixtures/sample_trace.txt --mode plan
```

---

## Nanny features demonstrated

| Feature | What it does |
| ------- | ------------ |
| `@tool(cost=5)` on `file_reader` | Each file read charges 5 cost units; tracked against the budget |
| `@tool(cost=8)` on `ripgrep` | Each search charges 8 cost units |
| `@rule("no_read_loop")` | Stops the agent if the last 5 calls were all `file_reader` — loop detected |
| `@agent("debugger")` | Activates `[limits.debugger]` from `nanny.toml` on entry; reverts on exit |

---

## Stop reasons you may see

| Reason | What caused it |
| ------ | -------------- |
| `BudgetExhausted` | Hit the cost ceiling before finishing the diagnosis |
| `RuleDenied: no_read_loop` | Agent kept re-reading the same files without making progress |
| `ToolDenied` | Agent tried `write_file` — not in the allowlist |
| `AgentCompleted` | Diagnosis produced within all limits |
