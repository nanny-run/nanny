"""Debug agent for dev_assist — LangGraph StateGraph.

Architecture: explicit Python-driven nodes.
    Python calls every tool (file_reader, ripgrep) directly in graph nodes.
    @nanny_tool fires on every call — enforcement is guaranteed regardless
    of model. The LLM is used only in diagnose_node for pure synthesis.

Execution order:
    extract_node      — Python extracts file paths and search patterns from the trace
    read_files_node   — Python calls file_reader for each unique extracted path
    search_node       — Python calls ripgrep for each extracted pattern
    diagnose_node     — LLM synthesises a diagnosis from all gathered context
    apply_patch_node  — Python attempts write_file to apply the fix (blocked by ToolDenied)

Governance:
    [tools.file_reader] max_calls — caps total file reads; RuleDenied fires when exceeded.
                                    Set low in demo-rule-denied tape via sed before running.
    @agent("debugger")            — activates [limits.debugger] scope for the run.
"""

from __future__ import annotations

import re
from pathlib import Path
from typing import Any, TypedDict

from langchain_core.messages import HumanMessage
from langchain_groq import ChatGroq
from langgraph.graph import END, START, StateGraph

from nanny_sdk import agent
from nanny_sdk._client import is_passthrough
from nanny_sdk.exceptions import NannyStop  # noqa: F401 — propagates through graph naturally

from dev_assist.config import MODEL
from dev_assist.tools import file_reader, ripgrep, write_file

# ── State ─────────────────────────────────────────────────────────────────────


class DebugState(TypedDict):
    trace: str                   # input: full stack trace text
    # file paths extracted from trace (duplicates preserved)
    file_paths: list[str]
    # exception class / key identifiers to search for
    search_patterns: list[str]
    files_read: str              # concatenated file_reader results
    search_results: str          # concatenated ripgrep results
    diagnosis: str               # final LLM synthesis
    patch_path: str              # target file for apply_patch_node write attempt


# ── Helpers ───────────────────────────────────────────────────────────────────

_PATH_RE = re.compile(r'File "([^"]+\.(?:py|ts|js|go|rs|java))"')
_EXC_RE = re.compile(r"^([A-Z][A-Za-z0-9_]+):\s", re.MULTILINE)
_VAL_RE = re.compile(r"(?:KeyError|AttributeError):\s*['\"]?(\w+)['\"]?")


def _extract_file_paths(trace: str) -> list[str]:
    """Extract file paths from a stack trace, preserving duplicates.

    Duplicates in the trace are ignored — read_files_node deduplicates paths
    so each file is read exactly once regardless of how many frames reference it.
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
    """Read each unique source file referenced in the stack trace.

    Python drives every file_reader call — @nanny_tool fires on every call.
    Paths are deduplicated (order-preserving) so the same file is never read twice.
    [tools.file_reader] max_calls caps the total number of reads; RuleDenied fires
    when exceeded (see demo-rule-denied tape).
    NannyStop propagates out of the LangGraph execution naturally.
    """
    parts: list[str] = []
    for path in dict.fromkeys(state["file_paths"]):
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
    # Resolve the innermost unique file as the patch target.
    seen: set[str] = set()
    target = ""
    for p in state["file_paths"]:
        if p not in seen:
            seen.add(p)
            target = p
    return {"diagnosis": str(response.content), "patch_path": target}


def apply_patch_node(state: DebugState) -> dict[str, Any]:
    """Write the suggested fix to the target file.

    Under `nanny run` with the default allowlist: write_file is permitted and
    the patch is written. Under the demo-tool-denied tape: write_file is removed
    from the allowlist via sed before running, so ToolDenied fires here instead.

    In passthrough (dev mode, no nanny): writes to reports/<filename>.patch instead
    of overwriting the source file.
    """
    fix_path = state.get("patch_path") or "patch.py"
    if is_passthrough():
        fix_path = f"reports/{Path(fix_path).name}.patch"
        Path("reports").mkdir(exist_ok=True)
    write_file(path=fix_path, content=state["diagnosis"])
    return {}


# ── Graph ─────────────────────────────────────────────────────────────────────

def _build_graph() -> Any:
    g = StateGraph(DebugState)
    g.add_node("extract",      extract_node)
    g.add_node("read_files",   read_files_node)
    g.add_node("search",       search_node)
    g.add_node("diagnose",     diagnose_node)
    g.add_node("apply_patch",  apply_patch_node)
    g.add_edge(START,          "extract")
    g.add_edge("extract",      "read_files")
    g.add_edge("read_files",   "search")
    g.add_edge("search",       "diagnose")
    g.add_edge("diagnose",     "apply_patch")
    g.add_edge("apply_patch",  END)
    return g.compile()


_graph = _build_graph()


# ── Public API ────────────────────────────────────────────────────────────────


@agent("debugger")
def run_debug(trace_text: str) -> str:
    """Analyse a stack trace and return a diagnosis.

    Runs the LangGraph pipeline: extract → read_files → search → diagnose → apply_patch.
    Python drives every tool call — @nanny_tool fires on file_reader, ripgrep, and
    write_file. The LLM only synthesises in diagnose_node.

    Raises a NannyStop subclass if the agent is stopped early by governance.
    The caller (cli/main.py) handles those and presents a user-facing message.
    """
    initial_state: DebugState = {
        "trace":            trace_text,
        "file_paths":       [],
        "search_patterns":  [],
        "files_read":       "",
        "search_results":   "",
        "diagnosis":        "",
        "patch_path":       "",
    }

    final_state = _graph.invoke(initial_state)
    return final_state.get("diagnosis", "[no diagnosis produced]")
