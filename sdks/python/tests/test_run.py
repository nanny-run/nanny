"""nanny_sdk.fresh_run — starting a new governed run within one process."""

from __future__ import annotations

import os

import pytest

import nanny_sdk
from nanny_sdk import _client as client


def test_fresh_run_sets_a_fresh_nanny_run_id(monkeypatch: pytest.MonkeyPatch) -> None:
    monkeypatch.delenv("NANNY_RUN_ID", raising=False)
    run_id = nanny_sdk.fresh_run()
    assert os.environ["NANNY_RUN_ID"] == run_id


def test_fresh_run_returns_a_different_id_each_call(monkeypatch: pytest.MonkeyPatch) -> None:
    monkeypatch.delenv("NANNY_RUN_ID", raising=False)
    first = nanny_sdk.fresh_run()
    second = nanny_sdk.fresh_run()
    assert first != second


def test_fresh_run_replaces_a_previously_set_run_id(monkeypatch: pytest.MonkeyPatch) -> None:
    monkeypatch.setenv("NANNY_RUN_ID", "old-run")
    new_id = nanny_sdk.fresh_run()
    assert new_id != "old-run"
    assert os.environ["NANNY_RUN_ID"] == new_id


def test_governed_requests_after_fresh_run_carry_the_new_id(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    """End to end: the id fresh_run() sets is exactly what the next request
    sends as X-Nanny-Run-Id, the same header G3's existing tests cover for
    manually-set NANNY_RUN_ID (see test_client.py) — fresh_run() is just a
    public, discoverable way to set the same thing, not a different
    mechanism."""
    monkeypatch.setenv("NANNY_SESSION_TOKEN", "tok")
    monkeypatch.delenv("NANNY_RUN_ID", raising=False)
    run_id = nanny_sdk.fresh_run()
    headers = client._headers()
    assert headers["X-Nanny-Run-Id"] == run_id
