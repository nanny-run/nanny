"""Injection and taint: stop untrusted content from reaching a privileged action.

Indirect prompt injection is the dominant agent failure mode, and it does not
arrive as a suspicious tool call. It arrives as ordinary content in the response
of a tool the operator allowed, and then the agent, acting in good faith, does
something with it. Nothing here reads that content. These rules govern the
*ordering of authority*: once untrusted material has entered a run, the actions
that can reach the outside world are no longer available to it.

That is why the rules reference labels rather than tool names. `web_search` and
`fetch_ticket` and `read_inbox` are different names for the same hazard, and a
rule naming any of them governs one application. A rule reading
`reads_untrusted` governs every application whose operator labelled their tools.
"""

from nanny_sdk import rule
from nanny_sdk._context import PolicyContext


@rule("no_external_effect_after_untrusted_read")
def no_external_effect_after_untrusted_read(ctx: PolicyContext) -> bool:
    """Deny an outward action once untrusted content has entered the run.

    The core taint rule. Reading a web page is safe; sending an email is safe;
    sending an email *after* reading a web page is the exfiltration path, and it
    is the ordering that makes it one.
    """
    pending = ctx.requested_tool
    if pending is None or not ctx.tool_has(pending, "external_effect"):
        return True
    return not any(ctx.tool_has(t, "reads_untrusted") for t in ctx.tool_call_history)


@rule("no_untrusted_read_after_secrets")
def no_untrusted_read_after_secrets(ctx: PolicyContext) -> bool:
    """Deny reading untrusted content once secrets are in play.

    The mirror of the rule above, and the half people forget. Taint flows both
    ways: pulling attacker-controlled instructions into a context that already
    holds credentials is how the credentials get used.
    """
    pending = ctx.requested_tool
    if pending is None or not ctx.tool_has(pending, "reads_untrusted"):
        return True
    return not any(ctx.tool_has(t, "reads_sensitive") for t in ctx.tool_call_history)
