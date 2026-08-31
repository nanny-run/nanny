# Architecture

This document explains how Nanny enforces its guarantees and how to design agents that work correctly with it. Read this before building, the enforcement model has specific properties that affect how you should structure your code.

---

## The enforcement guarantee

When you run `nanny run`, Nanny becomes the **parent process** of your agent. It reads `[start].cmd` from `nanny.toml` and spawns it as a child. The agent runs as its child. All enforcement happens in the parent.

This means:
- The agent cannot catch, delay, or prevent a stop
- A limit breach kills the process, no exceptions, no cleanup hooks
- The enforcement is structural, not advisory

The child process communicates with the parent through an enforcement bridge. Every tool call the agent makes passes through this bridge before anything executes. The bridge decides whether to allow it, charge tokens, and record it. If a limit is crossed, the parent kills the child immediately.

```
┌─────────────────────────────────────────┐
│  nanny (parent)                         │
│                                         │
│  ┌──────────────┐   tool call           │
│  │  your agent  │ ──────────────► bridge│
│  │  (child)     │ ◄──────────────       │
│  └──────────────┘   allowed / stop      │
│                                         │
│  limits enforced: steps · tokens · timeout│
└─────────────────────────────────────────┘
```

---

## Bridge modes

The enforcement bridge runs in two configurations depending on how agents are deployed.

**Local bridge (default, `nanny run`):**

The bridge runs as a thread inside the `nanny` process. It communicates with the agent through a Unix domain socket on macOS and Linux, or a TCP loopback port on Windows. Both are OS-enforced, no process outside the same user session can connect. The bridge starts when `nanny run` spawns the child process and exits when the child exits.

**Governance server (`nanny run --serve`):**

The bridge runs as a long-lived standalone daemon. Agents connect to it over TCP, with mutual TLS enforced on non-loopback addresses. Multiple agents, on multiple machines, can connect to the same server simultaneously. All of their tool calls are governed by the same allowlist and rules, and counted against the same per-tool `max_calls`.

The governance server is the right choice when:
- Agents run in separate processes or containers and need a shared enforcement boundary
- You need cross-machine enforcement (Docker, Kubernetes, remote workers)
- You want enforcement to persist across multiple agent runs on the same task

The local bridge is the right choice for everything else. It has no setup and no cert management.

**The protocol is the same regardless of mode.** The SDK client (Rust or Python) checks for `NANNY_BRIDGE_SOCKET`, then `NANNY_BRIDGE_PORT`, then `NANNY_BRIDGE_ADDR`. Whichever is set, the same HTTP-over-transport protocol runs on top. Changing from local to network enforcement is a configuration change, no code changes needed.

```
Local mode:
  agent ──(Unix socket / loopback TCP)──► nanny process (bridge inside)

Network mode:
  agent A ──(TCP + mTLS)──►
  agent B ──(TCP + mTLS)──► nanny run --serve (bridge as daemon)
  agent C ──(TCP + mTLS)──►
```

---

## The three limits

Every execution is governed by three independent limits. Any one of them stops the run.

| Limit | What it counts | Requires instrumentation |
|-------|----------------|--------------------------|
| `timeout` | Wall-clock time in ms | No, works for any process |
| `steps` | Tool calls made | Yes, SDK |
| `tokens` | Tokens spent | Yes, SDK |

Timeout enforcement is free. Step and token enforcement require your agent to declare its tools using the SDK so the bridge knows when a tool call happens and what to charge.

---

## Core abstractions

### Tool

A **tool** is a function your agent calls to do work. When you declare a function as a tool:

- It is registered on the allowlist
- Each call passes through the bridge for policy enforcement
- Tokens are charged and the step count increments on each successful call
- Any rule denial stops execution before the function body runs

Tools are declared in `nanny.toml` under `[tools] allowed`. The SDK decorator/macro marks the corresponding function in your code. Both are required, the config says what is permitted, the code says when it is used.

### Rule

A **rule** is a function you write that inspects the current execution state and returns a boolean: `true` to continue, `false` to stop.

Rules fire on every tool call, before the call executes. They receive a read-only snapshot of execution state:

- Which tool is being called and with what arguments
- The full history of tool calls made so far
- Counts per tool name
- Labels for every allowed tool, not only the pending one
- Elapsed time, tokens measured, and wall-clock at evaluation

Rules are stateless by design. All state they need comes from the execution snapshot. They cannot modify execution state, they can only allow or deny.

A denial exits the process immediately. The denied tool never runs.

### Agent scope

An **agent scope** names a phase of a run. When a function is declared as an
agent, `AgentScopeEntered` and `AgentScopeExited` bracket every event produced
inside it, so a verdict in the log can be attributed to the phase that caused
it. Scopes nest.

A scope carries no policy of its own. It is attribution, not enforcement.

### Runs

A **run** is Nanny's unit of governance: one rule set, one history, one stop
state, final once stopped. Under local `nanny run` a process is always exactly
one run. Under a governance server one process can hold many, and `run_scope()`
in either SDK opens one.

That matters for correctness, not just accounting. Taint rules read
`tool_call_history`, so a leaked run id means one request's untrusted read
poisons another's history. Run identity is scoped to the thread or task rather
than the process, which is why `run_scope()` is safe under concurrency.

Every event carries its `run_id` and a per-run `seq`. One log file can hold many
interleaved runs, and those two fields are what make it readable.

---

## The direct-call pattern

This is the most important architectural decision you will make.

**Do not rely on an LLM to invoke tools.** Nanny's enforcement is model-agnostic, it does not depend on the model's ability to use tool-calling APIs. An LLM that can't invoke tools doesn't bypass governance; it just produces output that your code ignores. But if your agent architecture depends on the LLM issuing tool calls for governance to work, a weaker model breaks your enforcement entirely.

The correct pattern:

```
LLM:       planning · reasoning · summarizing
Your code: deciding when to call tools · calling them deterministically
```

Concretely: your code drives the tool calls. The LLM tells you *what* to do (which URLs to fetch, which files to read); your code actually does it. Governance fires on every call your code makes, not on calls the LLM invents.

This makes your agent:
- **Model-agnostic**: enforcement works regardless of which model you use
- **Predictable**: the call sequence is determined by your code, not the model
- **Testable**: you can verify governance fires without needing a live model

The alternative, letting the LLM dispatch tool calls directly through a tool-calling API, works when the model reliably uses the API. It breaks silently when it doesn't. Under Nanny, that breakage means the model hallucinates results instead of being stopped, which is the opposite of what governance is for.

---

## Stop reasons

Every execution ends with an `ExecutionStopped` event carrying a `reason` field. The complete set of possible reasons:

| Reason | What it means |
|--------|---------------|
| `AgentCompleted` | Your agent finished normally. The process exited cleanly on its own. |
| `ToolDenied` | A tool call was blocked: the tool is not on the allowlist. Checked before any rule runs. |
| `RuleDenied` | A custom rule returned a denial. The tool never ran. |
| `ManualStop` | Execution was stopped programmatically via the SDK. |
| `ProcessCrashed` | The process exited unexpectedly with a non-zero code. Nanny did not stop it, something in the agent's own code did (panic, unhandled error, OOM, or the process could not be started). |

Note: `ToolFailed` is an **event** emitted when a permitted tool fails at runtime (network error, bad arguments). It is not a stop reason, execution continues and the agent receives an error response. Handle tool errors in your agent code rather than letting them propagate as crashes.

One stop reason requires attention from the developer rather than the operator:

- **`ProcessCrashed`**: this is not a governance event. Inspect your agent's stderr for the actual error. If the process could not be started at all, verify `[start].cmd` in `nanny.toml` is correct and the binary exists.

All other reasons are governance events: Nanny stopped the agent deliberately.

---

## How rules fire

Rules are evaluated on every tool call. The sequence for any tool call is:

1. All registered rules are evaluated against the current execution state
2. If any rule returns `false`, the process exits immediately, the tool never runs, no tokens are charged, no step is counted
3. If all rules pass, the bridge evaluates the allowlist and limits
4. If the bridge allows the call, it executes, tokens are charged, and the step count increments

Rules fire at step 1. Everything else is downstream of that. This is why a rule denial produces `steps: 0` in the event log if it fires on the first tool call, the bridge never recorded a step because the call never reached it.

Rules are evaluated in registration order. Write rules that are fast and pure, they run on every call.

---

## Designing rules

A few properties to keep in mind:

**Rules receive the full call history.** Use this for loop detection, repetition limits, and sequencing constraints. The history is a list of tool names in call order, not deduplicated.

**Rules receive the current call's arguments.** Use this for content-based enforcement: blocking specific file paths, URL patterns, or argument values before the call executes.

**Rules are stateless.** If you need to count calls to a specific tool, use `tool_call_counts` from the execution snapshot, the bridge maintains this for you. Do not use mutable module-level state in rules.

**Rules should be conservative.** A rule that incorrectly denies a legitimate call stops the agent. A rule that incorrectly allows a bad call lets it through. When in doubt, deny.

---

## Multi-agent pipelines

Each stage names itself, and one set of rules covers all of them:

```toml
[tools]
allowed = ["search", "fetch_page", "write_report"]

[tools.fetch_page]
reads_untrusted = true
max_calls       = 20

[tools.write_report]
external_effect = true

[rules]
extends = ["nanny:recommended@1.0.0"]
```

The rules do not name the stages, and they do not need to. `no_send_after_read`
denies `write_report` after `fetch_page` regardless of which agent scope is
active, because the hazard is the ordering of authority rather than the identity
of the caller.

Scopes give you attribution in the log; labels and rules give you enforcement.
Keep those two jobs separate when designing a pipeline.

---

## Testing your integration

Before shipping, verify that each of your governance constraints actually fires. The recommended approach is to construct minimal inputs that exercise each constraint:

- **Allowlist**: call a tool that is not in `[tools] allowed`. It should produce `ToolDenied`.
- **Rules**: construct input that your rule is designed to block. It should produce `RuleDenied`.
- **Per-tool cap**: call one tool past its `max_calls`. It should produce `RuleDenied` with `rule_name = "<tool>.max_calls"`.
- **Declared authority**: read `ExecutionStarted` and confirm `allowed_tools`, `tool_labels`, and `config_hash` match the config you shipped.
- **Rule packs**: remove an installed pack from disk while leaving it in `[rules] extends`. The run should refuse to start.

Use the `ExecutionStopped` event in the NDJSON log to verify the reason. Do not rely on stderr output alone, the event log is the authoritative record.

Keep these test inputs alongside your agent code. They are as important as unit tests, they verify that your governance constraints work as designed, not just that your agent logic works.

---

## What Nanny does not do

To avoid building on false assumptions:

- **Nanny does not isolate the agent.** The child process has the same filesystem and network access as any other process the user can run. Nanny stops it when limits are crossed, but it does not restrict what the agent can do before then.
- **Nanny does not validate tool outputs.** What a tool returns to the agent is the agent's concern. Nanny enforces whether the call is permitted, not whether the result is correct.
- **Nanny does not prevent all loops.** A loop that does not call tools (pure CPU computation, sleeping) is not visible to Nanny. The timeout is the backstop for those cases.
- **Nanny does not recover from crashes.** If your agent panics or crashes, Nanny kills it and emits `ProcessCrashed`. It does not restart the agent or retry.
