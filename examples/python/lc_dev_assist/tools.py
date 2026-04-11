"""Tools available to the dev_assist debug agent.

file_reader  — reads a source file and returns its contents as text.
               Useful when the stack trace points to a specific file.

ripgrep      — searches for a pattern or symbol across a directory tree.
               Useful when you need to find where a function or class is defined.

Both tools are exposed to LangChain and rate-limited in production via nanny.toml.
"""

from __future__ import annotations

import subprocess
from pathlib import Path

from langchain_core.tools import tool as lc_tool
from nanny_sdk import tool as nanny_tool

# ---------------------------------------------------------------------------
# file_reader
# ---------------------------------------------------------------------------

_FILE_CHAR_LIMIT = 8_000


@lc_tool
@nanny_tool(cost=5)
def file_reader(path: str) -> str:
    """Read a source file from disk and return its full contents.

    Use this to inspect files mentioned in a stack trace or error log.
    Supports any text file: Python, TypeScript, Rust, Go, logs, configs.
    """
    p = Path(path)
    if not p.exists():
        return f"[error] file not found: {path}"
    if not p.is_file():
        return f"[error] not a file: {path}"
    try:
        text = p.read_text(encoding="utf-8", errors="replace")
    except OSError as exc:
        return f"[error] could not read {path}: {exc}"

    if len(text) > _FILE_CHAR_LIMIT:
        text = (
            text[:_FILE_CHAR_LIMIT]
            + f"\n\n... [truncated — {len(text):,} chars total, showing first {_FILE_CHAR_LIMIT:,}]"
        )
    return text


# ---------------------------------------------------------------------------
# ripgrep
# ---------------------------------------------------------------------------

_RG_LINE_LIMIT = 50
_RG_TIMEOUT_S = 15


@lc_tool
@nanny_tool(cost=8)
def ripgrep(pattern: str, path: str = ".") -> str:
    """Search for a symbol, function name, or pattern across source files.

    Uses ripgrep (rg) for fast, recursive search. Returns matching lines
    with file names and line numbers so you can decide which files to read.

    Args:
        pattern: Regular expression or literal string to search for.
        path: File or directory to search in. Defaults to current directory.
    """
    try:
        result = subprocess.run(
            [
                "rg",
                "--with-filename",
                "--line-number",
                "--color=never",
                "--max-count=3",   # at most 3 matches per file — keeps output tight
                pattern,
                path,
            ],
            capture_output=True,
            text=True,
            timeout=_RG_TIMEOUT_S,
        )
    except FileNotFoundError:
        return "[error] ripgrep (rg) not found — install with: brew install ripgrep"
    except subprocess.TimeoutExpired:
        return f"[error] ripgrep timed out after {_RG_TIMEOUT_S}s"

    # rg exits 1 when no matches found (not an error)
    if result.returncode == 1:
        return f"No matches found for: {pattern!r} in {path}"
    if result.returncode != 0:
        return f"[error] ripgrep: {result.stderr.strip()}"

    lines = result.stdout.strip().splitlines()
    if len(lines) > _RG_LINE_LIMIT:
        truncated = len(lines) - _RG_LINE_LIMIT
        lines = lines[:_RG_LINE_LIMIT]
        lines.append(f"\n... [{truncated} more lines truncated — narrow the pattern or path]")

    return "\n".join(lines)
