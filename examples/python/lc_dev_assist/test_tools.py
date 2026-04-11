"""Day 7 smoke test — tools in isolation.

Verifies:
  1. Decorator stack is correct: @lc_tool outer, @nanny_tool inner.
  2. LangChain tool metadata (name, description) is set.
  3. In passthrough mode both tools run directly — no bridge, no exceptions.
  4. file_reader returns file contents / sensible errors.
  5. ripgrep returns matches / "no matches" string — no subprocess crash.

Run:
    uv run python test_tools.py
"""

from __future__ import annotations

import os
import sys
import tempfile
from pathlib import Path

# Guarantee passthrough mode: no bridge env vars set
os.environ.pop("NANNY_BRIDGE_SOCKET", None)
os.environ.pop("NANNY_BRIDGE_PORT", None)

from tools import file_reader, ripgrep  # noqa: E402 (must come after env setup)

PASS = "✓"
FAIL = "✗"
results: list[tuple[str, bool, str]] = []


def check(name: str, condition: bool, detail: str = "") -> None:
    results.append((name, condition, detail))
    status = PASS if condition else FAIL
    line = f"  {status}  {name}"
    if detail:
        line += f"  —  {detail}"
    print(line)


# ---------------------------------------------------------------------------
# 1. LangChain tool metadata
# ---------------------------------------------------------------------------

print("\nLangChain tool structure")
check("file_reader has .name", hasattr(file_reader, "name"), file_reader.name)
check("ripgrep has .name", hasattr(ripgrep, "name"), ripgrep.name)
check("file_reader has .description", bool(getattr(file_reader, "description", "")))
check("ripgrep has .description", bool(getattr(ripgrep, "description", "")))

# ---------------------------------------------------------------------------
# 2. file_reader — passthrough
# ---------------------------------------------------------------------------

print("\nfile_reader (passthrough)")

with tempfile.NamedTemporaryFile(mode="w", suffix=".py", delete=False) as f:
    f.write("def hello():\n    return 'world'\n")
    tmp_path = f.name

try:
    contents = file_reader.invoke({"path": tmp_path})
    check("reads an existing file", "def hello" in contents, repr(contents[:60]))

    missing = file_reader.invoke({"path": "/nonexistent/file.py"})
    check("returns error for missing file", "[error]" in missing, repr(missing[:60]))

    dir_result = file_reader.invoke({"path": "."})
    check("returns error for directory input", "[error]" in dir_result, repr(dir_result[:60]))
finally:
    Path(tmp_path).unlink(missing_ok=True)

# ---------------------------------------------------------------------------
# 3. file_reader — truncation
# ---------------------------------------------------------------------------

print("\nfile_reader truncation")

with tempfile.NamedTemporaryFile(mode="w", suffix=".txt", delete=False) as f:
    f.write("x" * 10_000)
    large_path = f.name

try:
    large = file_reader.invoke({"path": large_path})
    check("truncates large files", "truncated" in large, f"{len(large)} chars returned")
finally:
    Path(large_path).unlink(missing_ok=True)

# ---------------------------------------------------------------------------
# 4. ripgrep — passthrough
# ---------------------------------------------------------------------------

print("\nripgrep (passthrough)")

# Search within this file — guaranteed to have matches
this_file = str(Path(__file__).resolve())
rg_hit = ripgrep.invoke({"pattern": "smoke test", "path": this_file})
check("finds matches in this file", "smoke test" in rg_hit.lower(), repr(rg_hit[:80]))

with tempfile.NamedTemporaryFile(mode="w", suffix=".py", delete=False) as f:
    f.write("hello = 'world'\n")
    rg_miss_path = f.name
try:
    rg_miss = ripgrep.invoke({"pattern": "PATTERN_THAT_DOES_NOT_EXIST", "path": rg_miss_path})
    check("reports no matches cleanly", "no matches" in rg_miss.lower(), repr(rg_miss[:80]))
finally:
    Path(rg_miss_path).unlink(missing_ok=True)

# ---------------------------------------------------------------------------
# Summary
# ---------------------------------------------------------------------------

passed = sum(1 for _, ok, _ in results if ok)
total = len(results)
print(f"\n{'─' * 50}")
print(f"  {passed}/{total} checks passed")
if passed < total:
    print("\nFailed checks:")
    for name, ok, detail in results:
        if not ok:
            print(f"  {FAIL}  {name}  —  {detail}")
    sys.exit(1)
else:
    print("  All good. Tools ready for Day 8.")
