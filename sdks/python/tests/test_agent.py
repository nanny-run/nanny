"""Day 4 — @agent decorator tests."""

import pytest
from pytest_httpserver import HTTPServer

from nanny_sdk import AgentNotFound, agent

# ---------------------------------------------------------------------------
# Helpers
# ---------------------------------------------------------------------------


def _enter_ok() -> dict[str, object]:
    return {"status": "ok", "limits": {"steps": 10, "cost": 100, "timeout": 5000}}


def _exit_ok() -> dict[str, str]:
    return {"status": "ok"}


# ---------------------------------------------------------------------------
# Sync — happy path
# ---------------------------------------------------------------------------


def test_agent_enter_and_exit_both_called(mock_bridge: HTTPServer) -> None:
    """Both /agent/enter and /agent/exit are called for a normal execution."""
    mock_bridge.expect_request("/agent/enter", method="POST").respond_with_json(_enter_ok())
    mock_bridge.expect_request("/agent/exit", method="POST").respond_with_json(_exit_ok())

    @agent("researcher")
    def my_func() -> str:
        return "done"

    assert my_func() == "done"
    mock_bridge.check_assertions()


def test_agent_result_preserved(mock_bridge: HTTPServer) -> None:
    """The decorated function's return value passes through unchanged."""
    mock_bridge.expect_request("/agent/enter").respond_with_json(_enter_ok())
    mock_bridge.expect_request("/agent/exit").respond_with_json(_exit_ok())

    @agent("researcher")
    def my_func() -> int:
        return 42

    assert my_func() == 42


# ---------------------------------------------------------------------------
# Sync — exit always fires
# ---------------------------------------------------------------------------


def test_agent_exit_called_on_exception(mock_bridge: HTTPServer) -> None:
    """exit is called even when the wrapped function raises."""
    mock_bridge.expect_request("/agent/enter", method="POST").respond_with_json(_enter_ok())
    mock_bridge.expect_request("/agent/exit", method="POST").respond_with_json(_exit_ok())

    @agent("researcher")
    def my_func() -> str:
        raise ValueError("boom")

    with pytest.raises(ValueError, match="boom"):
        my_func()
    mock_bridge.check_assertions()


# ---------------------------------------------------------------------------
# Sync — scope not found
# ---------------------------------------------------------------------------


def test_agent_not_found_raises(mock_bridge: HTTPServer) -> None:
    """404 from /agent/enter raises AgentNotFound."""
    mock_bridge.expect_request("/agent/enter", method="POST").respond_with_data(
        '{"error":"named limits set \'researcher\' not found"}',
        status=404,
        content_type="application/json",
    )

    @agent("researcher")
    def my_func() -> str:
        return "done"

    with pytest.raises(AgentNotFound):
        my_func()
    mock_bridge.check_assertions()


def test_agent_not_found_body_never_runs(mock_bridge: HTTPServer) -> None:
    """When AgentNotFound is raised, the function body must not execute."""
    executed = False
    mock_bridge.expect_request("/agent/enter", method="POST").respond_with_data(
        '{"error":"named limits set \'x\' not found"}',
        status=404,
        content_type="application/json",
    )

    @agent("x")
    def my_func() -> str:
        nonlocal executed
        executed = True
        return "done"

    with pytest.raises(AgentNotFound):
        my_func()
    assert not executed


# ---------------------------------------------------------------------------
# Sync — passthrough
# ---------------------------------------------------------------------------


def test_agent_passthrough_runs_directly(monkeypatch: pytest.MonkeyPatch) -> None:
    """In passthrough mode, no network calls; function runs directly."""
    monkeypatch.delenv("NANNY_BRIDGE_PORT", raising=False)

    @agent("researcher")
    def my_func() -> str:
        return "direct"

    assert my_func() == "direct"


# ---------------------------------------------------------------------------
# Async — happy path
# ---------------------------------------------------------------------------


async def test_agent_async_enter_and_exit_called(mock_bridge: HTTPServer) -> None:
    """Async: both /agent/enter and /agent/exit are called."""
    mock_bridge.expect_request("/agent/enter", method="POST").respond_with_json(_enter_ok())
    mock_bridge.expect_request("/agent/exit", method="POST").respond_with_json(_exit_ok())

    @agent("researcher")
    async def my_async_func() -> str:
        return "async done"

    assert await my_async_func() == "async done"
    mock_bridge.check_assertions()


async def test_agent_async_result_preserved(mock_bridge: HTTPServer) -> None:
    """Async: return value passes through unchanged."""
    mock_bridge.expect_request("/agent/enter").respond_with_json(_enter_ok())
    mock_bridge.expect_request("/agent/exit").respond_with_json(_exit_ok())

    @agent("researcher")
    async def my_async_func() -> int:
        return 99

    assert await my_async_func() == 99


# ---------------------------------------------------------------------------
# Async — exit always fires
# ---------------------------------------------------------------------------


async def test_agent_async_exit_called_on_exception(mock_bridge: HTTPServer) -> None:
    """Async: exit is called even when the wrapped function raises."""
    mock_bridge.expect_request("/agent/enter", method="POST").respond_with_json(_enter_ok())
    mock_bridge.expect_request("/agent/exit", method="POST").respond_with_json(_exit_ok())

    @agent("researcher")
    async def my_async_func() -> str:
        raise RuntimeError("async boom")

    with pytest.raises(RuntimeError, match="async boom"):
        await my_async_func()
    mock_bridge.check_assertions()


# ---------------------------------------------------------------------------
# Async — passthrough
# ---------------------------------------------------------------------------


async def test_agent_async_passthrough(monkeypatch: pytest.MonkeyPatch) -> None:
    """Async passthrough: no network calls, function runs directly."""
    monkeypatch.delenv("NANNY_BRIDGE_PORT", raising=False)

    @agent("researcher")
    async def my_async_func() -> str:
        return "async direct"

    assert await my_async_func() == "async direct"
