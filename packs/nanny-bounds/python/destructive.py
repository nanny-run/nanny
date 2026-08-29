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


@rule("cap_destructive_calls_per_run")
def cap_destructive_calls_per_run(ctx: PolicyContext) -> bool:
    """Bound irreversible actions per run."""
    limit = 3
    used = sum(
        count for tool, count in ctx.tool_call_counts.items() if ctx.tool_has(tool, "destructive")
    )
    return used < limit


@rule("no_payment_above_threshold")
def no_payment_above_threshold(ctx: PolicyContext) -> bool:
    """Deny a single payment above a declared amount.

    A spend limit denominated in money, which is the unit a founder can reason
    about, rather than in tokens, which is not.
    """
    threshold = 1_000.0
    pending = ctx.requested_tool
    if pending is None or not ctx.tool_has(pending, "moves_money"):
        return True
    return all(a <= threshold for a in _amounts(ctx))


@rule("cap_money_moving_calls_per_run")
def cap_money_moving_calls_per_run(ctx: PolicyContext) -> bool:
    """Bound how many payments one run may make."""
    limit = 5
    used = sum(
        count for tool, count in ctx.tool_call_counts.items() if ctx.tool_has(tool, "moves_money")
    )
    return used < limit


@rule("cap_cumulative_amount_per_run")
def cap_cumulative_amount_per_run(ctx: PolicyContext) -> bool:
    """Bound the total moved in one run.

    Per-call thresholds are trivially defeated by splitting a payment, so the
    cumulative figure is the one that binds.
    """
    ceiling = 5_000.0
    pending = ctx.requested_tool
    if pending is None or not ctx.tool_has(pending, "moves_money"):
        return True
    return sum(_amounts(ctx)) <= ceiling
