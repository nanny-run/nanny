"""Nanny stop-reason exceptions.

Each variant of the Rust ``StopReason`` enum maps to a typed Python exception.
Names match exactly — no prefix, no divergence.

    from nanny_sdk import BudgetExhausted, ToolDenied
"""


class NannyStop(Exception):
    """Base class for all Nanny stop signals."""


class MaxStepsReached(NannyStop):
    """The step ceiling was reached before the agent completed."""


class BudgetExhausted(NannyStop):
    """The cost budget was exhausted before the agent completed."""


class TimeoutExpired(NannyStop):
    """The wall-clock timeout elapsed before the agent completed."""


class AgentCompleted(NannyStop):
    """The agent finished normally (used as a signal, not an error)."""


class AgentNotFound(NannyStop):
    """The named agent scope is not defined in nanny.toml."""


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
