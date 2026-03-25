# nanny

**Execution boundary for autonomous systems.**

Nanny enforces hard limits on agents and long-running processes. It deterministically stops execution when a limit is reached — no intelligence, no exceptions, no recovery logic inside the engine.

Think of it like a circuit breaker. You tell it "this agent is allowed 100 steps, 1000 cost units, and 30 seconds." The moment any limit is crossed, the process is killed and a structured log says exactly why.

```
nanny run python agent.py
nanny run node agent.js
nanny run ./my-agent
```

Any language. Any binary. Zero code changes required.

---

## Install

```sh
cargo install nanny
```

---

## Two modes of use

### Mode 1 — Zero code changes (CLI wrapper)

Nanny wraps any binary in any language. The child process is spawned, a bridge sidecar runs alongside it, and when a limit is hit the child is killed. Structured events go to stdout as NDJSON.

```sh
nanny init
nanny run python agent.py
nanny run --limits=researcher python researcher.py
```

### Mode 2 — Rust SDK (in-process, fine-grained)

Annotate your Rust functions directly with proc-macro attributes:

```rust
use nanny::{tool, rule, agent, PolicyContext};

// Declare a tool — the bridge charges 10 cost units and checks the allowlist.
#[tool(cost = 10)]
fn search_web(query: &str) -> String {
    // your implementation
}

// Register a local rule — fires before every tool call.
#[rule("no_spiral")]
fn check_spiral(ctx: &PolicyContext) -> bool {
    let h = &ctx.tool_call_history;
    !(h.len() >= 3 && h[h.len()-3..].iter().all(|t| *t == h[h.len()-1]))
}

// Activate named limits for a scope — auto-reverted on drop/panic.
#[agent("researcher")]
fn run_research(topic: &str) {
    // [limits.researcher] is active for the duration of this function
}
```

**Key property:** when `NANNY_BRIDGE_SOCKET` / `NANNY_BRIDGE_PORT` env vars are absent, all three macros are pure no-ops. Zero overhead, zero behavior change. They only activate when running under `nanny run`.

---

## Quickstart

**1. Initialize config in your project:**

```sh
nanny init
```

This writes a `nanny.toml` with safe default limits.

**2. Run your agent under enforcement:**

```sh
nanny run python agent.py
nanny run --limits=researcher python researcher.py
```

Nanny wraps the process, enforces limits, and emits a structured event log. When a limit is hit, the process is killed and nanny exits non-zero.

---

## nanny.toml

```toml
[runtime]
mode = "local"       # "local" (process enforcement) or "managed" (cloud-connected)

[limits]
steps   = 100        # max steps before forced stop
cost    = 1000       # max cost units before forced stop
timeout = 30000      # ms — wall-clock timeout, regardless of activity

[tools]
allowed = ["http_get", "write_file"]   # allowlist — anything else is denied

[observability]
log = "stdout"       # "stdout" or "file"
# log_file = "nanny.ndjson"   # required when log = "file"
```

---

## Named limits

Define multiple limit sets in one file and activate by name at runtime:

```toml
[limits]
steps   = 50
cost    = 500
timeout = 15000

[limits.researcher]
steps = 200
cost  = 2000
# timeout not set — inherits 15000 from [limits]

[limits.fast]
timeout = 5000
# steps and cost inherit from [limits]
```

```sh
nanny run --limits=researcher python researcher.py
nanny run --limits=fast       python quick_task.py
```

Named limit sets inherit all fields from `[limits]` and override only what they declare. No surprises.

---

## Event log

Every execution emits NDJSON to stdout (or a file). One JSON object per line.

```json
{"event":"ExecutionStarted","ts":1711234567000,"limits":{"steps":100,"cost":1000,"timeout":30000},"limits_set":"[limits]","command":"python agent.py"}
{"event":"ToolAllowed","ts":1711234567120,"tool":"http_get"}
{"event":"StepCompleted","ts":1711234567800,"step":1}
{"event":"ExecutionStopped","ts":1711234568432,"reason":"AgentCompleted","steps":7,"cost_spent":7,"elapsed_ms":1432}
```

`ExecutionStarted` is always the first line. `ExecutionStopped` is always the last.

**Stop reasons:**

| Reason            | Meaning                                       |
| ----------------- | --------------------------------------------- |
| `AgentCompleted`  | Process exited cleanly on its own             |
| `TimeoutExpired`  | Wall-clock timeout reached — process killed   |
| `MaxStepsReached` | Step limit reached                            |
| `BudgetExhausted` | Cost unit budget spent                        |
| `ToolDenied`      | Tool call not on the allowlist                |
| `RuleDenied`      | Per-tool rule fired (e.g. max calls exceeded) |
| `ManualStop`      | Stopped programmatically                      |

---

## Per-tool config

Override cost or call limits per tool:

```toml
[tools.http_get]
cost_per_call = 5     # override declared cost for this tool
max_calls     = 20    # deny after 20 calls in one execution
```

---

## Pipe the log

```sh
# stream events to a file while watching the agent run
nanny run python agent.py | tee agent.ndjson

# write events directly to a file
# set log = "file" and log_file = "nanny.ndjson" in nanny.toml
nanny run python agent.py
```

---

## Exit codes

| Code | Meaning                                                       |
| ---- | ------------------------------------------------------------- |
| `0`  | Process exited cleanly (`AgentCompleted`)                     |
| `1`  | Any enforced stop (`TimeoutExpired`, `BudgetExhausted`, etc.) |
| `1`  | Config error or spawn failure                                 |

---

## Architecture

```
nanny run python agent.py
    │
    ├── spawns child process (agent.py)
    │       └── env: NANNY_BRIDGE_SOCKET + NANNY_SESSION_TOKEN
    │
    └── runs Bridge (Unix socket / TCP sidecar)
            │
            ├── POST /tool/call      ← #[nanny::tool] calls this per invocation
            ├── POST /step           ← increments step counter
            ├── POST /agent/enter    ← #[nanny::agent] swaps to named limits
            ├── POST /agent/exit     ← reverts to global limits
            ├── GET  /status         ← read tool counts, cost, steps
            └── GET  /events         ← NDJSON stream drained by CLI to stdout
```

**Crate map:**

| Crate           | Job                                                                                           |
| --------------- | --------------------------------------------------------------------------------------------- |
| `nanny-core`    | Traits (`Policy`, `Ledger`, `ToolExecutor`); the `ExecutionEvent` type                        |
| `nanny-runtime` | Concrete impls: `FakeLedger`, `LimitsPolicy`, `RuleEvaluator`, `ToolRegistry`, built-in tools |
| `nanny-bridge`  | HTTP sidecar (Unix socket / TCP); holds all execution state                                   |
| `nanny-config`  | Parses `nanny.toml`; owns the `NannyConfig` schema                                            |
| `nanny-macros`  | The `#[tool]`, `#[rule]`, `#[agent]` proc-macros                                              |
| `nanny` (CLI)   | `nanny run` / `nanny init`; spawns bridge + child; writes event log                           |

---

## Contributing

The codebase is structured to make contributions straightforward.

**Easy wins:**

- **New built-in tools** — add a file under `crates/runtime/src/tools/`, register it in `default_registry()` in `tools/mod.rs`. Use `http_get.rs` as a template.
- **New stop reasons** — add a variant to `StopReason` in `crates/core/src/agent/state.rs` and update `stop_reason_name()` in the bridge.
- **Config validation** — `crates/config/src/lib.rs` accepts values like `steps = 0` without complaint; adding range checks with clear error messages would be useful.

**Medium complexity:**

- **Python SDK (v0.2.0)** — the bridge is language-agnostic plain HTTP. A Python client implementing `@tool`, `@rule`, `@agent` decorators is the next major milestone.
- **`--dry-run` flag** — simulate enforcement and print what _would_ have been stopped, without killing anything.
- **`nanny status` command** — query a running bridge's `/status` while an agent is executing.

**Core fixes:**

- **`steps` and `cost_spent` in `ExecutionStopped`** — the CLI currently emits `0` for both because it doesn't call `GET /status` before writing the final event. Fix is a single status fetch at the end of `cmd_run` in `crates/cli/src/main.rs`.
- **Managed mode** — `[runtime] mode = "managed"` is parsed and validated but not yet wired into any runtime behavior.

---

## Roadmap

| Version    | Scope                                                                                      |
| ---------- | ------------------------------------------------------------------------------------------ |
| **v0.1.0** | Runtime + Rust Macros: `nanny run` + `#[nanny::tool]`, `#[nanny::rule]`, `#[nanny::agent]` |
| **v0.2.0** | Python SDK: `@tool`, `@rule`, `@agent` — public launch                                     |
| **v0.3.0** | Cloud: managed enforcement, durable audit log, dashboard                                   |

---

## License

Apache-2.0 — see [LICENSE](LICENSE).
