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
