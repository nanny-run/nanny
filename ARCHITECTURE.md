# Architecture

This document explains how Nanny enforces its guarantees and how to design agents that work correctly with it. Read this before building — the enforcement model has specific properties that affect how you should structure your code.

---

## The enforcement guarantee

When you run `nanny run <cmd>`, Nanny becomes the **parent process** of your agent. The agent runs as its child. All enforcement happens in the parent.

This means:
- The agent cannot catch, delay, or prevent a stop
- A limit breach kills the process — no exceptions, no cleanup hooks
- The enforcement is structural, not advisory

The child process communicates with the parent through a local bridge. Every tool call the agent makes passes through this bridge before anything executes. The bridge decides whether to allow it, charge cost, and record it. If a limit is crossed, the parent kills the child immediately.

```
┌─────────────────────────────────────────┐
│  nanny (parent)                         │
│                                         │
│  ┌──────────────┐   tool call           │
│  │  your agent  │ ──────────────► bridge│
│  │  (child)     │ ◄──────────────       │
│  └──────────────┘   allowed / stop      │
│                                         │
│  limits enforced: steps · cost · timeout│
└─────────────────────────────────────────┘
```

---

## The three limits

Every execution is governed by three independent limits. Any one of them stops the run.

| Limit | What it counts | Requires instrumentation |
|-------|----------------|--------------------------|
| `timeout` | Wall-clock time in ms | No — works for any process |
| `steps` | Tool calls made | Yes — SDK |
| `cost` | Cost units spent | Yes — SDK |

Timeout enforcement is free. Step and cost enforcement require your agent to declare its tools using the SDK so the bridge knows when a tool call happens and what to charge.

---

## Core abstractions

### Tool

A **tool** is a function your agent calls to do work. When you declare a function as a tool:

- It is registered on the allowlist
- Each call passes through the bridge for policy enforcement
- Cost is charged and the step count increments on each successful call
- Any rule denial stops execution before the function body runs

Tools are declared in `nanny.toml` under `[tools] allowed`. The SDK decorator/macro marks the corresponding function in your code. Both are required — the config says what is permitted, the code says when it is used.

### Rule

A **rule** is a function you write that inspects the current execution state and returns a boolean: `true` to continue, `false` to stop.

Rules fire on every tool call, before the call executes. They receive a read-only snapshot of execution state:

- Which tool is being called and with what arguments
- The full history of tool calls made so far
- Counts per tool name
- Elapsed time and cost spent

Rules are stateless by design. All state they need comes from the execution snapshot. They cannot modify execution state — they can only allow or deny.

A denial exits the process immediately. The denied tool never runs.

### Agent scope

An **agent scope** is a named limits context. When a function is declared as an agent, the scope's limits become active for the duration of that function and revert when it returns.

Scopes inherit from the base `[limits]` and override only the fields they declare. A tight inner scope cannot exceed the outer scope's budget — the lowest limit always wins.

Scopes are designed for multi-agent pipelines where each stage has different resource requirements: a planner that makes no tool calls gets a tight budget; a researcher that fetches many URLs gets a larger one.

---

## The direct-call pattern

This is the most important architectural decision you will make.

**Do not rely on an LLM to invoke tools.** Nanny's enforcement is model-agnostic — it does not depend on the model's ability to use tool-calling APIs. An LLM that can't invoke tools doesn't bypass governance; it just produces output that your code ignores. But if your agent architecture depends on the LLM issuing tool calls for governance to work, a weaker model breaks your enforcement entirely.

The correct pattern:

```
LLM:       planning · reasoning · summarizing
Your code: deciding when to call tools · calling them deterministically
```

Concretely: your code drives the tool calls. The LLM tells you *what* to do (which URLs to fetch, which files to read); your code actually does it. Governance fires on every call your code makes — not on calls the LLM invents.

This makes your agent:
- **Model-agnostic** — enforcement works regardless of which model you use
- **Predictable** — the call sequence is determined by your code, not the model
- **Testable** — you can verify governance fires without needing a live model

The alternative — letting the LLM dispatch tool calls directly through a tool-calling API — works when the model reliably uses the API. It breaks silently when it doesn't. Under Nanny, that breakage means the model hallucinates results instead of being stopped, which is the opposite of what governance is for.

---

## Stop reasons

Every execution ends with an `ExecutionStopped` event carrying a `reason` field. The complete set of possible reasons:

| Reason | What it means |
|--------|---------------|
| `AgentCompleted` | Your agent finished normally. The process exited cleanly on its own. |
| `TimeoutExpired` | The wall-clock timeout was reached. The process was killed. |
| `MaxStepsReached` | The step limit was hit. The process was killed. |
| `BudgetExhausted` | The cost budget was exhausted. The process was killed. |
| `ToolDenied` | A tool call was blocked — the tool is not on the allowlist. |
| `RuleDenied` | A custom rule returned a denial. The tool never ran. |
| `ToolFailed` | A tool was allowed and called, but failed at runtime (e.g. network error). |
| `ProcessCrashed` | The process exited unexpectedly with a non-zero code. Nanny did not stop it — something in the agent's own code did (panic, unhandled error, OOM). |
| `SpawnFailed` | The child process could not be started at all. Check the command in `nanny.toml`. |

Three of these require attention from the developer rather than the operator:

- **`ProcessCrashed`** — this is a bug in your agent, not a governance event. Inspect your agent's stderr for the actual error.
- **`ToolFailed`** — the tool was permitted but the underlying operation failed. Handle errors gracefully in your agent so a single failed call doesn't crash the entire run.
- **`SpawnFailed`** — your `[start] cmd` in `nanny.toml` is wrong, or the binary doesn't exist.

All other reasons are governance events — Nanny stopped the agent deliberately.

---

## How rules fire

Rules are evaluated on every tool call. The sequence for any tool call is:

1. All registered rules are evaluated against the current execution state
2. If any rule returns `false`, the process exits immediately — the tool never runs, no cost is charged, no step is counted
3. If all rules pass, the bridge evaluates the allowlist and limits
4. If the bridge allows the call, it executes, cost is charged, and the step count increments

Rules fire at step 1. Everything else is downstream of that. This is why a rule denial produces `steps: 0` in the event log if it fires on the first tool call — the bridge never recorded a step because the call never reached it.

Rules are evaluated in registration order. Write rules that are fast and pure — they run on every call.

---

## Designing rules

A few properties to keep in mind:

**Rules receive the full call history.** Use this for loop detection, repetition limits, and sequencing constraints. The history is a list of tool names in call order — not deduplicated.

**Rules receive the current call's arguments.** Use this for content-based enforcement: blocking specific file paths, URL patterns, or argument values before the call executes.

**Rules are stateless.** If you need to count calls to a specific tool, use `tool_call_counts` from the execution snapshot — the bridge maintains this for you. Do not use mutable module-level state in rules.

**Rules should be conservative.** A rule that incorrectly denies a legitimate call stops the agent. A rule that incorrectly allows a bad call lets it through. When in doubt, deny.

---

## Multi-agent pipelines

When building a pipeline of agents, each stage should have its own scope with limits appropriate to its role:

```toml
[limits]
steps   = 50
cost    = 1000
timeout = 60000

[limits.planner]
steps   = 5
cost    = 100
timeout = 15000

[limits.researcher]
steps   = 20
cost    = 600
timeout = 60000

[limits.synthesizer]
steps   = 5
cost    = 200
timeout = 30000
```

The base `[limits]` is the outer budget for the entire run. Named scopes are inner budgets for each stage. A stage cannot exceed the outer budget — if the global cost limit is reached mid-pipeline, the run stops regardless of the active scope.

Design your limits so the sum of all stage budgets is comfortably within the global budget, with headroom for overhead between stages.

---

## Testing your integration

Before shipping, verify that each of your governance constraints actually fires. The recommended approach is to construct minimal inputs that exercise each constraint:

- **Allowlist** — call a tool that is not in `[tools] allowed`. It should produce `ToolDenied`.
- **Rules** — construct input that your rule is designed to block. It should produce `RuleDenied`.
- **Cost limit** — set a very low `cost` limit and make enough tool calls to exceed it. It should produce `BudgetExhausted`.
- **Step limit** — set a very low `steps` limit and make enough tool calls to exceed it. It should produce `MaxStepsReached`.
- **Timeout** — set a very short `timeout` and run a slow operation. It should produce `TimeoutExpired`.

Use the `ExecutionStopped` event in the NDJSON log to verify the reason. Do not rely on stderr output alone — the event log is the authoritative record.

Keep these test inputs alongside your agent code. They are as important as unit tests — they verify that your governance constraints work as designed, not just that your agent logic works.

---

## What Nanny does not do

To avoid building on false assumptions:

- **Nanny does not sandbox the agent.** The child process has the same filesystem and network access as any other process the user can run. Nanny stops it when limits are crossed, but it does not restrict what the agent can do before then.
- **Nanny does not validate tool outputs.** What a tool returns to the agent is the agent's concern. Nanny enforces whether the call is permitted, not whether the result is correct.
- **Nanny does not prevent all loops.** A loop that does not call tools (pure CPU computation, sleeping) is not visible to Nanny. The timeout is the backstop for those cases.
- **Nanny does not recover from crashes.** If your agent panics or crashes, Nanny kills it and emits `ProcessCrashed`. It does not restart the agent or retry.
