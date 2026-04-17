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

```python
from nanny_sdk import agent

@agent("researcher")
def run_research_loop(query: str) -> str:
    ...
```

Activates `[limits.researcher]` from `nanny.toml` for the duration of the function. Limits revert on exit, including on exception.

---

## `nanny.toml` example

```toml
[limits]
max_steps       = 50
max_cost_units  = 200
timeout_secs    = 120

[limits.researcher]
max_steps       = 30
max_cost_units  = 100

[tools]
allowed = ["fetch_page", "search"]

[start]
cmd = "uv run agent.py"
```

---

## Stop reasons

When a limit is exceeded, a `NannyStop` exception is raised with one of these reasons:

| Reason | Cause |
|--------|-------|
| `BudgetExhausted` | Cost ceiling reached |
| `MaxStepsReached` | Step limit reached |
| `TimeoutExpired` | Wall-clock limit reached |
| `ToolDenied` | Tool not in the allowlist |
| `RuleDenied` | A rule returned `False` |
| `AgentCompleted` | Clean exit |

---

## Requirements

- Python 3.11+
- `httpx` (only runtime dependency)
- `nanny` CLI: `brew tap nanny-run/nanny && brew install nannyd`

## Links

- [GitHub](https://github.com/nanny-run/nanny)
- [Documentation](https://docs.nanny.run)
- [Changelog](https://github.com/nanny-run/nanny/blob/main/CHANGELOG.md)
