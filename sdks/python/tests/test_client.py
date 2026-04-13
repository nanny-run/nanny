"""Day 1 — connectivity smoke tests for _client."""

import pytest
from pytest_httpserver import HTTPServer

import nanny_sdk._client as client


def test_passthrough_when_no_env(monkeypatch: pytest.MonkeyPatch) -> None:
    """is_passthrough() returns True when neither socket nor port is set."""
    monkeypatch.delenv("NANNY_BRIDGE_SOCKET", raising=False)
    monkeypatch.delenv("NANNY_BRIDGE_PORT", raising=False)
    assert client.is_passthrough() is True


def test_not_passthrough_when_port_set(mock_bridge: HTTPServer) -> None:
    """is_passthrough() returns False when NANNY_BRIDGE_PORT is set (Windows path)."""
    assert client.is_passthrough() is False


def test_not_passthrough_when_socket_set(monkeypatch: pytest.MonkeyPatch) -> None:
    """is_passthrough() returns False when NANNY_BRIDGE_SOCKET is set (Unix path)."""
    monkeypatch.delenv("NANNY_BRIDGE_PORT", raising=False)
    monkeypatch.setenv("NANNY_BRIDGE_SOCKET", "/tmp/nanny-test.sock")
    assert client.is_passthrough() is False


def test_health_ok(mock_bridge: HTTPServer) -> None:
    """health() returns True when bridge responds with state running."""
    mock_bridge.expect_request("/health").respond_with_json({"state": "running"})
    assert client.health() is True


def test_health_not_ok(mock_bridge: HTTPServer) -> None:
    """health() returns False when bridge responds with any non-running state."""
    mock_bridge.expect_request("/health").respond_with_json(
        {"state": "stopped", "reason": "MaxStepsReached"}
    )
    assert client.health() is False
