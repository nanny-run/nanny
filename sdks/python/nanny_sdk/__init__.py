"""Nanny SDK: authorization and audit for AI agents that take real-world actions.

    from nanny_sdk import tool, rule, agent
    from nanny_sdk import RuleDenied, ToolDenied

Run your agent under ``nanny run agent.py``. All decorators are no-ops when
``NANNY_BRIDGE_PORT`` is absent, zero friction in direct development.
"""

from nanny_sdk._decorators import agent, rule, tool
from nanny_sdk.app import set_app
from nanny_sdk.events import get_run_events
from nanny_sdk.exceptions import (
    AgentCompleted,
    BridgeUnavailable,
    ExecutionStopped,
    NannyStop,
    RuleDenied,
    ToolDenied,
)
from nanny_sdk.instrument import instrument
from nanny_sdk.packs import declare_all, load_installed_packs
from nanny_sdk.run import run_scope

__all__ = [
    "declare_all",
    "load_installed_packs",
    # Decorators
    "tool",
    "rule",
    "agent",
    # LLM instrumentation
    "instrument",
    # Run control
    "run_scope",
    # App attribution
    "set_app",
    # Usage events
    "get_run_events",
    # Exceptions
    "NannyStop",
    "AgentCompleted",
    "ToolDenied",
    "RuleDenied",
    "BridgeUnavailable",
    "ExecutionStopped",
]
