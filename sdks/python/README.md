<p align="center">
  <picture>
    <source media="(prefers-color-scheme: dark)" srcset="https://raw.githubusercontent.com/nanny-run/nanny/main/assets/nanny-logo-dark.svg" />
    <source media="(prefers-color-scheme: light)" srcset="https://raw.githubusercontent.com/nanny-run/nanny/main/assets/nanny-logo-light.svg" />
    <img src="https://raw.githubusercontent.com/nanny-run/nanny/main/assets/nanny-logo-light.svg" alt="Nanny" height="80" />
  </picture>
</p>

# nanny-sdk

Python SDK for [Nanny](https://github.com/nanny-run/nanny) — an execution boundary for autonomous AI agents.

`@tool`, `@rule`, and `@agent` decorators that enforce step limits, cost budgets, tool allowlists, and custom rules per function call. Works with LangChain, CrewAI, or any Python agent framework.

```bash
pip install nanny-sdk
```

Full docs: [docs.nanny.run](https://docs.nanny.run)

---

## How it works

Nanny runs as a parent process via `nanny run`. The SDK decorators communicate with it at each tool call to check limits before the function body executes. Outside `nanny run`, every decorator is a no-op — zero overhead in development and CI.

```bash
# Governed — enforcement active
nanny run

# Passthrough — decorators silent, agent runs normally
python agent.py
uv run agent.py
```

---

## `@tool` — declare a governed tool

```python
from nanny_sdk import tool

@tool(cost=10)
def fetch_page(url: str) -> str:
    import httpx
    return httpx.get(url).text
```

Before `fetch_page` runs, Nanny checks the allowlist, per-tool call limits, and charges 10 cost units against the budget. If any check fails, a `NannyStop` exception is raised and the function body never executes.

Async functions work identically:

```python
@tool(cost=10)
async def fetch_page(url: str) -> str:
    async with httpx.AsyncClient() as client:
        r = await client.get(url)
        return r.text
```

---

## `@rule` — enforce a custom policy

```python
from nanny_sdk import rule

@rule("no_sensitive_files")
def block_sensitive(ctx) -> bool:
    path = ctx.last_tool_args.get("path", "")
    return ".env" not in path and "secret" not in path
```

Rules run before every `@tool` call. Return `False` to stop execution with `RuleDenied`. The `ctx` object exposes `requested_tool`, `last_tool_args`, and counters.

---

## `@agent` — activate named limits for a scope

In a multi-agent system, each agent has a different role and a different risk profile. `@agent` activates the right named limit set when each role runs, then reverts automatically when it's done:

```python
from nanny_sdk import agent

@agent("researcher")
def run_research_loop(query: str) -> str:
    ...
```

Activates `[limits.researcher]` from `nanny.toml` for the duration of the function. Limits revert on exit, including on exception. Each role runs under its own budget and tool allowlist — hitting the analysis ceiling does not affect the reporter, and the analysis agent cannot call the reporter's tools.

![metrics_crew — ingestion, analysis, visualization, and reporter agent scopes entering and exiting](https://raw.githubusercontent.com/nanny-run/nanny/main/assets/demo/metrics-crew-agent-scopes.gif)

---

## `nanny.toml` example

```toml
[runtime]
mode = "local"

[start]
cmd = "uv run agent.py"

[limits]
steps   = 50
cost    = 200
timeout = 120000

[limits.researcher]
steps = 30
cost  = 100

[tools]
allowed = ["fetch_page", "search"]
```

---

## Stop reasons

When a limit is exceeded, a `NannyStop` exception is raised with one of these reasons:

| Reason              | Cause                                                                        |
| ------------------- | ---------------------------------------------------------------------------- |
| `BudgetExhausted`   | Cost ceiling reached                                                         |
| `MaxStepsReached`   | Step limit reached                                                           |
| `TimeoutExpired`    | Wall-clock limit reached                                                     |
| `ToolDenied`        | Tool not in the allowlist                                                    |
| `RuleDenied`        | A rule returned `False`                                                      |
| `AgentCompleted`    | Clean exit                                                                   |
| `AgentNotFound`     | Named limit set in `@agent` does not exist in `nanny.toml`                   |
| `BridgeUnavailable` | Bridge was active but unreachable — fails closed, never continues ungoverned |

---

## Requirements

- Python 3.11+
- `httpx` (only runtime dependency)
- `nanny` CLI:
  - macOS: `brew tap nanny-run/nanny && brew install nannyd`
  - Linux: `curl -fsSL https://install.nanny.run | sh`
  - Windows: `irm https://install.nanny.run/windows | iex`

## Links

- [GitHub](https://github.com/nanny-run/nanny)
- [Documentation](https://docs.nanny.run)
- [Changelog](https://github.com/nanny-run/nanny/blob/main/CHANGELOG.md)
