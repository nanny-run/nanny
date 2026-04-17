"""Day 6 — Passthrough verification.

All tests verify that when neither NANNY_BRIDGE_SOCKET nor NANNY_BRIDGE_PORT is
set (i.e. running ``python agent.py`` directly, not under ``nanny run``):

- ``@tool`` returns the original function unchanged — no wrapper, no network calls.
- ``@agent`` returns the original function unchanged — no wrapper, no network calls.
- ``@rule`` registers the function in _RULES as normal but is never *called*,
  because ``@tool`` in passthrough skips ``_check_rules`` entirely.
- All three decorators together on the same call path execute without error.
- Importing from ``nanny_sdk`` succeeds with no env vars set.

No ``mock_bridge`` fixture — no network activity is expected in any of these tests.
"""

from __future__ import annotations

import pytest

from nanny_sdk import (
    AgentCompleted,
    AgentNotFound,
    BudgetExhausted,
    MaxStepsReached,
    RuleDenied,
    TimeoutExpired,
    ToolDenied,
    agent,
    rule,
    tool,
)
from nanny_sdk._decorators import _RULES

# ---------------------------------------------------------------------------
# Module-wide fixture — both bridge env vars absent for every test here
# ---------------------------------------------------------------------------


@pytest.fixture(autouse=True)
def _no_bridge(monkeypatch: pytest.MonkeyPatch) -> None:
    """Unset both bridge env vars so every test runs in passthrough mode."""
    monkeypatch.delenv("NANNY_BRIDGE_SOCKET", raising=False)
    monkeypatch.delenv("NANNY_BRIDGE_PORT", raising=False)


# ---------------------------------------------------------------------------
# Import — zero errors with no env vars
# ---------------------------------------------------------------------------


def test_all_public_symbols_importable_without_env_vars() -> None:
    """Importing tool/rule/agent and all exceptions succeeds with no bridge set."""
    assert callable(tool)
    assert callable(rule)
    assert callable(agent)
    stop_exceptions = (
        BudgetExhausted,
        MaxStepsReached,
        TimeoutExpired,
        ToolDenied,
        RuleDenied,
        AgentCompleted,
        AgentNotFound,
    )
    for exc in stop_exceptions:
        assert issubclass(exc, Exception)


# ---------------------------------------------------------------------------
# @tool — sync passthrough
# ---------------------------------------------------------------------------


def test_tool_sync_passthrough_returns_original_function() -> None:
    """@tool in passthrough mode returns fn itself, not a wrapper."""

    def original(x: int) -> int:
        return x * 2

    decorated = tool(cost=10)(original)
    assert decorated is original


def test_tool_sync_passthrough_returns_value() -> None:
    @tool(cost=10)
    def fetch(url: str) -> str:
        return f"result: {url}"

    assert fetch("http://example.com") == "result: http://example.com"


def test_tool_sync_passthrough_no_exception() -> None:
    @tool(cost=99)
    def search(query: str) -> list[str]:
        return [query]

    assert search("nanny") == ["nanny"]


# ---------------------------------------------------------------------------
# @tool — async passthrough
# ---------------------------------------------------------------------------


async def test_tool_async_passthrough_returns_original_function() -> None:
    """@tool in passthrough mode returns the async fn itself, not a wrapper."""

    async def original(x: int) -> int:
        return x * 2

    decorated = tool(cost=10)(original)
    assert decorated is original


async def test_tool_async_passthrough_returns_value() -> None:
    @tool(cost=10)
    async def fetch(url: str) -> str:
        return f"async: {url}"

    assert await fetch("http://example.com") == "async: http://example.com"


# ---------------------------------------------------------------------------
# @rule — passthrough behaviour
# ---------------------------------------------------------------------------


def test_rule_always_registered_in_passthrough() -> None:
    """@rule registers the function in _RULES even in passthrough mode."""

    @rule("no_spiral")
    def check_spiral(ctx: object) -> bool:
        return True

    assert "no_spiral" in _RULES


def test_rule_never_called_in_passthrough() -> None:
    """In passthrough @tool skips _check_rules entirely — rule fn never invoked."""
    call_count = 0

    @rule("track_calls")
    def tracking_rule(ctx: object) -> bool:
        nonlocal call_count
        call_count += 1
        return True

    @tool(cost=10)
    def fetch(url: str) -> str:
        return "result"

    fetch("http://example.com")
    assert call_count == 0


# ---------------------------------------------------------------------------
# @agent — sync passthrough
# ---------------------------------------------------------------------------


def test_agent_sync_passthrough_returns_original_function() -> None:
    """@agent in passthrough mode returns fn itself, not a wrapper."""

    def original() -> str:
        return "direct"

    decorated = agent("researcher")(original)
    assert decorated is original


def test_agent_sync_passthrough_returns_value() -> None:
    @agent("support")
    def triage() -> str:
        return "triaged"

    assert triage() == "triaged"


def test_agent_sync_passthrough_no_exception() -> None:
    @agent("researcher")
    def run() -> int:
        return 42

    assert run() == 42


# ---------------------------------------------------------------------------
# @agent — async passthrough
# ---------------------------------------------------------------------------


async def test_agent_async_passthrough_returns_original_function() -> None:
    """@agent in passthrough mode returns the async fn itself, not a wrapper."""

    async def original() -> str:
        return "async direct"

    decorated = agent("researcher")(original)
    assert decorated is original


async def test_agent_async_passthrough_returns_value() -> None:
    @agent("support")
    async def triage() -> str:
        return "async triaged"

    assert await triage() == "async triaged"


# ---------------------------------------------------------------------------
# All three decorators together on the same call path
# ---------------------------------------------------------------------------


def test_all_decorators_together_sync() -> None:
    """@rule + @tool + @agent on the same sync call path — no error, correct value."""

    @rule("no_loop")
    def check_loop(ctx: object) -> bool:
        return True

    @tool(cost=10)
    def fetch(url: str) -> str:
        return f"result: {url}"

    @agent("researcher")
    def research() -> str:
        return fetch("http://example.com")

    assert research() == "result: http://example.com"


async def test_all_decorators_together_async() -> None:
    """@rule + @tool + @agent on the same async call path — no error, correct value."""

    @rule("no_loop")
    def check_loop(ctx: object) -> bool:
        return True

    @tool(cost=10)
    async def fetch(url: str) -> str:
        return f"async: {url}"

    @agent("researcher")
    async def research() -> str:
        return await fetch("http://example.com")

    assert await research() == "async: http://example.com"
