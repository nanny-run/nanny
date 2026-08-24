"""Day 5 — exception mapping tests.

Exercises every path through ``_raise_for_stop`` directly and verifies
the full public exception hierarchy.
"""

import pytest

from nanny_sdk import (
    AgentCompleted,
    RuleDenied,
    ToolDenied,
)
from nanny_sdk._client import _raise_for_stop
from nanny_sdk.exceptions import NannyStop

# ---------------------------------------------------------------------------
# _raise_for_stop — every reason string
# ---------------------------------------------------------------------------


def test_agent_completed() -> None:
    with pytest.raises(AgentCompleted):
        _raise_for_stop("AgentCompleted")


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
        RuleDenied,
        ToolDenied,
    )


# ---------------------------------------------------------------------------
# Inheritance — all are NannyStop subclasses
# ---------------------------------------------------------------------------


def test_all_are_nanny_stop_subclasses() -> None:
    assert issubclass(AgentCompleted, NannyStop)
    assert issubclass(ToolDenied, NannyStop)
    assert issubclass(RuleDenied, NannyStop)


def test_removed_stop_reasons_are_not_importable() -> None:
    """The three consumption stops are gone, not deprecated.

    A name that still imports but never fires is worse than one that does not
    import: an integrator's ``except BudgetExhausted`` would look like a live
    control and silently never catch anything.
    """
    import nanny_sdk

    for name in ("MaxStepsReached", "BudgetExhausted", "TimeoutExpired", "AgentNotFound"):
        assert not hasattr(nanny_sdk, name), f"{name} must not be importable"


def test_a_removed_stop_reason_from_the_wire_is_a_hard_error() -> None:
    """An old governor sending a deleted reason must fail loudly.

    Falling through to a generic stop would hide a genuine version mismatch.
    """
    with pytest.raises(RuntimeError, match="unknown stop reason"):
        _raise_for_stop("BudgetExhausted")


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
