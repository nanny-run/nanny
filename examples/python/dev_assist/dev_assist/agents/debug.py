"""Debug agent for dev_assist — LangGraph StateGraph.

Architecture: explicit Python-driven nodes.
    Python calls every tool (file_reader, ripgrep) directly in graph nodes.
    @nanny_tool fires on every call — enforcement is guaranteed regardless
    of model. The LLM is used only in diagnose_node for pure synthesis.

Execution order:
    extract_node      — Python extracts file paths and search patterns from the trace
    read_files_node   — Python calls file_reader for each extracted path
    search_node       — Python calls ripgrep for each extracted pattern
    diagnose_node     — LLM synthesises a diagnosis from all gathered context

Governance:
    @rule("no_read_loop")    — fires when the same file is read more than once.
                               Duplicate frames in the trace trigger this naturally,
                               making the sample trace a reliable demo without any
                               nanny.toml edits.
    @nanny_agent("debugger") — activates [limits.debugger] scope for the run.
"""

from __future__ import annotations

import re
from typing import Any, TypedDict

from langchain_core.messages import HumanMessage
from langchain_groq import ChatGroq
from langgraph.graph import END, START, StateGraph

from nanny_sdk import agent as nanny_agent, rule
from nanny_sdk.exceptions import NannyStop  # noqa: F401 — propagates through graph naturally

from dev_assist.config import MODEL
from dev_assist.tools import file_reader, ripgrep

# ── Rule ──────────────────────────────────────────────────────────────────────

_seen_files: set[str] = set()


@rule("no_read_loop")
def check_no_read_loop(ctx: Any) -> bool:
    """Deny if the same file is read more than once.

    A healthy agent reads each file once — the content stays in state and
    can be referenced during diagnosis without re-reading. Firing on the
    second read catches both runaway loops and redundant duplicate frames
    extracted from the stack trace.
    """
    if getattr(ctx, "requested_tool", "") == "file_reader":
        path = (ctx.last_tool_args or {}).get("path", "")
        if path in _seen_files:
            return False
        if path:
            _seen_files.add(path)
    return True


# ── State ─────────────────────────────────────────────────────────────────────


class DebugState(TypedDict):
    trace: str                   # input: full stack trace text
    file_paths: list[str]        # file paths extracted from trace (duplicates preserved)
    search_patterns: list[str]   # exception class / key identifiers to search for
    files_read: str              # concatenated file_reader results
    search_results: str          # concatenated ripgrep results
    diagnosis: str               # final LLM synthesis


# ── Helpers ───────────────────────────────────────────────────────────────────

_PATH_RE = re.compile(r'File "([^"]+\.(?:py|ts|js|go|rs|java))"')
_EXC_RE  = re.compile(r"^([A-Z][A-Za-z0-9_]+):\s", re.MULTILINE)
_VAL_RE  = re.compile(r"(?:KeyError|AttributeError):\s*['\"]?(\w+)['\"]?")


def _extract_file_paths(trace: str) -> list[str]:
    """Extract file paths from a stack trace, preserving duplicates.

    Duplicates are intentional: @rule("no_read_loop") fires when the same
    path is attempted a second time, so the sample_trace.txt (which has
    pipeline.py and normalizer.py in two frames each) makes the demo
    deterministic without any manual nanny.toml edits.
    """
    return _PATH_RE.findall(trace)


def _extract_search_patterns(trace: str) -> list[str]:
    """Extract the exception class name and the key error value from the trace."""
    patterns: list[str] = []
    for m in _EXC_RE.finditer(trace):
        cls = m.group(1)
        if cls not in patterns:
            patterns.append(cls)
    val_m = _VAL_RE.search(trace)
    if val_m and val_m.group(1) not in patterns:
        patterns.append(val_m.group(1))
    return patterns[:3]  # cap at 3 — one ripgrep call per pattern


# ── Nodes ─────────────────────────────────────────────────────────────────────


def extract_node(state: DebugState) -> dict[str, Any]:
    """Extract file paths and search patterns from the stack trace.

    Pure Python — no tool calls. Populates file_paths and search_patterns
    so the subsequent nodes know exactly what to read and search.
    """
    return {
        "file_paths":       _extract_file_paths(state["trace"]),
        "search_patterns":  _extract_search_patterns(state["trace"]),
    }


def read_files_node(state: DebugState) -> dict[str, Any]:
    """Read each source file referenced in the stack trace.

    Python drives every file_reader call — @nanny_tool fires on every call.
    Paths are iterated in extraction order including duplicates.
    @rule("no_read_loop") fires when the same path is attempted twice.
    NannyStop propagates out of the LangGraph execution naturally.
    """
    parts: list[str] = []
    for path in state["file_paths"]:
        content = file_reader(path=path)
        parts.append(f"### {path}\n{content}")
    return {"files_read": "\n\n---\n\n".join(parts)}


def search_node(state: DebugState) -> dict[str, Any]:
    """Search the codebase for key symbols extracted from the stack trace.

    Python drives every ripgrep call — @nanny_tool fires on every call.
    NannyStop propagates out of the LangGraph execution naturally.
    """
    parts: list[str] = []
    for pattern in state["search_patterns"]:
        result = ripgrep(pattern=pattern, path=".")
        parts.append(f"### rg {pattern!r}\n{result}")
    return {"search_results": "\n\n---\n\n".join(parts)}


_DIAGNOSE_PROMPT = """\
You are a debug assistant. Diagnose the error below using the source files \
and search results provided. Be concise and precise.

Stack trace:
{trace}

Source files:
{files}

Search results:
{searches}

Respond with:
Root cause: <one sentence>
Files involved: <paths>
Fix: <diff or clear description>"""


def diagnose_node(state: DebugState) -> dict[str, Any]:
    """Synthesise a diagnosis from the gathered context.

    Pure LLM reasoning — no tool calls. Uses ChatGroq for reliable,
    structured output. Nanny limits fire before this node if a tool
    call was denied or a budget/step limit was reached.
    """
    llm = ChatGroq(model=MODEL, temperature=0)
    prompt = _DIAGNOSE_PROMPT.format(
        trace=state["trace"],
        files=state["files_read"] or "(none — no file paths found in trace)",
        searches=state["search_results"] or "(none — no patterns extracted)",
    )
    response = llm.invoke([HumanMessage(content=prompt)])
    return {"diagnosis": str(response.content)}


# ── Graph ─────────────────────────────────────────────────────────────────────

def _build_graph() -> Any:
    g = StateGraph(DebugState)
    g.add_node("extract",    extract_node)
    g.add_node("read_files", read_files_node)
    g.add_node("search",     search_node)
    g.add_node("diagnose",   diagnose_node)
    g.add_edge(START,        "extract")
    g.add_edge("extract",    "read_files")
    g.add_edge("read_files", "search")
    g.add_edge("search",     "diagnose")
    g.add_edge("diagnose",   END)
    return g.compile()


_graph = _build_graph()


# ── Public API ────────────────────────────────────────────────────────────────


@nanny_agent("debugger")
def run_debug(trace_text: str) -> str:
    """Analyse a stack trace and return a diagnosis.

    Runs the LangGraph pipeline: extract → read_files → search → diagnose.
    Python drives every tool call — @nanny_tool fires on file_reader and
    ripgrep. The LLM only synthesises in diagnose_node.

    Raises a NannyStop subclass if the agent is stopped early by governance.
    The caller (cli/main.py) handles those and presents a user-facing message.
    """
    _seen_files.clear()  # reset per-run so repeated CLI invocations start fresh

    initial_state: DebugState = {
        "trace":            trace_text,
        "file_paths":       [],
        "search_patterns":  [],
        "files_read":       "",
        "search_results":   "",
        "diagnosis":        "",
    }

    final_state = _graph.invoke(initial_state)
    return final_state.get("diagnosis", "[no diagnosis produced]")
