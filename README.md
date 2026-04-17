<p align="center">
  <picture>
    <source media="(prefers-color-scheme: dark)" srcset="assets/nanny-logo-dark.svg" />
    <source media="(prefers-color-scheme: light)" srcset="assets/nanny-logo-light.svg" />
    <img src="assets/nanny-logo-light.svg" alt="Nanny" height="80" />
  </picture>
</p>

<p align="center">
  <strong>Open-source execution boundary for autonomous systems.</strong><br/>
  Hard limits. Deterministic stops. Structured audit trail.
</p>

<p align="center">
  <a href="LICENSE"><img src="https://img.shields.io/badge/License-Apache%202.0-blue.svg" alt="Apache 2.0" /></a>
  <a href="https://crates.io/crates/nannyd"><img src="https://img.shields.io/crates/v/nannyd?logo=rust&label=crates.io" alt="crates.io" /></a>
  <a href="https://pypi.org/project/nanny-sdk/"><img src="https://img.shields.io/pypi/v/nanny-sdk?logo=python&label=pypi" alt="PyPI" /></a>
  <a href="https://github.com/nanny-run/nanny/releases"><img src="https://img.shields.io/github/v/release/nanny-run/nanny?logo=github&label=release" alt="GitHub Release" /></a>
  <a href="https://github.com/nanny-run/nanny/actions/workflows/ci-rust.yml"><img src="https://img.shields.io/github/actions/workflow/status/nanny-run/nanny/ci-rust.yml?logo=github&label=CI" alt="CI" /></a>
  <a href="https://github.com/nanny-run/nanny/pulls"><img src="https://img.shields.io/badge/PRs-welcome-brightgreen.svg" alt="PRs Welcome" /></a>
</p>

<p align="center">
  <a href="https://docs.nanny.run">Documentation</a> ·
  <a href="https://docs.nanny.run/quickstart">Quickstart</a> ·
  <a href="CHANGELOG.md">Changelog</a> ·
  <a href="https://github.com/nanny-run/nanny/issues">Report a Bug</a> ·
  <a href="CONTRIBUTING.md">Contributing</a>
</p>

---

## What is Nanny?

Agents spend money. They call tools in loops. They run forever. They go over budget.

Nanny is the thing that stops them.

You tell nanny "this agent is allowed 100 steps, 1000 cost units, and 30 seconds." The moment any limit is crossed, nanny kills the process immediately, emits a structured event log saying exactly why it stopped, and exits. No grace period. No recovery logic. No second chances.

Think of it as a **hard execution boundary** — deterministic, auditable, and structurally impossible for the agent to bypass.

```mermaid
flowchart TD
    CMD(["nanny run"])
    CMD --> NANNY

    subgraph NANNY["Nanny — parent process"]
        direction LR

        subgraph CHILD["Child process"]
            AGENT["python agent.py"]
        end

        subgraph ENFORCE[" "]
            direction TB
            STEPS["steps"]
            COST["cost"]
            TIMER["timeout"]
        end

        AGENT -- "tool call" --> ENFORCE
        ENFORCE -- "✓  allowed" --> AGENT
    end

    ENFORCE -- "✗  limit reached → killed" --> DEAD(["process exits"])
    DEAD --> LOG["ExecutionStopped\nreason · steps · cost_spent\n→ stdout"]
```

---

## The Nanny ecosystem

| Layer                      | What it does                                                                      |
| -------------------------- | --------------------------------------------------------------------------------- |
| **Nanny CLI**              | Hard timeout, step, and cost limits for any agent process in any language.        |
| **Rust SDK**               | Per-function cost metering, allowlist enforcement, and custom rules — in-process. |
| **Python SDK**             | The same `@tool`, `@rule`, `@agent` model as Python decorators.                   |
| **Nanny Cloud** _(v0.1.6)_ | Durable audit logs, team dashboards, org-level budget aggregation.                |

→ Full docs at [docs.nanny.run](https://docs.nanny.run)

---

## Sample applications

Four complete agent samples ship in `examples/`. All use [Ollama](https://ollama.com) — no API key required.

| Sample                                                         | What it does                                                                                                                | Stop reasons demonstrated                     |
| -------------------------------------------------------------- | --------------------------------------------------------------------------------------------------------------------------- | --------------------------------------------- |
| [`examples/rust/webdingo`](examples/rust/webdingo)             | Web research agent (Rust) — fetches pages, synthesises a report. Classic spiral risk.                                       | `BudgetExhausted`, `RuleDenied`               |
| [`examples/rust/qabud`](examples/rust/qabud)                   | Code review agent (Rust) — reads source files, identifies issues, blocks sensitive files before they're opened.             | `RuleDenied`, `ToolDenied`, `MaxStepsReached` |
| [`examples/python/dev_assist`](examples/python/dev_assist)     | Debug agent (LangChain) — given a stack trace, reads relevant files and searches for related symbols.                       | `BudgetExhausted`, `RuleDenied`, `ToolDenied` |
| [`examples/python/metrics_crew`](examples/python/metrics_crew) | Incident pipeline (CrewAI) — four agents analyse server metrics, generate interactive charts, and write an incident report. | `BudgetExhausted`, `RuleDenied`, `ToolDenied` |

```bash
# Rust examples
cd examples/rust/webdingo && nanny run -- "best Rust HTTP clients"
cd examples/rust/qabud && nanny run -- ./src

# Python examples
cd examples/python/dev_assist && nanny run
cd examples/python/metrics_crew && nanny run
```

---

## Install

The Nanny CLI is a **system tool** — install it once globally and use `nanny run` from any project that has a `nanny.toml`.

**macOS**

```sh
brew tap nanny-run/nanny
brew install nannyd
```

**Linux**

```sh
curl -fsSL https://install.nanny.run | sh
```

**All platforms — via Rust toolchain**

```sh
cargo install nannyd
```

Or download a pre-built binary directly from [GitHub Releases](https://github.com/nanny-run/nanny/releases).

> **Windows note:** Process enforcement (hard kill on limit breach) requires Unix signal support and is not yet implemented on Windows. The CLI and SDK otherwise work correctly.

---

## SDK installation

SDKs are **project dependencies** — add them per project, not globally.

**Rust**

```sh
cargo add nannyd
```

**Python**

```sh
pip install nanny-sdk
```

---

## 60-second quickstart

```sh
# 1. Scaffold a nanny.toml in your project root
nanny init

# 2. Run your agent
nanny run

# 3. Use a named limit set for specific workloads
nanny run --limits=researcher
```

**nanny.toml:**

```toml
[runtime]
mode = "local"

[start]
cmd = "python agent.py"   # nanny run always reads this

[limits]
steps   = 100     # max tool calls
cost    = 1000    # max cost units
timeout = 30000   # wall-clock ms

[limits.researcher]
steps   = 200
cost    = 5000
timeout = 120000

[tools]
allowed = ["http_get", "read_file"]   # anything not listed is denied
```

![Nanny demo — BudgetExhausted stops a web research agent mid-run](assets/demo/webdingo-budget-exhausted.gif)

---

## Rust SDK — all three macros

For Rust agents, annotate functions directly to get per-function cost accounting,
allowlist enforcement, and custom policy rules:

```rust
use nannyd::{tool, rule, agent, PolicyContext};

/// Each call charges 10 cost units and requires the tool to be in the allowlist.
#[nanny::tool(cost = 10)]
fn search_web(query: String) -> String {
    // ... HTTP request ...
    String::new()
}

/// Return false to stop the agent immediately with RuleDenied.
#[nanny::rule("no_spiral")]
fn check_spiral(ctx: &PolicyContext) -> bool {
    let h = &ctx.tool_call_history;
    // Stop if the last 3 calls were all search_web
    !(h.len() >= 3 && h.iter().rev().take(3).all(|t| t == "search_web"))
}

/// Activates [limits.researcher] for the duration of this function.
/// Limits revert automatically on return, even if the function panics.
#[nanny::agent("researcher")]
async fn run_research(topic: &str) {
    // ... agent loop — search_web governed by nanny ...
}
```

All macros are no-ops when running outside `nanny run` — no enforcement overhead.

![Nanny demo — named agent scopes (planner → researcher → synthesizer) entering and exiting](assets/demo/webdingo-agent-scopes.gif)

→ Full Rust SDK guide at [docs.nanny.run/guides/rust-sdk](https://docs.nanny.run/guides/rust-sdk)

---

## Python SDK — all three decorators

For Python agents, the same model as the Rust SDK — as decorators:

```python
from nanny_sdk import tool, rule, agent

@tool(cost=10)
def search_web(query: str) -> str:
    import httpx
    return httpx.get(f"https://en.wikipedia.org/wiki/{query}").text

@rule("no_spiral")
def check_spiral(ctx) -> bool:
    h = ctx.tool_call_history
    return not (len(h) >= 3 and len(set(h[-3:])) == 1)

@agent("researcher")
def run_research(topic: str) -> list[str]:
    # Runs under [limits.researcher] from nanny.toml
    return [search_web(topic)]
```

Works with any framework — LangChain, CrewAI, plain Python. Stack decorators to combine framework registration with Nanny governance:

```python
from langchain_core.tools import tool as lc_tool
from nanny_sdk import tool as nanny_tool

@lc_tool                   # outer — LangChain registers for dispatch
@nanny_tool(cost=5)        # inner — Nanny intercepts before the function runs
def read_file(path: str) -> str:
    with open(path) as f:
        return f.read()
```

All decorators are no-ops when running outside `nanny run` — zero overhead in development and CI.

→ Full Python SDK guide at [docs.nanny.run/guides/python-sdk](https://docs.nanny.run/guides/python-sdk)

---

## Event log

Every run emits NDJSON to stdout. One event per line. Always starts with `ExecutionStarted`, always ends with `ExecutionStopped`.

```json
{"event":"ExecutionStarted","ts":1711234567000,"limits":{"steps":100,"cost":1000,"timeout":30000},"limits_set":"[limits]","command":"python agent.py"}
{"event":"ToolAllowed","ts":1711234567120,"tool":"http_get"}
{"event":"StepCompleted","ts":1711234567800,"step":1}
{"event":"ExecutionStopped","ts":1711234572000,"reason":"BudgetExhausted","steps":12,"cost_spent":1000,"elapsed_ms":5000}
```

Pipe it to a file, stream it to your log aggregator, or query it inline:

```sh
nanny run > nanny.log
nanny run | tee nanny.log
```

---

## Documentation

Full reference at **[docs.nanny.run](https://docs.nanny.run)** — quickstart, concepts, CLI reference, `nanny.toml` schema, event log, Rust SDK guide, and Python SDK guide.

---

## Contributing

See [CONTRIBUTING.md](CONTRIBUTING.md).

---

## License

Apache-2.0 — see [LICENSE](LICENSE).
