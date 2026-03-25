<p align="center">
  <picture>
    <source media="(prefers-color-scheme: dark)" srcset="assets/nanny-logo-dark.svg" />
    <source media="(prefers-color-scheme: light)" srcset="assets/nanny-logo-light.svg" />
    <img src="assets/nanny-logo-light.svg" alt="Nanny" height="80" />
  </picture>
</p>

<p align="center">
  <strong>Execution boundary for autonomous systems.</strong><br/>
  Hard limits. Deterministic stops. Structured audit trail. No code changes required.
</p>

<p align="center">
  <a href="LICENSE"><img src="https://img.shields.io/badge/License-Apache%202.0-blue.svg" alt="Apache 2.0" /></a>
  <a href="https://crates.io/crates/nannyd"><img src="https://img.shields.io/crates/v/nannyd?logo=rust&label=crates.io" alt="crates.io" /></a>
  <a href="https://pypi.org/project/nanny-sdk/"><img src="https://img.shields.io/pypi/v/nanny-sdk?logo=python&label=pypi" alt="PyPI" /></a>
  <a href="https://github.com/nanny-run/nanny/releases"><img src="https://img.shields.io/github/v/release/nanny-run/nanny?logo=github&label=release" alt="GitHub Release" /></a>
  <!-- <a href="https://github.com/nanny-run/nanny/actions/workflows/ci.yml"><img src="https://img.shields.io/github/actions/workflow/status/nanny-run/nanny/ci.yml?logo=github&label=CI" alt="CI" /></a> -->
  <a href="https://github.com/nanny-run/nanny/pulls"><img src="https://img.shields.io/badge/PRs-welcome-brightgreen.svg" alt="PRs Welcome" /></a>
  <!-- <a href="https://x.com/nanny_run"><img src="https://img.shields.io/twitter/follow/nanny_run?logo=x&color=%23000000" alt="X" /></a> -->
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

Think of it as a circuit breaker for autonomous systems — deterministic, auditable, and completely decoupled from the agent itself.

---

## Who is it for?

Nanny is for developers and teams running agents in production — or preparing to.

It is a good fit if you:

- Are running **autonomous agents** that call external tools, browse the web, or write to APIs
- Need hard guarantees that an agent **cannot exceed a cost budget or run indefinitely**
- Want a **structured audit trail** of every tool call and stop reason for every execution
- Are building with **CrewAI, LangChain, or any Python or Rust agent framework**
- Want enforcement that is **decoupled from your agent code** — no lock-in, no wrapper framework

Nanny may not be what you need if you're running simple scripts, batch jobs, or anything without autonomous tool-calling behaviour.

---

## The Nanny ecosystem

Nanny is designed to meet you where you are and grow with you.

**nanny CLI** — The universal starting point. Wraps any agent process in any language. Zero code changes required.

```sh
# Python agent
nanny run python agent.py

# Rust agent
nanny run ./my-agent
```

**Rust SDK** — For Rust agents, go deeper. Annotate individual functions to get per-function cost accounting, allowlist enforcement, and custom rules — all in-process, all with zero overhead when running outside `nanny run`.

```rust
use nanny::{tool, rule, agent};

#[tool(cost = 10)]
fn search_web(query: &str) -> String { ... }

#[agent("researcher")]
fn run_research(topic: &str) { ... }
```

**Python SDK** _(coming v0.2.0)_ — The same `#[tool]`, `#[rule]`, `#[agent]` model, as Python decorators. This is the public launch milestone — Python is where the majority of agent development happens.

**Nanny Cloud** _(coming v0.3.0)_ — Durable audit logs, team dashboards, org-level budget aggregation, and managed enforcement across all your agents. The OSS runtime stays unchanged — Cloud is the layer above it.

---

## Install

The nanny CLI is a **system tool** — install it once globally and use `nanny run` from any project that has a `nanny.toml`.

**macOS**

```sh
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

> **Windows note:** Process enforcement (hard kill on limit breach) requires Unix signal support and is not yet implemented on Windows. The CLI and SDK bridge otherwise work correctly.

---

## SDK installation

SDKs are **project dependencies** — add them per project, not globally.

**Rust**

```sh
cargo add nannyd
```

**Python** _(v0.2.0 — coming soon)_

```sh
pip install nanny-sdk
```

---

## 60-second quickstart

```sh
# 1. Scaffold a nanny.toml in your project root
nanny init

# 2. Run your agent under enforcement
nanny run python agent.py

# 3. Use a named limit set for specific workloads
nanny run --limits=researcher python agent.py
```

**nanny.toml:**

```toml
[runtime]
mode = "local"

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
nanny run python agent.py > nanny.log
nanny run python agent.py | tee nanny.log
```

---

## Documentation

Full reference at **[docs.nanny.run](https://docs.nanny.run)** — quickstart, concepts, CLI reference, `nanny.toml` schema, event log, and Rust SDK guide.

---

## Contributing

See [CONTRIBUTING.md](CONTRIBUTING.md).

---

## License

Apache-2.0 — see [LICENSE](LICENSE).
