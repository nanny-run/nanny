"""Nanny SDK — execution boundary for AI agents.

    from nanny_sdk import tool, rule, agent
    from nanny_sdk import BudgetExhausted, RuleDenied

Run your agent under ``nanny run agent.py``. All decorators are no-ops when
``NANNY_BRIDGE_PORT`` is absent — zero friction in direct development.
"""

from nanny_sdk._decorators import agent, rule, tool
from nanny_sdk.app import set_app
from nanny_sdk.exceptions import (
    AgentCompleted,
    AgentNotFound,
    BridgeUnavailable,
    BudgetExhausted,
    ExecutionStopped,
    MaxStepsReached,
    NannyStop,
    RuleDenied,
    TimeoutExpired,
    ToolDenied,
)
from nanny_sdk.instrument import instrument
from nanny_sdk.run import fresh_run, run_scope

__all__ = [
    # Decorators
    "tool",
    "rule",
    "agent",
    # LLM instrumentation
    "instrument",
    # Run control
    "fresh_run",
    "run_scope",
    # App attribution
    "set_app",
    # Exceptions
    "NannyStop",
    "MaxStepsReached",
    "BudgetExhausted",
    "TimeoutExpired",
    "AgentCompleted",
    "AgentNotFound",
    "ToolDenied",
    "RuleDenied",
    "BridgeUnavailable",
    "ExecutionStopped",
]
