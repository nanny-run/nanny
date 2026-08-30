"""Confirmation before irreversible action, for LLM08 (excessive agency)."""

from nanny_sdk import rule
from nanny_sdk._context import PolicyContext

CONFIRM_TOOL = "confirm_destructive"


@rule("no_destructive_without_confirmation")
def no_destructive_without_confirmation(ctx: PolicyContext) -> bool:
    """Deny an irreversible action that no confirmation step preceded."""
    pending = ctx.requested_tool
    if pending is None or not ctx.tool_has(pending, "destructive"):
        return True
    return CONFIRM_TOOL in ctx.tool_call_history
