# metrics_crew — incident analysis pipeline

Imagine a hospital emergency room. When a patient comes in, a team of specialists works in sequence: intake runs the initial tests, diagnostics finds what's wrong, radiology produces the scans, and the attending physician writes the final report. Each specialist has their own budget and their own tools — the radiologist can't order blood work, and the diagnostician can't write the discharge summary. There's also a hospital-wide spending cap that applies regardless of what any individual specialist is doing.

`metrics_crew` is a CrewAI pipeline that investigates a production incident from a server metrics CSV. Four agents work in sequence. Nanny plays the role of hospital administration: each specialist gets their own spending limit and their own tool access. When any limit is hit, the case closes immediately.

This is the canonical example of least-privilege multi-agent governance with Nanny.

---

## The governance story

In most multi-agent systems, governance is an afterthought. You get a global timeout and hope for the best. There's no per-role budget, no per-role tool access, no audit trail of which agent made which call.

`metrics_crew` shows what proper multi-agent governance looks like:

- **The analysis agent cannot call `write_report`.** If it tries — because the model hallucinated a tool call, or because you wired something wrong — `ToolDenied` fires immediately. The file is never written. The cost is never charged.
- **The reporter agent cannot call `compute_stats`.** Same story. Wrong tool for the role, instant stop.
- **If the analysis agent runs `compute_stats` five times in a row on the same metric**, the `no_analysis_loop` rule fires before the sixth call executes. The agent was stuck. Nanny stopped it. You get a log entry showing exactly why.
- **Each agent has its own cost ceiling.** Hitting the analysis budget does not kill the reporter. The pipeline continues with the agents that haven't exhausted their limits.
- **Every call is in the audit log.** Every `ToolAllowed`, every `StepCompleted`, every `ExecutionStopped` — structured NDJSON on stdout from the moment the process starts to the moment it ends.

This is 200 lines of Python showing the full pattern. Read the source in `metrics_crew/crew.py`, `metrics_crew/agents/`, and `metrics_crew/tools/`.

---

## What it does

Given a CSV of server metrics (CPU, memory, request rate, error rate, latency), the pipeline:

1. **Ingestion agent** — loads and validates the data, confirms available signals and date range
2. **Analysis agent** — detects anomalies using Z-score analysis and correlates affected signals
3. **Visualization agent** — generates interactive Plotly HTML charts for each anomalous signal
4. **Reporter agent** — writes a structured Markdown incident report linking to the charts

Output: HTML charts in `reports/` and an incident report Markdown file.

---

## Prerequisites

- **`nanny` CLI** — macOS: `brew tap nanny-run/nanny && brew install nannyd` · Linux: `curl -fsSL https://install.nanny.run | sh` · Windows: `irm https://install.nanny.run/windows | iex` · or `cargo install nannyd`
- **Groq API key** — free tier at [console.groq.com](https://console.groq.com) (no credit card required). Copy `.env.example` to `.env` and fill in `GROQ_API_KEY`.

---

## Install

```bash
cd examples/python/metrics_crew
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

Reads `[start].cmd` from `nanny.toml` and runs the full four-agent pipeline under Nanny governance. Charts are written to `reports/`. The NDJSON event log goes to stdout.

---

## Run without enforcement (passthrough)

All decorators are no-ops outside `nanny run`. The full pipeline runs normally with no bridge required:

```bash
uv run metrics-crew analyze --data fixtures/sample_metrics.csv
```

---

## Nanny features demonstrated

| Feature                      | What it does                                                                                                               |
| ---------------------------- | -------------------------------------------------------------------------------------------------------------------------- |
| `@tool(cost=N)` on each tool | Each tool call charges its declared cost against the active budget                                                         |
| Per-role limits              | `[limits.ingestion]`, `[limits.analysis]`, `[limits.visualization]`, `[limits.reporter]` — each agent gets its own ceiling |
| Per-role tool allowlists     | Each agent only receives the tools it needs; calling another role's tool raises `ToolDenied`                               |
| `@rule("no_analysis_loop")`  | Stops if `compute_stats` is called 5+ times in a row — prevents the analysis agent from looping on the same metric         |

---

## Demos

Multi-agent scopes entering and exiting with live NDJSON enforcement events:

![metrics_crew running under nanny run — budget exhausted stops the analysis agent mid-run](../../../assets/demo/metrics-crew-budget-exhausted.gif)

`ToolDenied` — analysis agent reaches for the reporter's `write_report` tool and is stopped immediately:

![metrics_crew — ToolDenied fires when the analysis agent calls write_report](../../../assets/demo/metrics-crew-tool-denied.gif)

---

## Stop reasons you may see

| Reason                         | What caused it                                                                                   |
| ------------------------------ | ------------------------------------------------------------------------------------------------ |
| `BudgetExhausted`              | Hit the cost ceiling during analysis before all signals were checked                             |
| `RuleDenied: no_analysis_loop` | Analysis agent kept re-running `compute_stats` on the same metric                                |
| `ToolDenied`                   | An agent tried to call a tool outside its allowlist (e.g. analysis agent calling `write_report`) |
| `AgentCompleted`               | All four agents finished within their limits; charts and report produced                         |

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
