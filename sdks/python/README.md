<p align="center">
  <picture>
    <source media="(prefers-color-scheme: dark)" srcset="https://raw.githubusercontent.com/nanny-run/nanny/main/assets/nanny-logo-dark.svg" />
    <source media="(prefers-color-scheme: light)" srcset="https://raw.githubusercontent.com/nanny-run/nanny/main/assets/nanny-logo-light.svg" />
    <img src="https://raw.githubusercontent.com/nanny-run/nanny/main/assets/nanny-logo-light.svg" alt="Nanny" height="80" />
  </picture>
</p>

# nanny-sdk

Python SDK for [Nanny](https://github.com/nanny-run/nanny) — the enforcement primitive for autonomous AI agents.

`@tool`, `@rule`, and `@agent` decorators that enforce step limits, token budgets, tool allowlists, and custom rules per function call. Works with LangChain, CrewAI, or any Python agent framework.

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

@tool(tokens=10)
def fetch_page(url: str) -> str:
    import httpx
    return httpx.get(url).text
```

Before `fetch_page` runs, Nanny checks the allowlist, per-tool call limits, and charges 10 tokens against the budget. If any check fails, a `NannyStop` exception is raised and the function body never executes.

Async functions work identically:

```python
@tool(tokens=10)
async def fetch_page(url: str) -> str:
    async with httpx.AsyncClient() as client:
        r = await client.get(url)
        return r.text
```

---

## `instrument` — automatic LLM token tracking

```python
import nanny_sdk, openai

client = openai.OpenAI()
nanny_sdk.instrument(client)   # one line — done
```

Call once at startup. Every LLM completion response is intercepted and its token counts are reported to Nanny's budget automatically — no `@tool` decorator needed on the LLM call itself.

Supported: OpenAI, Groq, Together AI, Azure OpenAI, LiteLLM, Anthropic, Mistral, Google Gemini (google-genai), Cohere v2. No-op in passthrough mode.

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

Activates `[limits.researcher]` from `nanny.toml` for the duration of the function. Limits revert on exit, including on exception. Each role gets its own tool allowlist — the analysis agent cannot call the reporter's tools. Budgets are a different story: tokens spent and steps taken are one running total for the *whole run*, not a separate pool per role, so hitting the analysis ceiling stops the run there, the reporter never gets to run at all. Want each role to genuinely start from a clean budget instead? See `fresh_run` below.

![metrics_crew — ingestion, analysis, visualization, and reporter agent scopes entering and exiting](https://raw.githubusercontent.com/nanny-run/nanny/main/assets/demo/metrics-crew-agent-scopes.gif)

---

## `fresh_run` — starting a genuinely independent budget

`@agent` changes which ceiling the run's *one* running total is checked against; it does not give a role its own budget (see above). If your process runs multiple independent phases back to back and want each one to start from zero instead, that's a new **run**:

```python
import nanny_sdk

nanny_sdk.fresh_run()   # everything governed after this point is a fresh run
```

Only meaningful under a network server (`nanny run --serve` / `--join`), which tracks each run's budget independently. Under local `nanny run`, one process is already always exactly one run, so it's a safe no-op there.

---

## `nanny.toml` example

```toml
[start]
cmd = "uv run agent.py"

[limits]
steps   = 50
tokens  = 200
timeout = 120000

[limits.researcher]
steps  = 30
tokens = 100

[tools]
allowed = ["fetch_page", "search"]
```

Cloud sync isn't a config field. Run `nanny auth login` once on a machine and every `nanny run` there forwards its event log automatically. No login, no sync.

---

## Stop reasons

When a limit is exceeded, a `NannyStop` exception is raised with one of these reasons:

| Reason              | Cause                                                                        |
| ------------------- | ---------------------------------------------------------------------------- |
| `BudgetExhausted`   | Token ceiling reached                                                        |
| `MaxStepsReached`   | Step limit reached                                                           |
| `TimeoutExpired`    | Wall-clock limit reached                                                     |
| `ToolDenied`        | Tool not in the allowlist                                                    |
| `RuleDenied`        | A rule returned `False`                                                      |
| `AgentCompleted`    | Clean exit                                                                   |
| `AgentNotFound`     | Named limit set in `@agent` does not exist in `nanny.toml`                   |
| `BridgeUnavailable` | Enforcement was active but became unreachable — fails closed, never continues ungoverned |

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
