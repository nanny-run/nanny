"""Declare which agentic harness is running this agent.

Mirrors ``nanny::set_harness`` in the Rust SDK.

You normally never call this. ``instrument()`` detects the well-known
frameworks from the call stack and the imported modules, and reports what it
found alongside each LLM call. This exists for the case detection cannot
cover, and it is not a rare one: an application whose agent loop is its own
code matches no framework, so detection correctly reports nothing and every
run reaches the cloud as an unattributed one. A first-party agent is not an
unknown harness, it is simply not a framework.

Explicit declaration wins over detection for the life of the process, so a
call here is never overridden by a framework that happens to be importable.
"""

from __future__ import annotations

from typing import Any

from nanny_sdk import _client as _bridge


def set_harness(name: str, version: str | None = None) -> None:
    """Declare the harness running this agent to the bridge.

    Records the harness so runs can be compared by what drove them. Deduped
    bridge-side, so calling it repeatedly is harmless and re-declaring on every
    request is safe.

    ``name`` is how it reads in the cloud (``"gotm-engine"``, ``"crewai"``);
    ``version`` is optional and may change without the harness becoming a
    different harness. An empty ``name`` is ignored rather than reported: a
    blank harness is worse than an absent one, because it looks declared.

    Passthrough mode (no bridge active) is a no-op, the same contract as
    ``set_app`` and the decorators. Fire and forget: the response is ignored
    and transport errors are swallowed, so declaring a harness can never
    interrupt the agent.
    """
    if not name.strip():
        return
    # Recorded even in passthrough, so a process that declares at import time
    # and only later reaches a bridge still reports what it declared.
    _bridge.set_harness_override(name)
    if _bridge.is_passthrough():
        return
    body: dict[str, Any] = {"name": name}
    if version:
        body["version"] = version
    try:
        with _bridge._make_client(timeout=5.0) as http:
            http.post("/harness", json=body, headers=_bridge._headers())
    except Exception:  # noqa: BLE001
        pass
