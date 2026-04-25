"""file_reader tool — reads a source file and returns its contents."""

from __future__ import annotations

from pathlib import Path

from nanny_sdk import tool as nanny_tool

_FILE_CHAR_LIMIT = 8_000


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
