"""Bridge HTTP client.

Reads ``NANNY_BRIDGE_PORT`` and ``NANNY_SESSION_TOKEN`` from the environment
at call time (not at import time) so tests can set them via ``monkeypatch``
without reloading the module.

When ``NANNY_BRIDGE_PORT`` is absent the SDK is in passthrough mode — every
decorator is a no-op and no network calls are made.
"""

from __future__ import annotations

import os
from typing import Any

import httpx

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


def _port() -> str | None:
    return os.environ.get("NANNY_BRIDGE_PORT")


def _token() -> str:
    return os.environ.get("NANNY_SESSION_TOKEN", "")


def is_passthrough() -> bool:
    """True when the SDK is running outside ``nanny run`` (no bridge)."""
    return _port() is None


def _base_url() -> str:
    port = _port()
    if port is None:  # pragma: no cover
        raise RuntimeError("nanny: bridge not available (NANNY_BRIDGE_PORT not set)")
    return f"http://127.0.0.1:{port}"


def _headers() -> dict[str, str]:
    return {"X-Nanny-Token": _token()}


# ---------------------------------------------------------------------------
# Stop-reason dispatch
# ---------------------------------------------------------------------------


def _raise_for_stop(reason: str, detail: str = "") -> None:
    """Convert a stop-reason string from the bridge into a typed exception."""
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
            raise AgentNotFound(detail)
        case "ToolDenied":
            raise ToolDenied(detail)
        case "RuleDenied":
            raise RuleDenied(detail)
        case _:
            raise RuntimeError(f"nanny: unknown stop reason: {reason!r}")


# ---------------------------------------------------------------------------
# Bridge calls (implemented incrementally per day)
# ---------------------------------------------------------------------------


def health() -> bool:
    """Connectivity check — returns True if bridge responds with status ok."""
    resp = httpx.get(f"{_base_url()}/health", headers=_headers(), timeout=5.0)
    resp.raise_for_status()
    data: dict[str, str] = resp.json()
    return data.get("status") == "ok"


def call_tool(tool_name: str, cost: int, args: dict[str, Any]) -> None:
    """POST /tool/call — raises a NannyStop subclass if denied, returns None if allowed."""
    payload = {"tool": tool_name, "cost": cost, "args": args}
    resp = httpx.post(
        f"{_base_url()}/tool/call",
        json=payload,
        headers=_headers(),
        timeout=10.0,
    )
    resp.raise_for_status()
    data: dict[str, Any] = resp.json()
    if data.get("status") == "denied":
        _raise_for_stop(str(data.get("reason", "")), str(data.get("detail", "")))


def agent_enter(name: str) -> None:
    """POST /agent/enter — activate a named limit scope."""
    resp = httpx.post(
        f"{_base_url()}/agent/enter",
        json={"name": name},
        headers=_headers(),
        timeout=5.0,
    )
    data = resp.json()
    if data.get("status") == "denied":
        _raise_for_stop(data.get("reason", ""), data.get("detail", ""))


def agent_exit(name: str) -> None:
    """POST /agent/exit — deactivate the named limit scope."""
    httpx.post(
        f"{_base_url()}/agent/exit",
        json={"name": name},
        headers=_headers(),
        timeout=5.0,
    )
