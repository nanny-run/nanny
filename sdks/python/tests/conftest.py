"""Shared pytest fixtures.

``mock_bridge`` — spins up a fake Nanny bridge via pytest-httpserver and
sets ``NANNY_BRIDGE_PORT`` so _client routes to it. Because _client reads
env vars lazily (at call time), monkeypatch works without reloading modules.

``_reset_rules`` — clears the global rule registry before and after every
test so rules registered in one test don't bleed into the next.
"""

from collections.abc import Generator

import pytest
from pytest_httpserver import HTTPServer


@pytest.fixture(autouse=True)
def _reset_rules() -> Generator[None, None, None]:
    """Clear _RULES before and after every test."""
    from nanny_sdk._decorators import _RULES

    _RULES.clear()
    yield
    _RULES.clear()


@pytest.fixture()
def mock_bridge(httpserver: HTTPServer, monkeypatch: pytest.MonkeyPatch) -> HTTPServer:
    """Fake Nanny bridge for unit tests.

    Sets ``NANNY_BRIDGE_PORT`` and ``NANNY_SESSION_TOKEN`` for the duration
    of the test, then restores the original environment on teardown.

    Returns the ``HTTPServer`` so tests can register expected requests::

        def test_something(mock_bridge):
            mock_bridge.expect_request("/health").respond_with_json({"status": "ok"})
            assert client.health() is True
    """
    monkeypatch.setenv("NANNY_BRIDGE_PORT", str(httpserver.port))
    monkeypatch.setenv("NANNY_SESSION_TOKEN", "test-token")
    # Permanent catch-all for POST /stop — report_stop() calls this on every denial.
    # Using expect_request (not expect_oneshot_request) so it handles any number of calls
    # and is NOT checked by check_assertions(), avoiding noise in allow-path tests.
    httpserver.expect_request("/stop", method="POST").respond_with_json({"status": "ok"})
    # Permanent catch-all for GET /status — the @tool decorator fetches live counters
    # before running rules. Tests that need specific counter values register a oneshot
    # handler before calling the tool (oneshot handlers take priority over persistent ones).
    httpserver.expect_request("/status", method="GET").respond_with_json({
        "state": "running",
        "step": 0,
        "cost_spent": 0,
        "elapsed_ms": 0,
        "tool_call_counts": {},
        "tool_call_history": [],
    })
    return httpserver
