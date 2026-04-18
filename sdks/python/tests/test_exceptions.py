"""Day 5 — exception mapping tests.

Exercises every path through ``_raise_for_stop`` directly and verifies
the full public exception hierarchy.
"""

import pytest

from nanny_sdk import (
    AgentCompleted,
    AgentNotFound,
    BudgetExhausted,
    MaxStepsReached,
    RuleDenied,
    TimeoutExpired,
    ToolDenied,
)
from nanny_sdk._client import _raise_for_stop
from nanny_sdk.exceptions import NannyStop

# ---------------------------------------------------------------------------
# _raise_for_stop — every reason string
# ---------------------------------------------------------------------------


def test_max_steps_reached() -> None:
    with pytest.raises(MaxStepsReached):
        _raise_for_stop("MaxStepsReached")


def test_budget_exhausted() -> None:
    with pytest.raises(BudgetExhausted):
        _raise_for_stop("BudgetExhausted")


def test_timeout_expired() -> None:
    with pytest.raises(TimeoutExpired):
        _raise_for_stop("TimeoutExpired")


def test_agent_completed() -> None:
    with pytest.raises(AgentCompleted):
        _raise_for_stop("AgentCompleted")


def test_agent_not_found() -> None:
    with pytest.raises(AgentNotFound):
        _raise_for_stop("AgentNotFound")


def test_tool_denied_carries_tool_name() -> None:
    with pytest.raises(ToolDenied) as exc_info:
        _raise_for_stop("ToolDenied", tool_name="write_file")
    assert exc_info.value.tool_name == "write_file"


def test_rule_denied_carries_rule_name() -> None:
    with pytest.raises(RuleDenied) as exc_info:
        _raise_for_stop("RuleDenied", rule_name="no_spiral")
    assert exc_info.value.rule_name == "no_spiral"


def test_unknown_reason_raises_runtime_error() -> None:
    with pytest.raises(RuntimeError, match="unknown stop reason"):
        _raise_for_stop("SomethingInvented")


# ---------------------------------------------------------------------------
# Importable directly from nanny_sdk
# ---------------------------------------------------------------------------


def test_all_exceptions_importable() -> None:
    """All stop exceptions are importable from the top-level package."""
    from nanny_sdk import (  # noqa: F401
        AgentCompleted,
        AgentNotFound,
        BudgetExhausted,
        MaxStepsReached,
        RuleDenied,
        TimeoutExpired,
        ToolDenied,
    )


# ---------------------------------------------------------------------------
# Inheritance — all are NannyStop subclasses
# ---------------------------------------------------------------------------


def test_all_are_nanny_stop_subclasses() -> None:
    assert issubclass(MaxStepsReached, NannyStop)
    assert issubclass(BudgetExhausted, NannyStop)
    assert issubclass(TimeoutExpired, NannyStop)
    assert issubclass(AgentCompleted, NannyStop)
    assert issubclass(AgentNotFound, NannyStop)
    assert issubclass(ToolDenied, NannyStop)
    assert issubclass(RuleDenied, NannyStop)


def test_all_are_base_exceptions() -> None:
    """NannyStop extends BaseException (not Exception) so it propagates through
    broad ``except Exception`` handlers in agent frameworks without being swallowed.
    """
    assert issubclass(NannyStop, BaseException)
    assert not issubclass(NannyStop, Exception)


# ---------------------------------------------------------------------------
# Detail attributes on ToolDenied and RuleDenied
# ---------------------------------------------------------------------------


def test_tool_denied_str_contains_name() -> None:
    exc = ToolDenied("delete_db")
    assert "delete_db" in str(exc)


def test_rule_denied_str_contains_name() -> None:
    exc = RuleDenied("no_loop")
    assert "no_loop" in str(exc)
