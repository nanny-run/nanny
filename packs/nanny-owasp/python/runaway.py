"""Bounded outward action, for LLM08 (excessive agency)."""

from nanny_sdk import rule
from nanny_sdk._context import PolicyContext


@rule("cap_external_effect_calls_per_run")
def cap_external_effect_calls_per_run(ctx: PolicyContext) -> bool:
    """Bound how many outward actions one run may take."""
    limit = 10
    used = sum(
        count
        for tool, count in ctx.tool_call_counts.items()
        if ctx.tool_has(tool, "external_effect")
    )
    return used < limit
