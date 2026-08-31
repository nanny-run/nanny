#!/usr/bin/env python3
"""Gate on open critical/high Dependabot alerts, respecting .security-allowlist.jsonc.

Reads a JSON array of Dependabot alert objects from stdin (the shape
returned by `GET /repos/{owner}/{repo}/dependabot/alerts`). Filters to
`state == "open"` and `severity in {critical, high}`.

Findings are cross-referenced against .security-allowlist.jsonc by alert
number. An alert covered by an ACTIVE, non-expired entry is reported but
does not fail the gate. Anything else does. Exit 1 if any unallowlisted
(or allowlisted-but-expired) finding remains.

Usage:
  gh api "repos/OWNER/REPO/dependabot/alerts" --paginate --slurp \
    | python3 scripts/security_audit_gate.py
"""

from __future__ import annotations

import argparse
import json
import sys
from datetime import datetime, timezone
from pathlib import Path

REPO_ROOT = Path(__file__).resolve().parent.parent
ALLOWLIST_PATH = REPO_ROOT / ".security-allowlist.jsonc"

sys.path.insert(0, str(REPO_ROOT / "scripts"))
from check_security_allowlist import load_allowlist, parse_expiry  # noqa: E402


def load_pages(raw_stdin: str) -> list[dict]:
    """`gh api --paginate --slurp` yields a JSON array of per-page arrays;
    flatten it. Also accept a single flat array for local/manual testing."""
    data = json.loads(raw_stdin)
    if data and isinstance(data[0], list):
        return [alert for page in data for alert in page]
    return data


def matching_allowlist_entry(alert_number: int, allowlist: list[dict]) -> dict | None:
    today = datetime.now(timezone.utc).date()
    for entry in allowlist:
        if entry.get("alert") != alert_number:
            continue
        if entry.get("active", True) is False:
            continue
        expiry = parse_expiry(entry.get("expiry", ""))
        if expiry is not None and expiry < today:
            continue  # expired, does not cover the alert, falls through to failing
        return entry
    return None


def main() -> int:
    argparse.ArgumentParser().parse_args()

    alerts = load_pages(sys.stdin.read())
    relevant = [
        a
        for a in alerts
        if a.get("state") == "open"
        and a.get("security_vulnerability", {}).get("severity") in ("critical", "high")
    ]

    if not relevant:
        print("No open critical/high alerts.")
        return 0

    allowlist = load_allowlist()

    failing: list[dict] = []
    covered: list[tuple[dict, dict]] = []
    for alert in relevant:
        entry = matching_allowlist_entry(alert["number"], allowlist)
        if entry:
            covered.append((alert, entry))
        else:
            failing.append(alert)

    def describe(a: dict) -> str:
        dep = a["dependency"]
        sev = a["security_vulnerability"]["severity"]
        return f"  #{a['number']}  {sev:<8}  {dep['manifest_path']:<30}  {dep['package']['ecosystem']}/{dep['package']['name']}  {a['html_url']}"

    if covered:
        print(f"{len(covered)} alert(s) covered by an active allowlist entry:")
        for alert, entry in covered:
            print(describe(alert) + f"  (expires {entry.get('expiry')})")
        print()

    if failing:
        print("Open critical/high alerts, not covered by an active allowlist entry:")
        for alert in failing:
            print(describe(alert))
        print()
        print("Fix (bump the dependency), or add a .security-allowlist.jsonc entry with a real")
        print("expiry and traced reason if it genuinely can't be fixed right now.")
        return 1

    print("No unallowlisted open critical/high alerts.")
    return 0


if __name__ == "__main__":
    sys.exit(main())
