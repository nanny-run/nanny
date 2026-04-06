"""Nanny SDK — execution boundary for AI agents.

    from nanny_sdk import tool, rule, agent
    from nanny_sdk import BudgetExhausted, RuleDenied

Run your agent under ``nanny run agent.py``. All decorators are no-ops when
``NANNY_BRIDGE_PORT`` is absent — zero friction in direct development.
"""

from nanny_sdk._decorators import agent, rule, tool
from nanny_sdk.exceptions import (
    AgentCompleted,
    AgentNotFound,
    BudgetExhausted,
    MaxStepsReached,
    NannyStop,
    RuleDenied,
    TimeoutExpired,
    ToolDenied,
)

__all__ = [
    # Decorators
    "tool",
    "rule",
    "agent",
    # Exceptions
    "NannyStop",
    "MaxStepsReached",
    "BudgetExhausted",
    "TimeoutExpired",
    "AgentCompleted",
    "AgentNotFound",
    "ToolDenied",
    "RuleDenied",
]
