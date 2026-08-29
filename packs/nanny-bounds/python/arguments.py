"""Argument safety: govern what is being passed, never what it means.

Invariant 6 bans reading *content* to decide, and explicitly permits the
identity, arguments, labels and ordering of actions. These rules match on
arguments. They do not interpret them, ask a model about them, or reason about
intent. A regex over an argument string is a deterministic function; anything
that inferred meaning would not be.
"""

from __future__ import annotations

import re

from nanny_sdk import rule
from nanny_sdk._context import PolicyContext

SECRET_PATTERNS = [
    re.compile(r"\bsk-[A-Za-z0-9]{16,}\b"),
    re.compile(r"\bghp_[A-Za-z0-9]{20,}\b"),
    re.compile(r"\bAKIA[0-9A-Z]{16}\b"),
    re.compile(r"-----BEGIN [A-Z ]*PRIVATE KEY-----"),
]

PII_PATTERNS = [
    re.compile(r"\b\d{3}-\d{2}-\d{4}\b"),
    re.compile(r"\b(?:\d[ -]*?){13,16}\b"),
]

SHELL_METACHARACTERS = re.compile(r"[;&|`$><]|\$\(")
SQL_PATTERNS = re.compile(
    r"(?i)\b(drop\s+table|delete\s+from|truncate\s+table|;\s*--|union\s+select)\b"
)

# Matched literally, never resolved.
#
# The draft of this rule said "deny URLs *resolving to* private ranges". A DNS
# lookup during enforcement is a network call on the deterministic path, and DNS
# answers change over time, so the same call could be allowed on Monday and
# denied on Tuesday with nothing in the config having changed. That is a direct
# violation of the determinism invariant. Matching declared patterns is
# genuinely narrower than the host guard it replaces, and the docs say so rather
# than implying parity.
PRIVATE_HOST_PATTERNS = [
    re.compile(r"^https?://(localhost|127\.\d+\.\d+\.\d+|\[::1\])(:\d+)?(/|$)", re.I),
    re.compile(r"^https?://10\.\d+\.\d+\.\d+(:\d+)?(/|$)"),
    re.compile(r"^https?://192\.168\.\d+\.\d+(:\d+)?(/|$)"),
    re.compile(r"^https?://172\.(1[6-9]|2\d|3[01])\.\d+\.\d+(:\d+)?(/|$)"),
    re.compile(r"^https?://169\.254\.169\.254(:\d+)?(/|$)"),
]


def _args(ctx: PolicyContext) -> str:
    return "\n".join(ctx.last_tool_args.values())


@rule("no_oversized_args")
def no_oversized_args(ctx: PolicyContext) -> bool:
    """Deny an argument payload past a declared size.

    Bulk is the shape of exfiltration: nobody emails a colleague sixty thousand
    characters by accident.
    """
    limit = 64_000
    return len(_args(ctx)) < limit
