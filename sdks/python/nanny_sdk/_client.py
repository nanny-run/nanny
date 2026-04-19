"""Bridge HTTP client.

The bridge uses different transports depending on the OS:

- **Unix (macOS/Linux):** Unix domain socket at ``/tmp/nanny-<token>.sock``.
  The CLI injects ``NANNY_BRIDGE_SOCKET`` into the child process environment.
- **Windows:** TCP loopback on an OS-assigned port.
  The CLI injects ``NANNY_BRIDGE_PORT`` into the child process environment.

``NANNY_SESSION_TOKEN`` is always injected on both platforms.

All environment variables are read at call time (not import time) so tests can
set them via ``monkeypatch`` without reloading the module.

When neither ``NANNY_BRIDGE_SOCKET`` nor ``NANNY_BRIDGE_PORT`` is set the SDK
is in passthrough mode — every decorator is a no-op and no network calls are
made. This is the normal state when running ``python agent.py`` directly
instead of ``nanny run agent.py``.
"""

from __future__ import annotations

import os
from typing import Any

import httpx

from nanny_sdk._context import PolicyContext
from nanny_sdk.exceptions import (
    AgentCompleted,
    AgentNotFound,
    BudgetExhausted,
    MaxStepsReached,
    RuleDenied,
    TimeoutExpired,
    ToolDenied,
)

# ---------------------------------------------------------------------------
# Environment helpers — evaluated lazily so monkeypatch works in tests
# ---------------------------------------------------------------------------


def _socket_path() -> str | None:
    """Unix domain socket path set by the CLI on macOS/Linux."""
    return os.environ.get("NANNY_BRIDGE_SOCKET")


def _port() -> str | None:
    """TCP port set by the CLI on Windows."""
    return os.environ.get("NANNY_BRIDGE_PORT")


def _token() -> str:
    return os.environ.get("NANNY_SESSION_TOKEN", "")


def is_passthrough() -> bool:
    """True when the SDK is running outside ``nanny run`` (no bridge present).

    Checks ``NANNY_BRIDGE_SOCKET`` first (Unix), then ``NANNY_BRIDGE_PORT``
    (Windows). Neither set → passthrough.
    """
    return _socket_path() is None and _port() is None


def _make_client(**kwargs: Any) -> httpx.Client:
    """Return an ``httpx.Client`` connected to the bridge.

    - Unix socket present → ``HTTPTransport(uds=...)`` with ``base_url=http://localhost``
    - TCP port present    → plain TCP with ``base_url=http://127.0.0.1:<port>``

    Raises ``RuntimeError`` if called in passthrough mode (should never happen
    because decorators check ``is_passthrough()`` first).
    """
    sock = _socket_path()
    if sock is not None:
        transport = httpx.HTTPTransport(uds=sock)
        return httpx.Client(transport=transport, base_url="http://localhost", **kwargs)
    port = _port()
    if port is not None:
        return httpx.Client(base_url=f"http://127.0.0.1:{port}", **kwargs)
    raise RuntimeError(  # pragma: no cover
        "nanny: bridge not available "
        "(NANNY_BRIDGE_SOCKET and NANNY_BRIDGE_PORT are both unset)"
    )


def _headers() -> dict[str, str]:
    return {"X-Nanny-Session-Token": _token()}


# ---------------------------------------------------------------------------
# Stop-reason dispatch
# ---------------------------------------------------------------------------


def _raise_for_stop(reason: str, tool_name: str = "", rule_name: str = "") -> None:
    """Convert a stop-reason string from the bridge into a typed exception.

    ``tool_name`` and ``rule_name`` carry the optional detail fields that the
    bridge includes in a ``ToolDenied`` or ``RuleDenied`` deny response.
    """
    match reason:
        case "MaxStepsReached":
            raise MaxStepsReached()
        case "BudgetExhausted":
            raise BudgetExhausted()
        case "TimeoutExpired":
            raise TimeoutExpired()
        case "AgentCompleted":
            raise AgentCompleted()
        case "AgentNotFound":
            raise AgentNotFound()
        case "ToolDenied":
            raise ToolDenied(tool_name)
        case "RuleDenied":
            raise RuleDenied(rule_name)
        case _:
            raise RuntimeError(f"nanny: unknown stop reason: {reason!r}")


# ---------------------------------------------------------------------------
# Bridge calls
# ---------------------------------------------------------------------------


def health() -> bool:
    """Connectivity check — returns True if bridge responds with state running."""
    with _make_client(timeout=5.0) as c:
        resp = c.get("/health", headers=_headers())
    resp.raise_for_status()
    data: dict[str, str] = resp.json()
    return data.get("state") == "running"


def get_status() -> PolicyContext:
    """GET /status — returns live execution counters as a ``PolicyContext``.

    ``requested_tool`` and ``last_tool_args`` are not populated from ``/status``;
    the ``@tool`` decorator sets them on the returned context before passing it
    to rules.

    The bridge response uses short wire names (``step``, ``cost_spent``) which
    ``PolicyContext.from_dict()`` maps to Python field names automatically.
    """
    with _make_client(timeout=5.0) as c:
        resp = c.get("/status", headers=_headers())
    resp.raise_for_status()
    return PolicyContext.from_dict(resp.json())


def call_tool(tool_name: str, cost: int, args: dict[str, Any]) -> None:
    """POST /tool/call — raises a NannyStop subclass if denied, returns None if allowed."""
    payload = {"tool": tool_name, "cost": cost, "args": args}
    with _make_client(timeout=10.0) as c:
        resp = c.post("/tool/call", json=payload, headers=_headers())
    resp.raise_for_status()
    data: dict[str, Any] = resp.json()
    if data.get("status") == "denied":
        _raise_for_stop(
            str(data.get("reason", "")),
            tool_name=str(data.get("tool_name") or ""),
            rule_name=str(data.get("rule_name") or ""),
        )


def agent_enter(name: str) -> None:
    """POST /agent/enter — activate a named limit scope.

    The bridge returns 404 when the named scope is not in nanny.toml —
    raises ``AgentNotFound`` in that case.
    """
    with _make_client(timeout=5.0) as c:
        resp = c.post("/agent/enter", json={"name": name}, headers=_headers())
    if resp.status_code == 404:
        raise AgentNotFound()
    resp.raise_for_status()


def agent_exit(name: str) -> None:
    """POST /agent/exit — deactivate the named limit scope.

    Silently ignored if the bridge closed the connection after a stop event —
    the bridge already recorded the scope exit when it issued the stop.
    """
    try:
        with _make_client(timeout=5.0) as c:
            c.post("/agent/exit", json={}, headers=_headers())
    except Exception:
        pass


def report_stop(reason: str) -> None:
    """POST /stop — notify the bridge of a stop reason before raising.

    The bridge records this so the NDJSON log shows the real stop reason
    (e.g. ``RuleDenied``) instead of ``ProcessCrashed`` when the process exits.
    Silently ignored if the bridge is unreachable — best-effort only.
    """
    try:
        with _make_client(timeout=2.0) as c:
            c.post("/stop", json={"reason": reason}, headers=_headers())
    except Exception:
        pass
