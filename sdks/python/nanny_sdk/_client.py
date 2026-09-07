"""Bridge HTTP client.

One transport, on every platform: TCP to the address in ``NANNY_BRIDGE_ADDR``,
which ``nanny run`` injects into the process it launches.

Loopback is plain HTTP; anything else requires mTLS, and the CLI injects
``NANNY_BRIDGE_CERT``, ``NANNY_BRIDGE_KEY`` and ``NANNY_BRIDGE_CA`` alongside
the address. Cross-machine deployments set all four themselves.

``NANNY_SESSION_TOKEN`` is always injected.

There used to be three rungs here: a Unix domain socket on macOS and Linux, TCP
loopback on Windows, and this one for anything over a network. Every SDK in
every language had to implement all three, and the runtime had to carry two
implementations of one enforcement surface to serve them. There is one governor
now, so there is one way to reach it.

Transport resolution:
1. ``NANNY_BRIDGE_ADDR``: TCP, plain on loopback and mTLS anywhere else
2. Unset → passthrough (all decorators are no-ops)

All environment variables are read at call time (not import time) so tests can
set them via ``monkeypatch`` without reloading the module.

Passthrough is the normal state when running ``python agent.py`` directly
instead of under ``nanny run``.
"""

from __future__ import annotations

import ipaddress
import json
import os
import ssl
import tempfile
import threading
import time
from collections.abc import Callable, Generator
from contextlib import contextmanager
from pathlib import Path
from typing import Any, TypeVar

import httpx

from nanny_sdk._context import PolicyContext
from nanny_sdk.exceptions import (
    AgentCompleted,
    BridgeUnavailable,
    ExecutionStopped,
    RuleDenied,
    ToolDenied,
)

T = TypeVar("T")

# ---------------------------------------------------------------------------
# Environment helpers, evaluated lazily so monkeypatch works in tests
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


def _run_id() -> str | None:
    """Run id for this process on the governance server.

    Checks the `run_scope()` ContextVar first (isolated per thread/task, so
    a host running several concurrent runs never races on it), then falls
    back to `NANNY_RUN_ID`, set by `nanny run` per invocation, or shared
    across processes on purpose to share one run. Absent means the
    server's default run, shared by every headerless client. The local bridge
    ignores it, one process is always one run. Mirrors the Rust client
    (`crates/cli/src/lib.rs`): run id is which run you are part of, distinct
    from `NANNY_SESSION_TOKEN` (who you are).

    With no scope ever entered, this resolves exactly as it did before
    `run_scope()` existed.
    """
    from nanny_sdk.run import _scoped_run_id

    scoped = _scoped_run_id()
    if scoped:
        return scoped
    val = os.environ.get("NANNY_RUN_ID")
    return val if val else None


# ---------------------------------------------------------------------------
# mTLS cert resolution, used when NANNY_BRIDGE_ADDR is set
# ---------------------------------------------------------------------------
#
# Two formats are accepted for all three NANNY_BRIDGE_CERT/KEY/CA env vars:
#
#   File path:   NANNY_BRIDGE_CA=/path/to/ca.crt
#   Inline PEM:  NANNY_BRIDGE_CA="-----BEGIN CERTIFICATE-----\n..."
#
# Inline PEM works without a filesystem, useful in Docker/k8s where secrets
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
        return val  # inline PEM or file path, both returned as-is
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
    ``load_cert_chain`` requires file paths, ``_as_path`` handles the
    temp-file dance for inline PEM values.
    """
    ctx = ssl.create_default_context()

    # CA: verify the server certificate.
    if ca_val.startswith("-----BEGIN"):
        ctx.load_verify_locations(cadata=ca_val)
    else:
        ctx.load_verify_locations(cafile=ca_val)

    # Client cert + key: prove our identity to the server (mTLS).
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


def _split_host(addr: str) -> str:
    """Return the host portion of a ``host:port`` address (handles IPv6 ``[::1]:port``)."""
    if addr.startswith("["):
        return addr[1 : addr.index("]")]
    return addr.rsplit(":", 1)[0]


def _is_loopback_host(host: str) -> bool:
    """True if ``host`` is loopback.

    Mirrors the governance server, which serves plain HTTP on loopback
    (127.0.0.0/8, ::1) and mTLS only on non-loopback addresses
    (see ``crates/bridge/src/network.rs``). The client must match: plain HTTP for
    loopback, mTLS otherwise. Using HTTPS against the plain-HTTP loopback server
    raises ``SSL: WRONG_VERSION_NUMBER``.
    """
    if host == "localhost":
        return True
    try:
        return ipaddress.ip_address(host).is_loopback
    except ValueError:
        return False


# ---------------------------------------------------------------------------
# Client factory
# ---------------------------------------------------------------------------


def _make_client(**kwargs: Any) -> httpx.Client:
    """Return an ``httpx.Client`` connected to the bridge.

    Transport selection:
    1. Unix socket present  → ``HTTPTransport(uds=...)`` with ``base_url=http://localhost``
    2. TCP port present     → plain TCP with ``base_url=http://127.0.0.1:<port>``
    3. NANNY_BRIDGE_ADDR set → loopback: plain HTTP (``base_url=http://<addr>``);
       non-loopback: HTTPS with mTLS (``base_url=https://<addr>``). This mirrors the
       server, which serves plain HTTP on loopback and mTLS off-loopback.

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
        # Mirror the server's transport (crates/bridge/src/network.rs): loopback is
        # served as plain HTTP, non-loopback as mTLS. Connecting with HTTPS to the
        # plain-HTTP loopback server raises SSL: WRONG_VERSION_NUMBER.
        if _is_loopback_host(_split_host(addr)):
            return httpx.Client(base_url=f"http://{addr}", **kwargs)
        # mTLS: build ssl.SSLContext from env vars or ~/.nanny/certs/ defaults.
        # Both file paths and inline PEM (NANNY_BRIDGE_CERT="-----BEGIN …") work.
        certs_dir = _default_certs_dir()
        cert_val = _resolve_pem_value("NANNY_BRIDGE_CERT", certs_dir / "client.crt")
        key_val = _resolve_pem_value("NANNY_BRIDGE_KEY", certs_dir / "client.key")
        ca_val = _resolve_pem_value("NANNY_BRIDGE_CA", certs_dir / "ca.crt")
        if cert_val and ca_val:
            ssl_ctx = _build_ssl_context(cert_val, key_val, ca_val)
            return httpx.Client(base_url=f"https://{addr}", verify=ssl_ctx, **kwargs)

    raise RuntimeError(  # pragma: no cover
        "nanny: bridge not available "
        "(NANNY_BRIDGE_SOCKET, NANNY_BRIDGE_PORT, and NANNY_BRIDGE_ADDR are all unset)"
    )


@contextmanager
def _bridge_call() -> Generator[None, None, None]:
    """Translate a network-level failure to reach the bridge into ``BridgeUnavailable``.

    ``httpx.TransportError`` covers connection refused, DNS failure, TLS
    handshake failure, and timeouts: anything below the HTTP layer, before a
    response was ever received. A response the bridge actually sent (even an
    unwelcome status code) is not a ``TransportError`` and is handled by each
    call site's own status-code logic; this only covers "never got a response
    at all." Without this, the raw httpx/httpcore exception (and its full
    traceback) propagates to the calling agent, which the manifesto's
    fail-closed guarantee already treats as unacceptable for the identical
    case in ``@rule`` evaluation. This makes every bridge call consistent
    with that, not just ``GET /status``.
    """
    try:
        yield
    except httpx.TransportError as exc:
        raise BridgeUnavailable() from exc


# ---------------------------------------------------------------------------
# First contact
# ---------------------------------------------------------------------------

#: Whether this process has ever reached the bridge. Flipped once, on the first
#: successful call, and never back.
_reached_bridge = False
_reach_lock = threading.Lock()

#: How long to keep trying to reach a governor that has never answered. Long
#: enough to cover an orchestrator starting a joiner before the governor it
#: joins, short enough that a genuinely absent governor is reported rather than
#: hung on.
FIRST_CONTACT_TIMEOUT_SECONDS = 30.0
_FIRST_CONTACT_BACKOFF = (0.25, 0.5, 1.0, 2.0, 4.0)


#: An explicitly declared harness, set by ``nanny_sdk.set_harness``. Held here
#: rather than in ``instrument`` because ``nanny_sdk/__init__.py`` binds the
#: name ``instrument`` to a function, which shadows the submodule of that name
#: and makes the module unreachable by any import form.
_harness_override: str | None = None


def set_harness_override(name: str) -> None:
    """Pin the harness, beating detection for the life of the process."""
    global _harness_override
    _harness_override = name


def harness_override() -> str | None:
    return _harness_override


def _mark_reached() -> None:
    global _reached_bridge
    with _reach_lock:
        _reached_bridge = True


def has_reached_bridge() -> bool:
    return _reached_bridge


def _retry_first_contact(call: Callable[[], T]) -> T:
    """Run *call*, retrying only while this process has never reached the bridge.

    **Waiting for a first connection is patience; waiting mid-run is running
    ungoverned.** Those are different things and must not share a code path.
    An orchestrator gives no ordering guarantee, so a joiner that starts before
    its governor would otherwise fail every job it is handed until something
    restarted it, and a governor redeploy would take out the fleet rather than
    pause it. But once a governor has answered, a later failure means the
    governor this run is being enforced by has gone away, and retrying then
    would let the agent keep calling tools while nothing was authorising them.

    So the retry is armed exactly once, before the first success, and disarms
    permanently the moment one call gets through.
    """
    if _reached_bridge:
        result = call()
        _mark_reached()
        return result

    deadline = time.monotonic() + FIRST_CONTACT_TIMEOUT_SECONDS
    attempt = 0
    while True:
        try:
            result = call()
        except httpx.TransportError:
            remaining = deadline - time.monotonic()
            if remaining <= 0:
                raise
            delay = _FIRST_CONTACT_BACKOFF[min(attempt, len(_FIRST_CONTACT_BACKOFF) - 1)]
            time.sleep(min(delay, remaining))
            attempt += 1
            continue
        _mark_reached()
        return result


def _send(
    method: str,
    path: str,
    *,
    timeout: float,
    json: Any | None = None,
) -> httpx.Response:
    """Perform one enforcement request, retrying only before first contact.

    Every call that enforcement depends on goes through here, so the
    first-contact rule is stated once rather than at four call sites that could
    drift apart. Fire-and-forget reporting (`/app`, `/harness`, `/llm/usage`)
    deliberately does not: it already swallows its own failures, and retrying
    attribution would delay an agent for something that does not govern it.
    """

    def once() -> httpx.Response:
        with _make_client(timeout=timeout) as c:
            resp: httpx.Response = c.request(method, path, json=json, headers=_headers())
            return resp

    with _bridge_call():
        return _retry_first_contact(once)


def _headers() -> dict[str, str]:
    h = {"X-Nanny-Session-Token": _token()}
    run_id = _run_id()
    if run_id:
        h["X-Nanny-Run-Id"] = run_id
    return h


# ---------------------------------------------------------------------------
# Stop-reason dispatch
# ---------------------------------------------------------------------------


def _raise_for_stop(reason: str, tool_name: str = "", rule_name: str = "") -> None:
    """Convert a stop-reason string from the bridge into a typed exception.

    ``tool_name`` and ``rule_name`` carry the optional detail fields that the
    bridge includes in a ``ToolDenied`` or ``RuleDenied`` deny response.
    """
    match reason:
        case "AgentCompleted":
            raise AgentCompleted()
        case "ToolDenied":
            raise ToolDenied(tool_name)
        case "RuleDenied":
            raise RuleDenied(rule_name)
        case _:
            raise RuntimeError(f"nanny: unknown stop reason: {reason!r}")


def _raise_stop_from_410(resp: httpx.Response) -> None:
    """Map a 410 Gone (this run already stopped) to a typed ``NannyStop``.

    Mirrors the Rust client: a stopped run answers action endpoints with 410
    carrying the stop reason (``{"error":"execution stopped","reason":"…"}``).
    We surface it as a typed stop instead of letting httpx raise a raw
    ``HTTPStatusError``, so agents and frameworks catch it cleanly. The run
    stopped on an earlier call, possibly on another process sharing the same
    ``NANNY_RUN_ID``, so the precise tool/rule detail is not on this response;
    ``AgentCompleted`` maps to its class, everything else to
    ``ExecutionStopped`` carrying the reason.
    """
    reason = ""
    try:
        reason = str(resp.json().get("reason", ""))
    except Exception:  # noqa: BLE001 (a malformed body still means the run stopped)
        pass
    match reason:
        case "AgentCompleted":
            raise AgentCompleted()
        case _:
            raise ExecutionStopped(reason or "execution stopped")


# ---------------------------------------------------------------------------
# Bridge calls
# ---------------------------------------------------------------------------


def health() -> bool:
    """Connectivity check: returns True if bridge responds with state running."""
    resp = _send("GET", "/health", timeout=5.0)
    resp.raise_for_status()
    data: dict[str, str] = resp.json()
    return data.get("state") == "running"


def get_run_events(run_id: str) -> list[dict[str, Any]]:
    """GET /events for a specific run id, not necessarily this thread's own.

    Unlike every other bridge call in this module, the caller supplies
    ``run_id`` explicitly instead of it being read off the current thread's
    ``run_scope()``/``NANNY_RUN_ID`` (see ``_headers()``): a usage tailer
    polling many runs' events from one background thread has no "current
    run" of its own to read a contextvar for, it needs one specific run's
    events on demand. Returns the run's full buffered event list every
    call (the bridge only clears it via the separate cloud-forwarding
    hook, ``take_run_events``, never on a plain GET), the caller is
    expected to track how many it has already consumed, e.g. by index or
    by best-effort recorded ``ts``.

    Returns `[]` for a run the bridge has no record of yet (a fresh run
    with no events at all is a case, not an error) as well as when the
    bridge is unreachable (``BridgeUnavailable``): a usage tailer is a
    best-effort side channel, not something that should crash a session
    over a transient network blip the actual governed calls already
    tolerate by failing closed elsewhere.
    """
    headers = {"X-Nanny-Session-Token": _token(), "X-Nanny-Run-Id": run_id}
    try:
        with _make_client(timeout=5.0) as c:
            resp = c.get("/events", headers=headers)
    except httpx.TransportError:
        return []
    if resp.status_code != 200:
        return []
    events: list[dict[str, Any]] = []
    for line in resp.text.splitlines():
        line = line.strip()
        if not line:
            continue
        try:
            events.append(json.loads(line))
        except json.JSONDecodeError:
            continue
    return events


def get_status() -> PolicyContext:
    """GET /status: returns live execution counters as a ``PolicyContext``.

    ``requested_tool`` and ``last_tool_args`` are not populated from ``/status``;
    the ``@tool`` decorator sets them on the returned context before passing it
    to rules.

    ``tool_labels`` arrives here too: an out-of-process SDK never reads
    nanny.toml, so ``/status`` is the only place it can learn what a tool is.
    """
    resp = _send("GET", "/status", timeout=5.0)
    resp.raise_for_status()
    return PolicyContext.from_dict(resp.json())


def call_tool(
    tool_name: str,
    args: dict[str, Any],
    cleared_by: list[str] | None = None,
) -> None:
    """POST /tool/call: raises a NannyStop subclass if denied, returns None if allowed.

    Raises ``BridgeUnavailable`` (also a ``NannyStop``) if the bridge can't be
    reached at all: a governed tool call must fail closed, not silently run
    ungoverned because the governor happened to be down.
    """
    payload: dict[str, Any] = {"tool": tool_name, "args": args}
    # Which rules evaluated and allowed this call. Assembled here because this
    # is the only place it exists: rule bodies run in this process, before the
    # bridge is contacted, so the governor cannot observe them. Without it a
    # rule that ran clean and a rule never reached produce identical logs, and
    # the healthy state, which is the normal state for a good control, becomes
    # unprovable.
    if cleared_by:
        payload["cleared_by"] = cleared_by
    resp = _send("POST", "/tool/call", timeout=10.0, json=payload)
    # 410 Gone: this run already stopped, raise a typed stop, not a raw HTTP error.
    if resp.status_code == 410:
        _raise_stop_from_410(resp)
    resp.raise_for_status()
    data: dict[str, Any] = resp.json()
    if data.get("status") == "denied":
        _raise_for_stop(
            str(data.get("reason", "")),
            tool_name=str(data.get("tool_name") or ""),
            rule_name=str(data.get("rule_name") or ""),
        )


def agent_enter(name: str) -> None:
    """POST /agent/enter: record entry into a named scope.

    A scope names a phase of the run so the audit log can attribute each
    verdict to the phase that produced it. Any name is valid: there is nothing
    to look up, so this cannot fail on an unknown name. Raises
    ``BridgeUnavailable`` if the bridge can't be reached at all, same reasoning
    as ``call_tool``.
    """
    resp = _send("POST", "/agent/enter", timeout=5.0, json={"name": name})
    # 410 Gone: this run already stopped, raise a typed stop, not a raw HTTP error.
    if resp.status_code == 410:
        _raise_stop_from_410(resp)
    resp.raise_for_status()


def agent_exit(name: str) -> None:
    """POST /agent/exit: record exit from a named scope.

    Silently ignored if the bridge closed the connection after a stop event,
    the bridge already recorded the scope exit when it issued the stop.
    """
    try:
        with _make_client(timeout=5.0) as c:
            c.post("/agent/exit", json={}, headers=_headers())
    except Exception:
        pass


def report_stop(reason: str) -> None:
    """POST /stop: notify the bridge of a stop reason before raising.

    The bridge records this so the NDJSON log shows the real stop reason
    (e.g. ``RuleDenied``) instead of ``ProcessCrashed`` when the process exits.
    Silently ignored if the bridge is unreachable, best-effort only.
    """
    try:
        with _make_client(timeout=2.0) as c:
            c.post("/stop", json={"reason": reason}, headers=_headers())
    except Exception:
        pass


def declare_rules(rules: list[dict[str, str]]) -> None:
    """POST /rules: record which rules this process registered.

    The half of declared authority the governor cannot see for itself, since
    rule bodies are compiled into this process. Fire-and-forget: a failure to
    declare must never stop a governed run.
    """
    if not rules:
        return
    try:
        with _make_client(timeout=2.0) as c:
            c.post("/rules", json={"rules": rules}, headers=_headers())
    except Exception:
        pass


def report_stop_rule(tool_name: str, rule_name: str, cleared_by: list[str] | None = None) -> None:
    """POST /stop with RuleDenied metadata so the bridge can emit the NDJSON event.

    Client-side rule denials never reach ``/tool/call``, so the bridge has no
    other opportunity to append a ``RuleDenied`` event to the stream.
    Silently ignored if the bridge is unreachable, best-effort only.
    """
    try:
        with _make_client(timeout=2.0) as c:
            c.post(
                "/stop",
                json={
                    "reason": "RuleDenied",
                    "tool": tool_name,
                    "rule_name": rule_name,
                    # Rules that cleared *before* the one that fired. Evaluation
                    # short-circuits, so rules after it never produced a verdict
                    # and listing them would claim a control operated when it
                    # did not.
                    "cleared_by": cleared_by or [],
                },
                headers=_headers(),
            )
    except Exception:
        pass
