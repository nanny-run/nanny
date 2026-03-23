# nanny

**Execution boundary for autonomous systems.**

Nanny enforces hard limits on agents and long-running processes. It deterministically stops execution when a limit is reached — no intelligence, no exceptions, no recovery logic inside the engine.

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
{"event":"ExecutionStopped","ts":1711234568432,"reason":"AgentCompleted","steps":7,"cost_spent":7,"elapsed_ms":1432}
```

`ExecutionStarted` is always the first line. `ExecutionStopped` is always the last.

**Stop reasons:**

| Reason | Meaning |
|---|---|
| `AgentCompleted` | Process exited cleanly on its own |
| `TimeoutExpired` | Wall-clock timeout reached — process killed |
| `MaxStepsReached` | Step limit reached |
| `BudgetExhausted` | Cost unit budget spent |
| `ToolDenied` | Tool call not on the allowlist |
| `RuleDenied` | Per-tool rule fired (e.g. max calls exceeded) |
| `ManualStop` | Stopped programmatically |

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

| Code | Meaning |
|---|---|
| `0` | Process exited cleanly (`AgentCompleted`) |
| `1` | Any enforced stop (`TimeoutExpired`, `BudgetExhausted`, etc.) |
| `1` | Config error or spawn failure |

---

## Roadmap

| Version | Scope |
|---|---|
| **v0.1.0** | Process-level enforcement via `nanny run` |
| **v0.2.0** | Rust macros: `#[nanny::tool]`, `#[nanny::rule]`, `#[nanny::agent]` |
| **v0.3.0** | Python SDK: `@tool`, `@rule`, `@agent` — public launch |
| **v0.4.0** | Cloud: managed enforcement, durable audit log, dashboard |

---

## License

Apache-2.0 — see [LICENSE](LICENSE).
