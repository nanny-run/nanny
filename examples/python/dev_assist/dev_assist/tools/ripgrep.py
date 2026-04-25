"""ripgrep tool — searches for patterns across source files."""

from __future__ import annotations

import subprocess

from nanny_sdk import tool as nanny_tool

_RG_LINE_LIMIT = 50
_RG_TIMEOUT_S  = 15


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
