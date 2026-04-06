"""Day 1 — connectivity smoke tests for _client."""

import pytest
from pytest_httpserver import HTTPServer

import nanny_sdk._client as client


def test_passthrough_when_no_env(monkeypatch: pytest.MonkeyPatch) -> None:
    """is_passthrough() returns True when NANNY_BRIDGE_PORT is not set."""
    monkeypatch.delenv("NANNY_BRIDGE_PORT", raising=False)
    assert client.is_passthrough() is True


def test_not_passthrough_when_env_set(mock_bridge: HTTPServer) -> None:
    """is_passthrough() returns False when NANNY_BRIDGE_PORT is set."""
    assert client.is_passthrough() is False


def test_health_ok(mock_bridge: HTTPServer) -> None:
    """health() returns True when bridge responds with status ok."""
    mock_bridge.expect_request("/health").respond_with_json({"status": "ok"})
    assert client.health() is True


def test_health_not_ok(mock_bridge: HTTPServer) -> None:
    """health() returns False when bridge responds with unexpected status."""
    mock_bridge.expect_request("/health").respond_with_json({"status": "degraded"})
    assert client.health() is False
