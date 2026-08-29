"""Operational: govern the circumstances an action is permitted in."""

from __future__ import annotations

import datetime as _dt

from nanny_sdk import rule
from nanny_sdk._context import PolicyContext


@rule("no_external_effect_outside_operating_hours")
def no_external_effect_outside_operating_hours(ctx: PolicyContext) -> bool:
    """Deny outward actions outside declared hours.

    Reads ``ctx.now_ms`` rather than the clock. A rule that called
    ``datetime.now()`` would reach outside its context to decide, which makes it
    untestable and breaks the guarantee that identical inputs produce identical
    behaviour. Time is an input, so it arrives as one.
    """
    open_hour, close_hour = 6, 22
    pending = ctx.requested_tool
    if pending is None or not ctx.tool_has(pending, "external_effect"):
        return True
    hour = _dt.datetime.fromtimestamp(ctx.now_ms / 1000, tz=_dt.timezone.utc).hour
    return open_hour <= hour < close_hour
