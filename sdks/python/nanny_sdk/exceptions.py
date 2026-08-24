"""Nanny stop-reason exceptions.

Each variant of the Rust ``StopReason`` enum maps to a typed Python exception.
Names match exactly, no prefix, no divergence.

    from nanny_sdk import RuleDenied, ToolDenied
"""


class NannyStop(BaseException):
    """Base class for all Nanny stop signals.

    Extends BaseException (not Exception) so stop signals propagate through
    broad ``except Exception`` handlers in agent frameworks (CrewAI, LangChain
    AgentExecutor, etc.) without being silently swallowed.
    """


class AgentCompleted(NannyStop):
    """The agent finished normally (used as a signal, not an error)."""


class ToolDenied(NannyStop):
    """A tool call was denied by the allowlist or a rule."""

    def __init__(self, tool_name: str) -> None:
        self.tool_name = tool_name
        super().__init__(f"tool denied: {tool_name!r}")


class RuleDenied(NannyStop):
    """A policy rule returned False and blocked the tool call."""

    def __init__(self, rule_name: str) -> None:
        self.rule_name = rule_name
        super().__init__(f"rule denied: {rule_name!r}")


class BridgeUnavailable(NannyStop):
    """The bridge was active but unreachable during rule evaluation or a tool call.

    Extends NannyStop (BaseException) so it propagates through broad
    ``except Exception`` handlers in agent frameworks, the same reason all
    stop signals use BaseException. Silently swallowing a bridge failure would
    let the agent continue ungoverned, violating the manifesto guarantee.
    """


class ExecutionStopped(NannyStop):
    """The run this call belongs to has already stopped (G3/G7).

    Raised when an action endpoint answers 410 Gone: the run was stopped on an
    earlier call, possibly by another process sharing the same ``NANNY_RUN_ID``.
    The governance server keys enforcement state by run id, so the stop is final
    for this run only, not the whole server. ``reason`` carries the stop reason
    the bridge reported. ``AgentCompleted`` is raised as its own class;
    everything else surfaces as this generic stop.
    """

    def __init__(self, reason: str = "execution stopped") -> None:
        self.reason = reason
        super().__init__(reason)
