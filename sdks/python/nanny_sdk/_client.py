"""Bridge HTTP client.

The bridge uses different transports depending on the OS and configuration:

- **Unix (macOS/Linux):** Unix domain socket at ``/tmp/nanny-<token>.sock``.
  The CLI injects ``NANNY_BRIDGE_SOCKET`` into the child process environment.
- **Windows:** TCP loopback on an OS-assigned port.
  The CLI injects ``NANNY_BRIDGE_PORT`` into the child process environment.
- **Network (cross-process / cross-machine):** TCP + mTLS to the address in
  ``NANNY_BRIDGE_ADDR``. The CLI auto-injects ``NANNY_BRIDGE_CERT``,
  ``NANNY_BRIDGE_KEY``, and ``NANNY_BRIDGE_CA`` from ``~/.nanny/certs/`` when
  ``NANNY_BRIDGE_ADDR`` is set. Cross-machine deployments set these env vars
  manually.

``NANNY_SESSION_TOKEN`` is always injected on all platforms.

Transport priority:
1. ``NANNY_BRIDGE_SOCKET``  — Unix domain socket (macOS/Linux local)
2. ``NANNY_BRIDGE_PORT``    — TCP loopback (Windows local)
3. ``NANNY_BRIDGE_ADDR``    — TCP + mTLS (network / cross-machine)
4. None of the above        → passthrough (all decorators are no-ops)

All environment variables are read at call time (not import time) so tests can
set them via ``monkeypatch`` without reloading the module.

When none of the three transport env vars are set the SDK is in passthrough
mode — every decorator is a no-op and no network calls are made. This is the
normal state when running ``python agent.py`` directly instead of
``nanny run agent.py``.
"""

from __future__ import annotations

import os
import ssl
import tempfile
from collections.abc import Generator
from contextlib import contextmanager
from pathlib import Path
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


def _bridge_addr() -> str | None:
    """Network governance server address (host:port) for cross-process enforcement.

    Set automatically by ``nanny run`` when ``NANNY_BRIDGE_ADDR`` is in the
    environment. Cross-machine deployments set this manually.
    """
    val = os.environ.get("NANNY_BRIDGE_ADDR")
    return val if val else None


def _token() -> str:
    return os.environ.get("NANNY_SESSION_TOKEN", "")


# ---------------------------------------------------------------------------
# mTLS cert resolution — used when NANNY_BRIDGE_ADDR is set
# ---------------------------------------------------------------------------
#
# Two formats are accepted for all three NANNY_BRIDGE_CERT/KEY/CA env vars:
#
#   File path:   NANNY_BRIDGE_CA=/path/to/ca.crt
#   Inline PEM:  NANNY_BRIDGE_CA="-----BEGIN CERTIFICATE-----\n..."
#
# Inline PEM works without a filesystem — useful in Docker/k8s where secrets
# are injected as env var values rather than mounted files.
#
# NANNY_BRIDGE_CERT may be a combined cert+key PEM bundle, in which case
# NANNY_BRIDGE_KEY can be omitted.


def _default_certs_dir() -> Path:
    return Path.home() / ".nanny" / "certs"


def _resolve_pem_value(env_var: str, fallback: Path) -> str | None:
    """Resolve PEM content from an env var or fallback file.

    - Env var starts with ``-----BEGIN`` → treat as inline PEM, return as-is.
    - Env var is a non-empty string      → treat as file path, return as-is.
    - Env var is absent                  → return fallback path string if the
      file exists, else ``None``.
    """
    val = os.environ.get(env_var)
    if val:
        return val  # inline PEM or file path — both returned as-is
    return str(fallback) if fallback.exists() else None


@contextmanager
def _as_path(pem_or_path: str) -> Generator[str, None, None]:
    """Yield a filesystem path for the given PEM string or file path.

    - Inline PEM (starts with ``-----BEGIN``): write to a NamedTemporaryFile,
      yield the path, delete the file on exit.  ``ssl.SSLContext.load_cert_chain``
      reads the file immediately when called, so the temp file is safe to delete
      as soon as the ``with`` block exits.
    - Anything else: yield unchanged (already a file path).
    """
    if pem_or_path.startswith("-----BEGIN"):
        tmp = tempfile.NamedTemporaryFile(mode="wb", suffix=".pem", delete=False)
        try:
            tmp.write(pem_or_path.encode())
            tmp.flush()
            tmp.close()
            yield tmp.name
        finally:
            try:
                os.unlink(tmp.name)
            except OSError:
                pass
    else:
        yield pem_or_path


def _build_ssl_context(cert_val: str, key_val: str | None, ca_val: str) -> ssl.SSLContext:
    """Build an ``ssl.SSLContext`` for mTLS from resolved cert/key/CA values.

    Each value may be an inline PEM string or a file path.
    ``load_verify_locations(cadata=...)`` accepts inline PEM directly.
    ``load_cert_chain`` requires file paths — ``_as_path`` handles the
    temp-file dance for inline PEM values.
    """
    ctx = ssl.create_default_context()

    # CA — verify the server certificate.
    if ca_val.startswith("-----BEGIN"):
        ctx.load_verify_locations(cadata=ca_val)
    else:
        ctx.load_verify_locations(cafile=ca_val)

    # Client cert + key — prove our identity to the server (mTLS).
    # ssl.SSLContext.load_cert_chain reads the files immediately, so
    # _as_path temp files are cleaned up while the data is already loaded.
    with _as_path(cert_val) as cert_path:
        if key_val:
            with _as_path(key_val) as key_path:
                ctx.load_cert_chain(certfile=cert_path, keyfile=key_path)
        else:
            # Key embedded in cert bundle (combined PEM).
            ctx.load_cert_chain(certfile=cert_path)

    return ctx


# ---------------------------------------------------------------------------
# Passthrough detection
# ---------------------------------------------------------------------------


def is_passthrough() -> bool:
    """True when the SDK is running outside ``nanny run`` (no bridge present).

    All three transport env vars must be absent for passthrough mode:
    - ``NANNY_BRIDGE_SOCKET`` (Unix domain socket)
    - ``NANNY_BRIDGE_PORT``   (TCP loopback)
    - ``NANNY_BRIDGE_ADDR``   (network mTLS)

    Checking only the first two would silently skip enforcement when the
    process was started with ``NANNY_BRIDGE_ADDR`` set.
    """
    return _socket_path() is None and _port() is None and _bridge_addr() is None


# ---------------------------------------------------------------------------
# Client factory
# ---------------------------------------------------------------------------


def _make_client(**kwargs: Any) -> httpx.Client:
    """Return an ``httpx.Client`` connected to the bridge.

    Transport selection:
    1. Unix socket present  → ``HTTPTransport(uds=...)`` with ``base_url=http://localhost``
    2. TCP port present     → plain TCP with ``base_url=http://127.0.0.1:<port>``
    3. NANNY_BRIDGE_ADDR set → HTTPS with mTLS, ``base_url=https://<addr>``

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

    addr = _bridge_addr()
    if addr is not None:
        # mTLS: build ssl.SSLContext from env vars or ~/.nanny/certs/ defaults.
        # Both file paths and inline PEM (NANNY_BRIDGE_CERT="-----BEGIN …") work.
        certs_dir = _default_certs_dir()
        cert_val = _resolve_pem_value("NANNY_BRIDGE_CERT", certs_dir / "client.crt")
        key_val  = _resolve_pem_value("NANNY_BRIDGE_KEY",  certs_dir / "client.key")
        ca_val   = _resolve_pem_value("NANNY_BRIDGE_CA",   certs_dir / "ca.crt")
        if cert_val and ca_val:
            ssl_ctx = _build_ssl_context(cert_val, key_val, ca_val)
            return httpx.Client(base_url=f"https://{addr}", verify=ssl_ctx, **kwargs)

    raise RuntimeError(  # pragma: no cover
        "nanny: bridge not available "
        "(NANNY_BRIDGE_SOCKET, NANNY_BRIDGE_PORT, and NANNY_BRIDGE_ADDR are all unset)"
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


def report_stop_rule(tool_name: str, rule_name: str) -> None:
    """POST /stop with RuleDenied metadata so the bridge can emit the NDJSON event.

    Client-side rule denials never reach ``/tool/call``, so the bridge has no
    other opportunity to append a ``RuleDenied`` event to the stream.
    Silently ignored if the bridge is unreachable — best-effort only.
    """
    try:
        with _make_client(timeout=2.0) as c:
            c.post(
                "/stop",
                json={"reason": "RuleDenied", "tool": tool_name, "rule_name": rule_name},
                headers=_headers(),
            )
    except Exception:
        pass
