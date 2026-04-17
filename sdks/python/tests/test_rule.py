"""Day 3 — @rule decorator tests."""

import pytest
from pytest_httpserver import HTTPServer

from nanny_sdk import RuleDenied, rule, tool
from nanny_sdk._context import PolicyContext

# ---------------------------------------------------------------------------
# Helpers
# ---------------------------------------------------------------------------


def _allow() -> dict[str, str]:
    return {"status": "allowed"}


# ---------------------------------------------------------------------------
# Allow path — rule passes, bridge proceeds
# ---------------------------------------------------------------------------


def test_rule_allow_bridge_called(mock_bridge: HTTPServer) -> None:
    """Rule returning True: bridge is still called and function executes."""
    mock_bridge.expect_request("/tool/call", method="POST").respond_with_json(_allow())

    @rule("allow_all")
    def allow_all(ctx: PolicyContext) -> bool:
        return True

    @tool(cost=10)
    def my_func() -> str:
        return "result"

    assert my_func() == "result"
    mock_bridge.check_assertions()


# ---------------------------------------------------------------------------
# Deny path — rule fires before bridge
# ---------------------------------------------------------------------------


def test_rule_deny_raises_rule_denied(mock_bridge: HTTPServer) -> None:
    """Rule returning False raises RuleDenied with the correct rule name."""

    @rule("no_everything")
    def no_everything(ctx: PolicyContext) -> bool:
        return False

    @tool(cost=10)
    def my_func() -> str:
        return "result"

    with pytest.raises(RuleDenied) as exc_info:
        my_func()
    assert exc_info.value.rule_name == "no_everything"


def test_rule_deny_bridge_never_called(mock_bridge: HTTPServer) -> None:
    """When a rule denies, the bridge is never contacted."""

    @rule("always_deny")
    def always_deny(ctx: PolicyContext) -> bool:
        return False

    @tool(cost=10)
    def my_func() -> str:
        return "result"

    with pytest.raises(RuleDenied):
        my_func()

    # mock_bridge has no registered handlers — any unexpected request raises
    mock_bridge.check_assertions()


def test_rule_deny_function_body_never_runs(mock_bridge: HTTPServer) -> None:
    """When a rule denies, the wrapped function body must not execute."""
    executed = False

    @rule("deny_rule")
    def deny_rule(ctx: PolicyContext) -> bool:
        return False

    @tool(cost=10)
    def my_func() -> str:
        nonlocal executed
        executed = True
        return "result"

    with pytest.raises(RuleDenied):
        my_func()
    assert not executed


# ---------------------------------------------------------------------------
# PolicyContext contents
# ---------------------------------------------------------------------------


def test_rule_ctx_last_tool_args(mock_bridge: HTTPServer) -> None:
    """ctx.last_tool_args contains the tool's call arguments."""
    captured: list[PolicyContext] = []
    mock_bridge.expect_request("/tool/call").respond_with_json(_allow())

    @rule("capture")
    def capture(ctx: PolicyContext) -> bool:
        captured.append(ctx)
        return True

    @tool(cost=10)
    def read_file(path: str) -> str:
        return ""

    read_file("src/main.rs")
    assert captured[0].last_tool_args == {"path": "src/main.rs"}


def test_rule_ctx_requested_tool(mock_bridge: HTTPServer) -> None:
    """ctx.requested_tool is set to the decorated function's name."""
    captured: list[PolicyContext] = []
    mock_bridge.expect_request("/tool/call").respond_with_json(_allow())

    @rule("capture")
    def capture(ctx: PolicyContext) -> bool:
        captured.append(ctx)
        return True

    @tool(cost=10)
    def search_web(query: str) -> str:
        return ""

    search_web("rust http clients")
    assert captured[0].requested_tool == "search_web"


# ---------------------------------------------------------------------------
# Multiple rules
# ---------------------------------------------------------------------------


def test_multiple_rules_all_evaluated_when_passing(mock_bridge: HTTPServer) -> None:
    """All registered rules are called when all return True."""
    call_log: list[str] = []
    mock_bridge.expect_request("/tool/call").respond_with_json(_allow())

    @rule("rule_a")
    def rule_a(ctx: PolicyContext) -> bool:
        call_log.append("a")
        return True

    @rule("rule_b")
    def rule_b(ctx: PolicyContext) -> bool:
        call_log.append("b")
        return True

    @tool(cost=10)
    def my_func() -> str:
        return "ok"

    my_func()
    assert set(call_log) == {"a", "b"}


def test_multiple_rules_first_deny_stops_evaluation(mock_bridge: HTTPServer) -> None:
    """Once a rule denies, remaining rules are not evaluated."""
    call_log: list[str] = []

    @rule("deny_first")
    def deny_first(ctx: PolicyContext) -> bool:
        call_log.append("first")
        return False

    @rule("should_not_run")
    def should_not_run(ctx: PolicyContext) -> bool:
        call_log.append("second")
        return True

    @tool(cost=10)
    def my_func() -> str:
        return "ok"

    with pytest.raises(RuleDenied) as exc_info:
        my_func()

    assert call_log == ["first"]
    assert exc_info.value.rule_name == "deny_first"


def test_rules_evaluated_in_registration_order(mock_bridge: HTTPServer) -> None:
    """Rules are evaluated in the order they were registered."""
    call_log: list[str] = []
    mock_bridge.expect_request("/tool/call").respond_with_json(_allow())

    @rule("first")
    def first(ctx: PolicyContext) -> bool:
        call_log.append("first")
        return True

    @rule("second")
    def second(ctx: PolicyContext) -> bool:
        call_log.append("second")
        return True

    @rule("third")
    def third(ctx: PolicyContext) -> bool:
        call_log.append("third")
        return True

    @tool(cost=10)
    def my_func() -> str:
        return "ok"

    my_func()
    assert call_log == ["first", "second", "third"]


# ---------------------------------------------------------------------------
# Passthrough — rules not evaluated
# ---------------------------------------------------------------------------


def test_passthrough_rules_not_evaluated(monkeypatch: pytest.MonkeyPatch) -> None:
    """In passthrough mode, rule functions are never called."""
    evaluated = False

    @rule("would_deny")
    def would_deny(ctx: PolicyContext) -> bool:
        nonlocal evaluated
        evaluated = True
        return False

    monkeypatch.delenv("NANNY_BRIDGE_PORT", raising=False)

    @tool(cost=10)
    def my_func() -> str:
        return "direct"

    assert my_func() == "direct"
    assert not evaluated
