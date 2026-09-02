"""Retrying to reach a governor, and knowing when not to.

The distinction this file exists to hold: **waiting for a first connection is
patience; waiting mid-run is running ungoverned.** An orchestrator gives no
ordering guarantee, so a joiner started before its governor has to wait rather
than fail every job it is handed. But once a governor has answered, a later
failure means the thing authorising this run has gone away, and retrying then
would let the agent keep calling tools while nothing was allowing them.
"""

from __future__ import annotations

import httpx
import pytest

from nanny_sdk import _client


@pytest.fixture(autouse=True)
def _fresh_process(monkeypatch):
    """Each test is a process that has never reached a bridge."""
    monkeypatch.setattr(_client, "_reached_bridge", False)
    # Keep the suite fast: the real backoff is measured in seconds.
    monkeypatch.setattr(_client, "_FIRST_CONTACT_BACKOFF", (0.0,))
    monkeypatch.setattr(_client, "FIRST_CONTACT_TIMEOUT_SECONDS", 0.5)


def _boom() -> None:
    raise httpx.ConnectError("connection refused")


def test_it_retries_until_the_governor_answers() -> None:
    calls: list[int] = []

    def flaky() -> str:
        calls.append(1)
        if len(calls) < 3:
            _boom()
        return "joined"

    assert _client._retry_first_contact(flaky) == "joined"
    assert len(calls) == 3, "must keep trying while it has never connected"


def test_it_gives_up_at_the_bound() -> None:
    calls: list[int] = []

    def never() -> str:
        calls.append(1)
        _boom()
        raise AssertionError("unreachable")

    with pytest.raises(httpx.ConnectError):
        _client._retry_first_contact(never)
    assert len(calls) > 1, "a governor that never answers is still retried"


def test_a_failure_after_first_contact_is_not_retried() -> None:
    """The half that keeps enforcement honest.

    Retrying here would mean the agent carries on calling tools while the
    governor that was authorising them is gone.
    """
    calls: list[int] = []

    def fails() -> str:
        calls.append(1)
        _boom()
        raise AssertionError("unreachable")

    _client._mark_reached()

    with pytest.raises(httpx.ConnectError):
        _client._retry_first_contact(fails)
    assert calls == [1], "exactly one attempt once the bridge has been reached"


def test_one_success_disarms_the_retry_permanently() -> None:
    assert not _client.has_reached_bridge()
    _client._retry_first_contact(lambda: "ok")
    assert _client.has_reached_bridge()

    calls: list[int] = []

    def fails() -> str:
        calls.append(1)
        _boom()
        raise AssertionError("unreachable")

    with pytest.raises(httpx.ConnectError):
        _client._retry_first_contact(fails)
    assert calls == [1]
