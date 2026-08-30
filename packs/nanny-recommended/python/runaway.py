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
    """Deny an argument-less tool called twice in a row.

    A call with no arguments carries nothing to distinguish it from the one
    before, so repeating it immediately cannot produce information the first did
    not: the signature of a stuck loop.

    **It deliberately stops there.** The obvious rule is "same tool, same
    arguments", and it is not implementable with this context:
    ``tool_call_history`` holds names only and ``last_tool_args`` belongs to the
    *pending* call, so the previous call's arguments are not available to
    compare against. An earlier version of this rule claimed to make that
    comparison and, having no arguments to compare, denied every consecutive
    call that had any arguments at all, which refuses an ordinary research loop
    on its second search. Narrow and true beats broad and wrong, especially in a
    pack that installs unread.
    """
    pending = ctx.requested_tool
    if pending is None or not ctx.tool_call_history:
        return True
    if ctx.tool_call_history[-1] != pending:
        return True
    return bool(ctx.last_tool_args)
