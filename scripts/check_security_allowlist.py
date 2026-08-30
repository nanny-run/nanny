#!/usr/bin/env python3
"""Validate .security-allowlist.jsonc's own hygiene.

Fails if an entry looks unreviewed: missing/placeholder notes, no expiry, or
an already-expired entry left active. The allowlist is a reviewed baseline,
not a place to silently suppress a new finding, this catches the case
where someone adds an alert number with a lazy or copy-pasted note just to
make the gate pass.

Run standalone: python3 scripts/check_security_allowlist.py
Also run as the first step of security-audit.yml's shipped-code job.
"""

from __future__ import annotations

import json
import re
import sys
from datetime import date, datetime, timezone
from pathlib import Path

REPO_ROOT = Path(__file__).resolve().parent.parent
ALLOWLIST_PATH = REPO_ROOT / ".security-allowlist.jsonc"

MIN_NOTES_LENGTH = 40
PLACEHOLDER_PATTERNS = [
    re.compile(p, re.IGNORECASE)
    for p in [r"^todo", r"^tbd", r"^n/?a$", r"^ignore$", r"^known issue$", r"^false positive$", r"^unused$"]
]


def strip_jsonc_comments(text: str) -> str:
    """Strip `// line comments` outside of strings, then trailing commas
    before a closing `}`/`]`. Good enough for this file's own formatting
    (no `//` inside string values here); not a general JSONC parser."""
    out_lines = []
    for line in text.split("\n"):
        in_string = False
        cut_at = None
        i = 0
        while i < len(line):
            ch = line[i]
            if ch == '"' and (i == 0 or line[i - 1] != "\\"):
                in_string = not in_string
            if not in_string and ch == "/" and i + 1 < len(line) and line[i + 1] == "/":
                cut_at = i
                break
            i += 1
        out_lines.append(line if cut_at is None else line[:cut_at])
    without_comments = "\n".join(out_lines)
    return re.sub(r",(\s*[}\]])", r"\1", without_comments)


def load_allowlist() -> list[dict]:
    if not ALLOWLIST_PATH.exists():
        print("check_security_allowlist: no .security-allowlist.jsonc found, nothing to check.")
        return []
    raw = ALLOWLIST_PATH.read_text(encoding="utf-8")
    try:
        config = json.loads(strip_jsonc_comments(raw))
    except json.JSONDecodeError as e:
        print(f"FAIL: .security-allowlist.jsonc is not valid JSON(C): {e}")
        sys.exit(1)
    return config.get("allowlist", [])


def parse_expiry(value: str) -> date | None:
    try:
        return datetime.fromisoformat(value).date()
    except ValueError:
        return None


def validate(allowlist: list[dict]) -> list[str]:
    offenses: list[str] = []
    today = datetime.now(timezone.utc).date()

    for entry in allowlist:
        alert = entry.get("alert", "<missing alert number>")
        label = f"alert #{alert}"

        if "alert" not in entry or not isinstance(entry["alert"], int):
            offenses.append(f"{label}: missing or non-integer \"alert\" (the Dependabot alert number)")

        expiry_raw = entry.get("expiry")
        if not expiry_raw:
            offenses.append(f"{label}: missing \"expiry\"")
        else:
            expiry = parse_expiry(expiry_raw)
            if expiry is None:
                offenses.append(f"{label}: \"expiry\" ({expiry_raw!r}) is not a parseable ISO date (YYYY-MM-DD)")
            elif entry.get("active", True) is not False and expiry < today:
                offenses.append(
                    f"{label}: expiry ({expiry_raw}) has passed and the entry is still active, "
                    "resolve the finding or extend the expiry with a reason"
                )

        notes = (entry.get("notes") or "").strip()
        if len(notes) < MIN_NOTES_LENGTH:
            offenses.append(
                f"{label}: \"notes\" is missing or too short ({len(notes)} chars, need {MIN_NOTES_LENGTH}+), "
                "trace the actual reason, don't just name the package"
            )
        elif any(p.match(notes) for p in PLACEHOLDER_PATTERNS):
            offenses.append(f"{label}: \"notes\" looks like a placeholder ({notes!r})")

    return offenses


def main() -> int:
    allowlist = load_allowlist()
    print(f"check_security_allowlist: checked {len(allowlist)} allowlist entr{'y' if len(allowlist) == 1 else 'ies'}.")

    offenses = validate(allowlist)
    if not offenses:
        print("OK: every entry has a real expiry and a traced justification.")
        return 0

    print(f"\nFAIL: {len(offenses)} allowlist entry issue(s):\n")
    for off in offenses:
        print(f"  {off}")
    print(
        "\nEach .security-allowlist.jsonc entry should trace why the finding can't be fixed with "
        "cargo update/uv lock --upgrade-package, and carry a real expiry, not a placeholder. "
        "See .security-allowlist.jsonc's own header for the reasoning."
    )
    return 1


if __name__ == "__main__":
    sys.exit(main())
