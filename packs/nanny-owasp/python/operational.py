"""The allowlist backstop, kept in the OWASP pack for LLM08 (excessive agency)."""

from nanny_sdk import rule
from nanny_sdk._context import PolicyContext


@rule("no_tool_outside_declared_allowlist")
def no_tool_outside_declared_allowlist(ctx: PolicyContext) -> bool:
    """Deny anything the operator never declared."""
    pending = ctx.requested_tool
    if pending is None:
        return True
    return pending in ctx.tool_labels
