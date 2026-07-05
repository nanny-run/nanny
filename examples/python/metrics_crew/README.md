# metrics_crew — incident analysis pipeline

Imagine a hospital emergency room. When a patient comes in, a team of specialists works in sequence: intake runs the initial tests, diagnostics finds what's wrong, radiology produces the scans, and the attending physician writes the final report. Each specialist has their own budget and their own tools — the radiologist can't order blood work, and the diagnostician can't write the discharge summary. There's also a hospital-wide spending cap that applies regardless of what any individual specialist is doing.

`metrics_crew` is a CrewAI pipeline that investigates a production incident from a server metrics CSV. Four agents work in sequence. Nanny plays the role of hospital administration: each specialist gets their own spending limit and their own tool access. When any limit is hit, the case closes immediately.

This is the canonical example of least-privilege multi-agent governance with Nanny.

---

## The governance story

In most multi-agent systems, governance is an afterthought. You get a global timeout and hope for the best. There's no per-role budget, no per-role tool access, no audit trail of which agent made which call.

`metrics_crew` shows what proper multi-agent governance looks like:

- **The analysis agent cannot call `write_report`.** If it tries — because the model hallucinated a tool call, or because you wired something wrong — `ToolDenied` fires immediately. The file is never written. No tokens are charged.
- **The reporter agent cannot call `compute_stats`.** Same story. Wrong tool for the role, instant stop.
- **If the analysis agent runs `compute_stats` five times in a row on the same metric**, the `no_analysis_loop` rule fires before the sixth call executes. The agent was stuck. Nanny stopped it. You get a log entry showing exactly why.
- **Each agent has its own token ceiling.** Hitting the analysis budget does not kill the reporter. The pipeline continues with the agents that haven't exhausted their limits.
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
- **OpenAI API key** — get yours at [platform.openai.com/api-keys](https://platform.openai.com/api-keys). Copy `.env.example` to `.env` and fill in `OPENAI_API_KEY`.

---

## Install

```bash
cd examples/python/metrics_crew
cp .env.example .env
# Edit .env and set OPENAI_API_KEY=<your_key_from_platform.openai.com>
uv sync
```

---

## Run under enforcement (server mode)

```bash
nanny run
```

Reads `[start].cmd` from `nanny.toml` and starts a FastAPI server at `http://localhost:8080`, wrapped by Nanny governance. The NDJSON event log goes to stdout and `nanny.log`.

Submit a CSV and poll for results:

```bash
# Submit a job
JOB=$(curl -s -X POST http://localhost:8080/analyze \
  -F "file=@fixtures/sample_metrics.csv" | jq -r .job_id)

# Poll until done
curl http://localhost:8080/jobs/$JOB
```

### API endpoints

| Method | Path | Description |
|--------|------|-------------|
| `GET` | `/health` | Liveness probe |
| `POST` | `/analyze` | Upload a metrics CSV — returns `{"job_id": "..."}` |
| `GET` | `/jobs/{job_id}` | Poll status: `queued` → `running` → `done` / `stopped` / `failed` |

### Demo scenarios

Trigger specific Nanny stop reasons without waiting for a real limit to be hit:

```bash
# BudgetExhausted — activates [limits.demo-budget] (100 tokens)
curl -s -X POST "http://localhost:8080/analyze?scenario=demo-budget" \
  -F "file=@fixtures/sample_metrics.csv" | jq .job_id

# MaxStepsReached — activates [limits.demo-steps] (4 steps)
curl -s -X POST "http://localhost:8080/analyze?scenario=demo-steps" \
  -F "file=@fixtures/sample_metrics.csv" | jq .job_id
```

---

## Run without enforcement (passthrough)

All decorators are no-ops outside `nanny run`. Run the CLI directly:

```bash
uv run metrics-crew analyze --data fixtures/sample_metrics.csv
```

Or start the server without the Nanny wrapper:

```bash
uvicorn metrics_crew.api:app --reload --port 8080
```

---

## Nanny features demonstrated

| Feature | What it does |
|---------|-------------|
| `@tool(tokens=N)` on each tool | Each tool call charges its declared tokens against the active budget |
| Per-role limits | `[limits.ingestion]`, `[limits.analysis]`, `[limits.visualization]`, `[limits.reporter]` — each agent gets its own ceiling |
| Per-role tool allowlists | Each agent only receives the tools it needs; calling another role's tool raises `ToolDenied` |
| `@rule("no_analysis_loop")` | Stops if `compute_stats` is called 5+ times in a row — prevents the analysis agent from looping on the same metric |
| Demo limit sets | `[limits.demo-budget]` and `[limits.demo-steps]` trigger governed stops on demand via `?scenario=` |

---

## Demos

Multi-agent scopes entering and exiting with live NDJSON enforcement events:

![metrics_crew running under nanny run — budget exhausted stops the analysis agent mid-run](../../../assets/demo/metrics-crew-budget-exhausted.gif)

`ToolDenied` — analysis agent reaches for the reporter's `write_report` tool and is stopped immediately:

![metrics_crew — ToolDenied fires when the analysis agent calls write_report](../../../assets/demo/metrics-crew-tool-denied.gif)

---

## Stop reasons you may see

| Reason | What caused it |
|--------|---------------|
| `BudgetExhausted` | Hit the token ceiling during analysis before all signals were checked |
| `RuleDenied: no_analysis_loop` | Analysis agent kept re-running `compute_stats` on the same metric |
| `ToolDenied` | An agent tried to call a tool outside its allowlist (e.g. analysis agent calling `write_report`) |
| `MaxStepsReached` | Hit the step ceiling — use `[limits.demo-steps]` to trigger this deliberately |
| `AgentCompleted` | All four agents finished within their limits; charts and report produced |

---

## Development

### Testing against local builds (pre-publish)

To test a local nanny build before publishing:

**1. Install the local nanny binary:**
```bash
# From the workspace root (nanny/)
brew unlink nannyd   # if installed via Homebrew — prevents PATH conflict
cargo install --path crates/cli --force
```

**2. Point the SDK at local source — uncomment in `pyproject.toml`:**
```toml
[tool.uv.sources]
nanny-sdk = { path = "../../../sdks/python" }
```

**3. Sync and run:**
```bash
uv sync
nanny run
```

`nanny run` now uses the binary from `~/.cargo/bin/nanny` (local build) and the SDK from `sdks/python/` (local source).

### Switching back to published versions

Re-comment the `[tool.uv.sources]` block, run `uv sync`, then restore the published binary:

```bash
cargo uninstall nannyd
brew link nannyd   # if originally installed via Homebrew
```
