"""Day 1, connectivity smoke tests for _client."""

import pytest
from pytest_httpserver import HTTPServer

import nanny_sdk._client as client
from nanny_sdk.exceptions import AgentCompleted, BridgeUnavailable, ExecutionStopped


def test_passthrough_when_no_env(monkeypatch: pytest.MonkeyPatch) -> None:
    """is_passthrough() returns True when no transport env vars are set."""
    monkeypatch.delenv("NANNY_BRIDGE_SOCKET", raising=False)
    monkeypatch.delenv("NANNY_BRIDGE_PORT", raising=False)
    monkeypatch.delenv("NANNY_BRIDGE_ADDR", raising=False)
    assert client.is_passthrough() is True


def test_not_passthrough_when_addr_set(monkeypatch: pytest.MonkeyPatch) -> None:
    """is_passthrough() returns False when NANNY_BRIDGE_ADDR is set (network path)."""
    monkeypatch.delenv("NANNY_BRIDGE_SOCKET", raising=False)
    monkeypatch.delenv("NANNY_BRIDGE_PORT", raising=False)
    monkeypatch.setenv("NANNY_BRIDGE_ADDR", "10.0.0.1:62669")
    assert client.is_passthrough() is False


def test_bridge_addr_returns_none_when_unset(monkeypatch: pytest.MonkeyPatch) -> None:
    """_bridge_addr() returns None when NANNY_BRIDGE_ADDR is not set."""
    monkeypatch.delenv("NANNY_BRIDGE_ADDR", raising=False)
    assert client._bridge_addr() is None


def test_bridge_addr_returns_value_when_set(monkeypatch: pytest.MonkeyPatch) -> None:
    """_bridge_addr() returns the env var value when set."""
    monkeypatch.setenv("NANNY_BRIDGE_ADDR", "server.example.com:62669")
    assert client._bridge_addr() == "server.example.com:62669"


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
        {"state": "stopped", "reason": "ToolDenied"}
    )
    assert client.health() is False


def test_report_stop_rule_posts_correct_payload(mock_bridge: HTTPServer) -> None:
    """report_stop_rule posts reason, tool, and rule_name to /stop."""
    mock_bridge.expect_oneshot_request(
        "/stop",
        method="POST",
        json={"reason": "RuleDenied", "tool": "read_file", "rule_name": "no_sensitive_files"},
    ).respond_with_json({"status": "ok"})

    client.report_stop_rule("read_file", "no_sensitive_files")

    mock_bridge.check_assertions()


def test_report_stop_rule_ignores_bridge_errors(monkeypatch: pytest.MonkeyPatch) -> None:
    """report_stop_rule is fire-and-forget, bridge unreachable must not raise."""
    monkeypatch.setenv("NANNY_BRIDGE_PORT", "19999")  # nothing listening here
    monkeypatch.setenv("NANNY_SESSION_TOKEN", "test-token")

    # Must not raise even though no bridge is running
    client.report_stop_rule("read_file", "no_sensitive_files")


# ---------------------------------------------------------------------------
# An unreachable bridge fails closed as BridgeUnavailable, never a raw httpx
# traceback: the identical guarantee @rule's GET /status check already had,
# now applied consistently to every bridge call.
# ---------------------------------------------------------------------------


def test_agent_enter_raises_bridge_unavailable_when_unreachable(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    monkeypatch.setenv("NANNY_BRIDGE_PORT", "19999")  # nothing listening here
    monkeypatch.setenv("NANNY_SESSION_TOKEN", "test-token")
    with pytest.raises(BridgeUnavailable):
        client.agent_enter("researcher")


def test_call_tool_raises_bridge_unavailable_when_unreachable(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    monkeypatch.setenv("NANNY_BRIDGE_PORT", "19999")
    monkeypatch.setenv("NANNY_SESSION_TOKEN", "test-token")
    with pytest.raises(BridgeUnavailable):
        client.call_tool("search", {})


def test_health_raises_bridge_unavailable_when_unreachable(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    monkeypatch.setenv("NANNY_BRIDGE_PORT", "19999")
    monkeypatch.setenv("NANNY_SESSION_TOKEN", "test-token")
    with pytest.raises(BridgeUnavailable):
        client.health()


def test_get_status_raises_bridge_unavailable_when_unreachable(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    monkeypatch.setenv("NANNY_BRIDGE_PORT", "19999")
    monkeypatch.setenv("NANNY_SESSION_TOKEN", "test-token")
    with pytest.raises(BridgeUnavailable):
        client.get_status()


# ---------------------------------------------------------------------------
# A 410 (this run already stopped) becomes a typed stop, not a raw HTTP error
# ---------------------------------------------------------------------------


def test_call_tool_410_raises_typed_stop(mock_bridge: HTTPServer) -> None:
    """A 410 whose reason has its own class raises that class."""
    mock_bridge.expect_request("/tool/call", method="POST").respond_with_json(
        {"error": "execution stopped", "reason": "AgentCompleted"}, status=410
    )
    with pytest.raises(AgentCompleted):
        client.call_tool("search", {})


def test_call_tool_410_generic_reason_raises_execution_stopped(mock_bridge: HTTPServer) -> None:
    """A 410 with a non-limit reason raises ExecutionStopped carrying the reason."""
    mock_bridge.expect_request("/tool/call", method="POST").respond_with_json(
        {"error": "execution stopped", "reason": "ManualStop"}, status=410
    )
    with pytest.raises(ExecutionStopped) as excinfo:
        client.call_tool("search", {})
    assert excinfo.value.reason == "ManualStop"


def test_agent_enter_410_raises_typed_stop(mock_bridge: HTTPServer) -> None:
    """agent_enter on a stopped run raises a typed stop, not an HTTPStatusError."""
    mock_bridge.expect_request("/agent/enter", method="POST").respond_with_json(
        {"error": "execution stopped", "reason": "AgentCompleted"}, status=410
    )
    with pytest.raises(AgentCompleted):
        client.agent_enter("researcher")


def test_call_tool_410_from_earlier_denial_raises_execution_stopped(
    mock_bridge: HTTPServer,
) -> None:
    """A run already stopped by a denial reads 'ToolDenied' as the 410 reason.

    ToolDenied does not get its own class here: that class carries a tool_name,
    and a denial that happened on an earlier, possibly out-of-process call does
    not carry one. It falls through to ExecutionStopped, same as any other
    reason without its own class.
    """
    mock_bridge.expect_request("/tool/call", method="POST").respond_with_json(
        {"error": "execution stopped", "reason": "ToolDenied"}, status=410
    )
    with pytest.raises(ExecutionStopped) as excinfo:
        client.call_tool("search", {})
    assert excinfo.value.reason == "ToolDenied"


# ---------------------------------------------------------------------------
# The run id (NANNY_RUN_ID) travels on every request as X-Nanny-Run-Id
# ---------------------------------------------------------------------------


def test_headers_include_run_id_when_set(monkeypatch: pytest.MonkeyPatch) -> None:
    monkeypatch.setenv("NANNY_SESSION_TOKEN", "tok")
    monkeypatch.setenv("NANNY_RUN_ID", "team-alpha")
    h = client._headers()
    assert h["X-Nanny-Session-Token"] == "tok"
    assert h["X-Nanny-Run-Id"] == "team-alpha"


def test_headers_omit_run_id_when_unset(monkeypatch: pytest.MonkeyPatch) -> None:
    monkeypatch.setenv("NANNY_SESSION_TOKEN", "tok")
    monkeypatch.delenv("NANNY_RUN_ID", raising=False)
    assert "X-Nanny-Run-Id" not in client._headers()


def test_run_id_header_sent_on_tool_call(
    mock_bridge: HTTPServer, monkeypatch: pytest.MonkeyPatch
) -> None:
    """End to end: NANNY_RUN_ID is sent as X-Nanny-Run-Id on a real request."""
    monkeypatch.setenv("NANNY_RUN_ID", "team-alpha")
    mock_bridge.expect_oneshot_request(
        "/tool/call", method="POST", headers={"X-Nanny-Run-Id": "team-alpha"}
    ).respond_with_json({"status": "allowed", "result": ""})
    client.call_tool("search", {})
    mock_bridge.check_assertions()
