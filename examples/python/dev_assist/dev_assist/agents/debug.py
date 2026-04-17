"""Debug agent for dev_assist.

Given a stack trace, the agent diagnoses the root cause and suggests a fix.
Two execution modes are available:

  react  (default) — self-controlled ReAct loop.
    Model outputs Thought/Action/Observation cycles in text.
    Our code parses each step and calls tools directly, so @nanny_tool
    intercepts every tool call regardless of model API capabilities.

  plan   — Plan-and-Execute.
    Phase 1: single LLM call produces a structured JSON plan (files + searches).
    Phase 2: deterministic code executes each step in the plan.
    Phase 3: single LLM call synthesises a diagnosis from the gathered context.
    The model cannot add steps during execution — enforcement surface is fixed.

Both modes use the same @nanny_tool-decorated tools and the same @agent scope.
Nanny governs every tool call in both modes; only the execution structure differs.

Model: llama3.1:8b via Ollama (local, no API key needed).
"""

from __future__ import annotations

import json
import re
from typing import Any

from pydantic import ValidationError

from langchain_core.messages import AIMessage, HumanMessage
from langchain_ollama import ChatOllama
from nanny_sdk import agent as nanny_agent
from nanny_sdk import rule

from dev_assist.config import MAX_STEPS, MODEL, OLLAMA_BASE_URL
from dev_assist.tools import file_reader, ripgrep, write_file

# ---------------------------------------------------------------------------
# Policy: no_read_loop
#
# Fires the moment any file is read a second time.
# A healthy agent reads each file once — the content stays in the conversation
# history and can be referenced without re-reading. Re-reading the same file
# means the agent forgot it or is circling without making progress.
# ---------------------------------------------------------------------------

_seen_files: set[str] = set()


@rule("no_read_loop")
def check_no_read_loop(ctx: Any) -> bool:
    if getattr(ctx, "requested_tool", "") == "file_reader":
        path = (ctx.last_tool_args or {}).get("path", "")
        if path in _seen_files:
            return False
        if path:
            _seen_files.add(path)
    return True


# ---------------------------------------------------------------------------
# Shared helpers
# ---------------------------------------------------------------------------

_PY_PATH_RE = re.compile(r'File "([^"]+\.(?:py|ts|js|go|rs|java))"')


def _extract_file_paths(trace: str) -> list[str]:
    """Pull unique source file paths out of a stack trace."""
    return list(dict.fromkeys(_PY_PATH_RE.findall(trace)))


# ---------------------------------------------------------------------------
# Mode 1: ReAct loop
# ---------------------------------------------------------------------------

_REACT_SYSTEM = """\
You are a debug assistant. Diagnose bugs by reading the actual source files.

Tools:
{tool_list}

For every step, respond in exactly this format:
Thought: <what you need to do next>
Action: <tool name — one of: {tool_names}>
Action Input: <JSON object matching the tool's arguments>

When you have enough information to give a diagnosis, respond with:
Thought: I now have enough information
Final Answer:
Root cause: <one sentence>
Files involved: <paths>
Fix: <diff or clear description>\
"""


_ACTION_RE = re.compile(r"Action:\s*(\w+)", re.IGNORECASE)
_INPUT_RE = re.compile(
    r"Action Input:\s*(\{.*?\}|\S+)", re.DOTALL | re.IGNORECASE)


def _system_prompt(tools: list[Any]) -> str:
    tool_list = "\n".join(
        f"  {t.name} — {t.description.strip()}  args: {json.dumps(t.args)}"
        for t in tools
    )
    tool_names = ", ".join(t.name for t in tools)
    return _REACT_SYSTEM.format(tool_list=tool_list, tool_names=tool_names)


def _parse_step(text: str) -> tuple[str | None, dict[str, Any]]:
    """Extract (action_name, kwargs) from a ReAct step."""
    action_m = _ACTION_RE.search(text)
    if not action_m:
        return None, {}
    action = action_m.group(1).strip()

    input_m = _INPUT_RE.search(text)
    args: dict[str, Any] = {}
    if input_m:
        raw = input_m.group(1).strip()
        try:
            parsed = json.loads(raw)
            if isinstance(parsed, dict):
                args = parsed
        except json.JSONDecodeError:
            if action == "file_reader":
                args = {"path": raw}
            elif action == "ripgrep":
                args = {"pattern": raw}

    return action, args


def _react_loop(llm: ChatOllama, tools: list[Any], trace_text: str) -> str:
    """Self-controlled ReAct loop.

    We parse the model's text output and call tool.run() directly — @nanny_tool
    always intercepts regardless of whether the model supports structured tool-calling.
    NannyStop exceptions propagate here and bubble up to cli/main.py.
    """
    tools_by_name = {t.name: t for t in tools}

    paths = _extract_file_paths(trace_text)
    if paths:
        path_lines = "\n".join(f"  - {p}" for p in paths)
        user_msg = (
            f"These files appear in the stack trace:\n{path_lines}\n\n"
            f"Read them and diagnose the bug.\n\nStack trace:\n{trace_text}"
        )
    else:
        user_msg = f"Diagnose this error:\n\n{trace_text}"

    from langchain_core.messages import SystemMessage

    messages: list[Any] = [
        SystemMessage(content=_system_prompt(tools)),
        HumanMessage(content=user_msg),
    ]

    for _ in range(MAX_STEPS):
        response = llm.invoke(messages)
        content: str = response.content  # type: ignore[assignment]

        if "Final Answer:" in content:
            return content.split("Final Answer:", 1)[1].strip()

        action, args = _parse_step(content)
        if action is None:
            return content

        tool = tools_by_name.get(action)
        if tool is None:
            observation = (
                f"[error] Unknown tool '{action}'. "
                f"Available tools: {list(tools_by_name)}"
            )
        else:
            # Sanitize: some models pass a list where a scalar is expected.
            # Take the first element so LangChain's schema validation passes.
            clean_args = {
                k: (v[0] if isinstance(v, list) and v else v)
                for k, v in args.items()
            }
            try:
                observation = str(tool.run(clean_args))
            except ValidationError as exc:
                observation = f"[error] Invalid arguments for '{action}': {exc}"

        messages.append(AIMessage(content=content))
        messages.append(HumanMessage(content=f"Observation: {observation}"))

    return "[stopped — max steps reached without a final answer]"


# ---------------------------------------------------------------------------
# Mode 2: Plan-and-Execute
# ---------------------------------------------------------------------------

_PLAN_PROMPT = """\
You are a debug assistant. Given the stack trace below, produce a JSON plan
listing which files to read and which symbols to search for.

Output ONLY valid JSON — no explanation, no markdown fences:
{{
  "files": ["path/to/file.py"],
  "searches": [{{"pattern": "SymbolName", "path": "."}}]
}}

Rules:
- files: paths that appear in the stack trace (up to 5)
- searches: symbols, function names, or error messages to locate (up to 3)
- If a path is not in the trace, do not invent one

Stack trace:
{trace}"""

_ANALYZE_PROMPT = """\
You are a debug assistant. Diagnose the error below using the file contents
and search results provided.

Stack trace:
{trace}

Gathered context:
{context}

Respond with:
Root cause: <one sentence>
Files involved: <paths>
Fix: <diff or clear description>"""


def _parse_plan(text: str) -> dict[str, Any]:
    """Extract JSON plan from model output, tolerating markdown fences."""
    text = re.sub(r"```(?:json)?\s*", "", text).strip().rstrip("`").strip()
    m = re.search(r"\{.*\}", text, re.DOTALL)
    if m:
        try:
            return json.loads(m.group())
        except json.JSONDecodeError:
            pass
    return {"files": [], "searches": []}


def _plan_loop(llm: ChatOllama, tools: list[Any], trace_text: str) -> str:
    """Plan-and-Execute mode.

    Phase 1 — Plan: one LLM call → structured JSON (files + searches).
    Phase 2 — Execute: deterministic code calls each tool in order.
               @nanny_tool intercepts every call — enforcement is identical
               to the ReAct loop; only the execution structure differs.
    Phase 3 — Analyze: one LLM call with all gathered context → diagnosis.
    """
    tools_by_name = {t.name: t for t in tools}

    # Phase 1: plan
    plan_response = llm.invoke(
        [HumanMessage(content=_PLAN_PROMPT.format(trace=trace_text))])
    plan = _parse_plan(plan_response.content)  # type: ignore[arg-type]

    # Phase 2: execute — deterministic, our code drives every tool call
    gathered: list[str] = []

    for path in plan.get("files", [])[:5]:
        tool = tools_by_name.get("file_reader")
        if tool:
            result = str(tool.run({"path": path}))
            gathered.append(f"### {path}\n{result}")

    for search in plan.get("searches", [])[:3]:
        tool = tools_by_name.get("ripgrep")
        if tool and isinstance(search, dict):
            result = str(tool.run({
                "pattern": search.get("pattern", ""),
                "path": search.get("path", "."),
            }))
            gathered.append(f"### rg {search.get('pattern', '')!r}\n{result}")

    if not gathered:
        return (
            "Plan produced no files or searches to inspect. "
            "Try --mode react for a more exploratory analysis."
        )

    # Phase 3: analyze
    context = "\n\n---\n\n".join(gathered)
    analyze_response = llm.invoke([
        HumanMessage(content=_ANALYZE_PROMPT.format(
            trace=trace_text, context=context))
    ])

    # Phase 4: apply fix — deterministic write_file call.
    # Under `nanny run`: ToolDenied fires if write_file is not in [tools] allowed.
    # In passthrough (uv run dev ...): actually writes the fix to disk.
    # Using plan["files"][0] as the target — the primary file in the trace.
    if plan.get("files"):
        fix_target = plan["files"][0]
        write_tool = tools_by_name.get("write_file")
        if write_tool:
            write_tool.run({"path": fix_target, "content": str(analyze_response.content)})

    return analyze_response.content  # type: ignore[return-value]


# ---------------------------------------------------------------------------
# Public API
# ---------------------------------------------------------------------------


@nanny_agent("debugger")
def run_debug(trace_text: str, mode: str = "react") -> str:
    """Analyse a stack trace and return a diagnosis.

    Args:
        trace_text: The full stack trace or error log to diagnose.
        mode: Execution mode — "react" (default) or "plan".

    Raises a NannyStop subclass if the agent is stopped early.
    The caller (cli/main.py) handles those and presents a user-facing message.
    """
    llm = ChatOllama(model=MODEL, base_url=OLLAMA_BASE_URL, temperature=0)
    tools = [file_reader, ripgrep, write_file]
    if mode == "plan":
        return _plan_loop(llm, tools, trace_text)
    return _react_loop(llm, tools, trace_text)
