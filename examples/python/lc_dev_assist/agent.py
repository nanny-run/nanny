"""Debug agent for dev_assist.

Given a stack trace, the agent:
  1. Reads the files mentioned in the trace.
  2. Searches for related symbols with ripgrep.
  3. Returns a root cause + suggested fix.

Model: llama3.1:8b via Ollama (local, no API key needed).
Tools: file_reader, ripgrep — both defined in tools.py.

The agent is rate-limited via nanny.toml so it can't read files or run searches
indefinitely.  The no_read_loop policy stops it if it gets stuck reading the
same files in a row.

CLI entry point: cli.py.  This module exports run_debug() only.
"""

from __future__ import annotations

from collections import deque
from typing import Any

from langchain.agents import create_agent
from langchain_core.messages import HumanMessage
from langchain_ollama import ChatOllama
from nanny_sdk import agent as nanny_agent
from nanny_sdk import rule

from tools import file_reader, ripgrep

# ---------------------------------------------------------------------------
# Config
# ---------------------------------------------------------------------------

MODEL = "llama3.1:8b"
OLLAMA_BASE_URL = "http://localhost:11434"

# ---------------------------------------------------------------------------
# Policy: no_read_loop
#
# If the agent reads the same files over and over without making progress,
# stop it.  Tracked locally — the last 5 tool calls must not all be
# file_reader.
# ---------------------------------------------------------------------------

_call_window: deque[str] = deque(maxlen=5)


@rule("no_read_loop")
def check_no_read_loop(ctx: Any) -> bool:
    tool_name = getattr(ctx, "requested_tool", "") or ""
    _call_window.append(tool_name)
    if len(_call_window) == 5 and all(t == "file_reader" for t in _call_window):
        return False
    return True


# ---------------------------------------------------------------------------
# System prompt
# ---------------------------------------------------------------------------

_SYSTEM = """\
You are a debug assistant. Given a stack trace or error output, your job is to
diagnose the root cause and propose a concrete fix.

Follow these steps every time:
1. Identify every file path mentioned in the trace.
2. Read those files using the file_reader tool to understand the context.
3. Search for related symbols or function names using the ripgrep tool.
4. Once you understand the code, respond with:
   - Root cause: one sentence explaining what went wrong.
   - Files involved: the relevant file paths.
   - Fix: a code diff or clear description of what to change.

Read the relevant files before drawing any conclusions.
Stop as soon as you have a clear diagnosis — do not keep reading indefinitely.\
"""


# ---------------------------------------------------------------------------
# Public API
# ---------------------------------------------------------------------------


@nanny_agent("debugger")
def run_debug(trace_text: str) -> str:
    """Analyse a stack trace and return a diagnosis.

    Raises a NannyStop subclass (ToolDenied, RuleDenied, BudgetExhausted,
    MaxStepsReached, TimeoutExpired) if the agent is stopped early.
    The caller (cli.py) handles those and presents a user-facing message.
    """
    llm = ChatOllama(model=MODEL, base_url=OLLAMA_BASE_URL, temperature=0)
    tools = [file_reader, ripgrep]

    agent = create_agent(llm, tools, system_prompt=_SYSTEM)
    result = agent.invoke({"messages": [HumanMessage(content=trace_text)]})

    messages = result.get("messages", [])
    if not messages:
        return "[no response from agent]"

    last = messages[-1]
    return getattr(last, "content", str(last))
