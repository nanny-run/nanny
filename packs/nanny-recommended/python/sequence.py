"""Sequence and authority: govern the order actions may happen in.

Authority is not only *what* an agent may do but *when*. A payment tool that is
safe after an approval step is not the same tool before one, and nothing about
the call itself distinguishes the two. Only the ordering does.
"""

from nanny_sdk import rule
from nanny_sdk._context import PolicyContext

APPROVAL_TOOL = "request_approval"


@rule("require_approval_before_external_effect")
def require_approval_before_external_effect(ctx: PolicyContext) -> bool:
    """Deny an outward action unless an approval step ran first."""
    pending = ctx.requested_tool
    if pending is None or not ctx.tool_has(pending, "external_effect"):
        return True
    return APPROVAL_TOOL in ctx.tool_call_history


@rule("no_tool_after_a_prior_denial")
def no_tool_after_a_prior_denial(ctx: PolicyContext) -> bool:
    """Deny everything once a denial has already occurred in this run.

    A stop is final, so in practice this guards the case where an SDK-side rule
    denied and the agent tried to continue anyway. An agent that keeps calling
    after being refused is not recovering, it is retrying a blocked action, and
    the second attempt deserves no more benefit of the doubt than the first.
    """
    return "__denied__" not in ctx.tool_call_history


@rule("no_agent_spawn_beyond_depth")
def no_agent_spawn_beyond_depth(ctx: PolicyContext) -> bool:
    """Bound how many sub-agents a run may create.

    Delegation multiplies authority: every spawned agent inherits the ability to
    act, and a run that has spawned eight of them has stopped being one
    accountable actor.
    """
    max_depth = 3
    spawns = sum(
        count for tool, count in ctx.tool_call_counts.items() if tool.startswith("spawn_")
    )
    return spawns < max_depth


@rule("prerequisite_tool_ordering")
def prerequisite_tool_ordering(ctx: PolicyContext) -> bool:
    """Deny a destructive action that was not preceded by a read of its target.

    Deleting something nobody looked at first is the shape of a mistake rather
    than a decision.
    """
    pending = ctx.requested_tool
    if pending is None or not ctx.tool_has(pending, "destructive"):
        return True
    return any(not ctx.tool_has(t, "destructive") for t in ctx.tool_call_history)
