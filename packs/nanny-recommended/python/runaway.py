"""Loop and runaway: bound repetition, denominated in actions.

This group is what step ceilings and token budgets were reaching for, expressed
as authority over actions instead of resource consumption. A step count could
never be set correctly because nobody knows their agent's normal step count
until production. "This agent may call the same tool with the same arguments at
most twice" is a sentence an operator can write on day one and defend in a
review.
"""

from nanny_sdk import rule
from nanny_sdk._context import PolicyContext


@rule("no_identical_repeat_call")
def no_identical_repeat_call(ctx: PolicyContext) -> bool:
    """Deny the same tool with the same arguments twice in a row.

    The signature of a stuck loop. Identical arguments mean the agent is not
    responding to what it learned, so the second call cannot produce information
    the first did not.
    """
    pending = ctx.requested_tool
    if pending is None or not ctx.tool_call_history:
        return True
    if ctx.tool_call_history[-1] != pending:
        return True
    return not ctx.last_tool_args


@rule("no_consecutive_identical_tool")
def no_consecutive_identical_tool(ctx: PolicyContext) -> bool:
    """Deny the same tool N times consecutively, regardless of arguments."""
    limit = 5
    pending = ctx.requested_tool
    if pending is None:
        return True
    run = 0
    for tool in reversed(ctx.tool_call_history):
        if tool != pending:
            break
        run += 1
    return run < limit


@rule("cap_external_effect_calls_per_run")
def cap_external_effect_calls_per_run(ctx: PolicyContext) -> bool:
    """Bound how many outward actions one run may take.

    The blast radius of a run that goes wrong, stated as a number.
    """
    limit = 10
    used = sum(
        count
        for tool, count in ctx.tool_call_counts.items()
        if ctx.tool_has(tool, "external_effect")
    )
    return used < limit


@rule("cap_total_tool_calls_per_run")
def cap_total_tool_calls_per_run(ctx: PolicyContext) -> bool:
    """Bound total tool calls in a run."""
    limit = 200
    return sum(ctx.tool_call_counts.values()) < limit
