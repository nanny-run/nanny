<p align="center">
  <picture>
    <source media="(prefers-color-scheme: dark)" srcset="https://raw.githubusercontent.com/nanny-run/nanny/main/assets/nanny-logo-dark.svg" />
    <source media="(prefers-color-scheme: light)" srcset="https://raw.githubusercontent.com/nanny-run/nanny/main/assets/nanny-logo-light.svg" />
    <img src="https://raw.githubusercontent.com/nanny-run/nanny/main/assets/nanny-logo-light.svg" alt="Nanny" height="80" />
  </picture>
</p>

# nanny-sdk

Python SDK for [Nanny](https://github.com/nanny-run/nanny): the enforcement primitive for autonomous AI agents.

`@tool`, `@rule`, and `@agent` decorators that enforce tool allowlists, per-tool call caps, and custom rules per function call. Works with LangChain, CrewAI, or any Python agent framework.

```bash
pip install nanny-sdk
```

Full docs: [docs.nanny.run](https://docs.nanny.run)

---

## How it works

Nanny runs as a parent process via `nanny run`. The SDK decorators communicate with it at each tool call, so the call is authorized before the function body executes. Outside `nanny run`, every decorator is a no-op, zero overhead in development and CI.

```bash
# Governed, enforcement active
nanny run

# Passthrough, decorators silent, agent runs normally
python agent.py
uv run agent.py
```

---

## `@tool`: declare a governed tool

```python
from nanny_sdk import tool

@tool(tokens=10)
def fetch_page(url: str) -> str:
    import httpx
    return httpx.get(url).text
```

Before `fetch_page` runs, Nanny checks the allowlist, the per-tool call cap, and every registered rule, and records 10 tokens against the run. If any check refuses, a `NannyStop` exception is raised and the function body never executes.

Async functions work identically:

```python
@tool(tokens=10)
async def fetch_page(url: str) -> str:
    async with httpx.AsyncClient() as client:
        r = await client.get(url)
        return r.text
```

---

## `instrument`: automatic LLM token tracking

```python
import nanny_sdk, openai

client = openai.OpenAI()
nanny_sdk.instrument(client)   # one line, done
```

Call once at startup. Every LLM completion response is intercepted and its token counts are recorded automatically, no `@tool` decorator needed on the LLM call itself. Tokens are measured for attribution, never enforced: no token count stops a run.

Supported: OpenAI, Groq, Together AI, Azure OpenAI, LiteLLM, Anthropic, Mistral, Google Gemini (google-genai), Cohere v2. No-op in passthrough mode.

For providers that report prompt-caching usage (OpenAI, Anthropic, DeepSeek, Gemini), `instrument` also captures `cache_read`/`cache_write`, a finer, reporting-only split of `input`, never additional tokens and never used for enforcement.

---

## `@rule`: enforce a custom policy

```python
from nanny_sdk import rule

@rule("no_sensitive_files")
def block_sensitive(ctx) -> bool:
    path = ctx.last_tool_args.get("path", "")
    return ".env" not in path and "secret" not in path
```

Rules run before every `@tool` call. Return `False` to stop execution with `RuleDenied`. The `ctx` object exposes `requested_tool`, `last_tool_args`, `tool_labels`, `tool_call_history`, and `now_ms`.

---

## `@agent`: name a phase of the run

In a multi-agent system a denial is worth attributing to the phase that caused it:

```python
from nanny_sdk import agent

@agent("researcher")
def run_research_loop(query: str) -> str:
    ...
```

Names a phase of the run for the duration of the function, so the audit log can attribute each verdict to the phase that produced it. The scope exits on return and on exception. Any name works: a scope labels, it does not look anything up. Want a phase to be a genuinely separate run, with its own stop state and history? See `run_scope` below.

---

## `run_scope`: an independent run, safely, even concurrently

`@agent` labels a phase within one run. If a phase should be a genuinely separate run, with its own stop state and its own tool call history, that is a new **run**. A threaded or async server handling several independent sessions at once needs one per session:

```python
import nanny_sdk

with nanny_sdk.run_scope() as run_id:
    ...  # every governed call in this thread or task uses this run_id
```

Each call gets its own run id, isolated per thread and per asyncio task, so two runs in flight at once in the same process never clobber each other's stop state or tool call history. Pass an explicit `run_id` to resume a specific run instead of minting a fresh one. Every caller that never calls this is unaffected, resolution falls through to `NANNY_RUN_ID` exactly as before.

---

## `nanny.toml` example

```toml
[start]
cmd = "uv run agent.py"

[tools]
allowed = ["fetch_page", "search"]

[tools.fetch_page]
max_calls       = 30
reads_untrusted = true

[tools.search]
reads_untrusted = true
```

The five labels (`reads_untrusted`, `external_effect`, `destructive`, `moves_money`, `reads_sensitive`) describe what a tool *is*. Rules read labels rather than tool names, which is what lets a rule written for one app govern another.

Cloud sync isn't a config field. Set `NANNY_API_KEY` and every `nanny run` on that machine forwards its event log automatically. No key, no sync.

---

## Stop reasons

When an action is refused, a `NannyStop` exception is raised with one of these reasons:

| Reason              | Cause                                                                        |
| ------------------- | ---------------------------------------------------------------------------- |
| `ToolDenied`        | Tool not in the allowlist                                                    |
| `RuleDenied`        | A rule returned `False`                                                      |
| `AgentCompleted`    | Clean exit                                                                   |
| `ExecutionStopped`  | This run was already stopped by an earlier call                              |
| `BridgeUnavailable` | Enforcement was active but became unreachable, fails closed, never continues ungoverned |

Both refusals are policy decisions. Nanny bounds what an agent may do, not how much it may consume.

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
