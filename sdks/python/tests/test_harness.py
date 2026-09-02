"""``nanny_sdk.set_harness``: declare what is running this agent.

Mirrors the Rust SDK's ``nanny::set_harness``. The gap it closes: detection
recognises twelve frameworks, so an application whose agent loop is its own
code reports nothing and reaches the cloud unattributed. A first-party agent is
not an unknown harness, it simply is not a framework.
"""

from __future__ import annotations

from typing import Any

import pytest
from pytest_httpserver import HTTPServer

from nanny_sdk import _client, set_harness


@pytest.fixture(autouse=True)
def _no_declaration(monkeypatch):
    """Each test starts as a process that has declared nothing."""
    monkeypatch.setattr(_client, "_harness_override", None)


def _harness_posts(bridge: HTTPServer) -> list[dict[str, Any]]:
    return [
        req.get_json()
        for req, _resp in bridge.log
        if req.path == "/harness" and req.method == "POST"
    ]


def _expect_harness(bridge: HTTPServer) -> None:
    bridge.expect_request("/harness", method="POST").respond_with_json({"status": "ok"})


def test_declares_the_name(mock_bridge: HTTPServer) -> None:
    _expect_harness(mock_bridge)
    set_harness("gotm-engine")
    assert _harness_posts(mock_bridge) == [{"name": "gotm-engine"}]


def test_version_is_optional(mock_bridge: HTTPServer) -> None:
    _expect_harness(mock_bridge)
    set_harness("gotm-engine", "1.4.0")
    assert _harness_posts(mock_bridge) == [{"name": "gotm-engine", "version": "1.4.0"}]


def test_a_blank_name_is_ignored(mock_bridge: HTTPServer) -> None:
    """A blank harness is worse than an absent one: it looks declared."""
    _expect_harness(mock_bridge)
    set_harness("")
    set_harness("   ")
    assert _harness_posts(mock_bridge) == []


def test_an_explicit_declaration_beats_detection(mock_bridge: HTTPServer) -> None:
    """A framework being importable does not mean it drove the call."""
    _expect_harness(mock_bridge)
    set_harness("gotm-engine")
    assert _client.harness_override() == "gotm-engine"


def test_it_never_interrupts_the_agent(monkeypatch) -> None:
    """Attribution is fire and forget, the same contract as ``set_app``."""

    def explode(*_args: Any, **_kwargs: Any) -> Any:
        raise RuntimeError("bridge is on fire")

    monkeypatch.setattr("nanny_sdk._client._make_client", explode)
    monkeypatch.setattr("nanny_sdk._client.is_passthrough", lambda: False)
    set_harness("gotm-engine")  # must not raise


def test_passthrough_still_records_the_declaration(monkeypatch) -> None:
    """A process that declares at import time and only later reaches a bridge
    still reports what it declared."""
    monkeypatch.setattr("nanny_sdk._client.is_passthrough", lambda: True)
    set_harness("gotm-engine")
    assert _client.harness_override() == "gotm-engine"
