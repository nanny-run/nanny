"""Destructive and financial: the actions people actually get fired over.

Nobody is accountable for a token count. They are accountable when an agent
deletes the wrong record or moves money it should not have. These rules are
denominated in the units those conversations happen in.
"""

from __future__ import annotations

import re

from nanny_sdk import rule
from nanny_sdk._context import PolicyContext

CONFIRM_TOOL = "confirm_destructive"
AMOUNT = re.compile(r"(?<![\w.])(\d+(?:\.\d{1,2})?)(?![\w.])")


def _amounts(ctx: PolicyContext) -> list[float]:
    out: list[float] = []
    for key, value in ctx.last_tool_args.items():
        if any(k in key.lower() for k in ("amount", "total", "value", "sum", "price")):
            out.extend(float(m) for m in AMOUNT.findall(value))
    return out


@rule("no_destructive_without_confirmation")
def no_destructive_without_confirmation(ctx: PolicyContext) -> bool:
    """Deny an irreversible action that no confirmation step preceded."""
    pending = ctx.requested_tool
    if pending is None or not ctx.tool_has(pending, "destructive"):
        return True
    return CONFIRM_TOOL in ctx.tool_call_history


@rule("no_payment_without_prior_approval")
def no_payment_without_prior_approval(ctx: PolicyContext) -> bool:
    """Deny a payment that no approval step preceded."""
    pending = ctx.requested_tool
    if pending is None or not ctx.tool_has(pending, "moves_money"):
        return True
    return "request_approval" in ctx.tool_call_history
