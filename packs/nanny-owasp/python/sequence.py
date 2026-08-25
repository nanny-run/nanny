"""Delegation depth, for LLM08 (excessive agency)."""

from nanny_sdk import rule
from nanny_sdk._context import PolicyContext


@rule("no_agent_spawn_beyond_depth")
def no_agent_spawn_beyond_depth(ctx: PolicyContext) -> bool:
    """Bound how many sub-agents a run may create.

    Delegation multiplies authority: every spawned agent inherits the ability to
    act, so a run that has spawned many has stopped being one accountable actor.
    """
    max_depth = 3
    spawns = sum(
        count for tool, count in ctx.tool_call_counts.items() if tool.startswith("spawn_")
    )
    return spawns < max_depth
