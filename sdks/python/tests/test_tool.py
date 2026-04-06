"""Day 2 — @tool decorator tests."""

import pytest
from pytest_httpserver import HTTPServer

from nanny_sdk import BudgetExhausted, MaxStepsReached, RuleDenied, ToolDenied, tool

# ---------------------------------------------------------------------------
# Helpers
# ---------------------------------------------------------------------------


def _allow() -> dict[str, str]:
    return {"status": "allowed"}


def _deny(reason: str, detail: str = "") -> dict[str, str]:
    return {"status": "denied", "reason": reason, "detail": detail}


# ---------------------------------------------------------------------------
# Routing + result passthrough
# ---------------------------------------------------------------------------


def test_bridge_called_and_result_returned(mock_bridge: HTTPServer) -> None:
    """Bridge is contacted before the function; function return value is preserved."""
    call_log: list[str] = []
    mock_bridge.expect_request("/tool/call", method="POST").respond_with_json(_allow())

    @tool(cost=10)
    def my_func() -> str:
        call_log.append("ran")
        return "result"

    assert my_func() == "result"
    assert call_log == ["ran"]


def test_payload_tool_name_and_cost(mock_bridge: HTTPServer) -> None:
    """POST body includes the function name as 'tool' and the declared cost."""
    mock_bridge.expect_request(
        "/tool/call", method="POST", json={"tool": "fetch", "cost": 25, "args": {}}
    ).respond_with_json(_allow())

    @tool(cost=25)
    def fetch() -> str:
        return "ok"

    fetch()
    mock_bridge.check_assertions()


def test_payload_args_stringified(mock_bridge: HTTPServer) -> None:
    """Function arguments are sent as string values keyed by parameter name."""
    mock_bridge.expect_request(
        "/tool/call",
        method="POST",
        json={"tool": "read_file", "cost": 10, "args": {"path": "src/main.rs"}},
    ).respond_with_json(_allow())

    @tool(cost=10)
    def read_file(path: str) -> str:
        return ""

    read_file("src/main.rs")
    mock_bridge.check_assertions()


def test_multiple_args_all_sent(mock_bridge: HTTPServer) -> None:
    """All positional arguments are included in the payload."""
    mock_bridge.expect_request(
        "/tool/call",
        method="POST",
        json={"tool": "write_file", "cost": 5, "args": {"path": "out.txt", "content": "hello"}},
    ).respond_with_json(_allow())

    @tool(cost=5)
    def write_file(path: str, content: str) -> None:
        pass

    write_file("out.txt", "hello")
    mock_bridge.check_assertions()


# ---------------------------------------------------------------------------
# Deny → exception mapping
# ---------------------------------------------------------------------------


def test_deny_budget_exhausted(mock_bridge: HTTPServer) -> None:
    mock_bridge.expect_request("/tool/call").respond_with_json(_deny("BudgetExhausted"))

    @tool(cost=10)
    def my_func() -> str:
        return "result"

    with pytest.raises(BudgetExhausted):
        my_func()


def test_deny_max_steps_reached(mock_bridge: HTTPServer) -> None:
    mock_bridge.expect_request("/tool/call").respond_with_json(_deny("MaxStepsReached"))

    @tool(cost=10)
    def my_func() -> str:
        return "result"

    with pytest.raises(MaxStepsReached):
        my_func()


def test_deny_tool_denied_carries_name(mock_bridge: HTTPServer) -> None:
    mock_bridge.expect_request("/tool/call").respond_with_json(_deny("ToolDenied", "write_file"))

    @tool(cost=10)
    def my_func() -> str:
        return "result"

    with pytest.raises(ToolDenied) as exc_info:
        my_func()
    assert exc_info.value.tool_name == "write_file"


def test_deny_rule_denied_carries_name(mock_bridge: HTTPServer) -> None:
    mock_bridge.expect_request("/tool/call").respond_with_json(_deny("RuleDenied", "no_spiral"))

    @tool(cost=10)
    def my_func() -> str:
        return "result"

    with pytest.raises(RuleDenied) as exc_info:
        my_func()
    assert exc_info.value.rule_name == "no_spiral"


def test_function_body_never_runs_on_deny(mock_bridge: HTTPServer) -> None:
    """When the bridge denies, the wrapped function body must not execute."""
    executed = False
    mock_bridge.expect_request("/tool/call").respond_with_json(_deny("BudgetExhausted"))

    @tool(cost=10)
    def my_func() -> str:
        nonlocal executed
        executed = True
        return "result"

    with pytest.raises(BudgetExhausted):
        my_func()
    assert not executed


# ---------------------------------------------------------------------------
# Passthrough — no bridge
# ---------------------------------------------------------------------------


def test_passthrough_calls_function_directly(monkeypatch: pytest.MonkeyPatch) -> None:
    """Without NANNY_BRIDGE_PORT the function runs directly, no network calls."""
    monkeypatch.delenv("NANNY_BRIDGE_PORT", raising=False)

    @tool(cost=10)
    def my_func() -> str:
        return "direct"

    assert my_func() == "direct"


# ---------------------------------------------------------------------------
# Async functions
# ---------------------------------------------------------------------------


async def test_async_allowed(mock_bridge: HTTPServer) -> None:
    mock_bridge.expect_request("/tool/call").respond_with_json(_allow())

    @tool(cost=10)
    async def my_async_func() -> str:
        return "async result"

    assert await my_async_func() == "async result"


async def test_async_denied_raises(mock_bridge: HTTPServer) -> None:
    mock_bridge.expect_request("/tool/call").respond_with_json(_deny("BudgetExhausted"))

    @tool(cost=10)
    async def my_async_func() -> str:
        return "result"

    with pytest.raises(BudgetExhausted):
        await my_async_func()


async def test_async_body_not_called_on_deny(mock_bridge: HTTPServer) -> None:
    executed = False
    mock_bridge.expect_request("/tool/call").respond_with_json(_deny("MaxStepsReached"))

    @tool(cost=10)
    async def my_async_func() -> str:
        nonlocal executed
        executed = True
        return "result"

    with pytest.raises(MaxStepsReached):
        await my_async_func()
    assert not executed
