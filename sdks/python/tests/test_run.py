"""nanny_sdk.fresh_run / run_scope — starting or scoping a governed run."""

from __future__ import annotations

import os
import threading
import time

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


def test_run_scope_sets_the_run_id_for_the_block(monkeypatch: pytest.MonkeyPatch) -> None:
    monkeypatch.delenv("NANNY_RUN_ID", raising=False)
    with nanny_sdk.run_scope("scoped-123") as run_id:
        assert run_id == "scoped-123"
        assert client._run_id() == "scoped-123"


def test_run_scope_generates_an_id_when_none_given(monkeypatch: pytest.MonkeyPatch) -> None:
    monkeypatch.delenv("NANNY_RUN_ID", raising=False)
    with nanny_sdk.run_scope() as run_id:
        assert run_id
        assert client._run_id() == run_id


def test_run_scope_restores_the_previous_state_on_exit(monkeypatch: pytest.MonkeyPatch) -> None:
    monkeypatch.setenv("NANNY_RUN_ID", "env-run")
    assert client._run_id() == "env-run"
    with nanny_sdk.run_scope("scoped-run"):
        assert client._run_id() == "scoped-run"
    # Falls back to the env var again once the scope exits — never leaks.
    assert client._run_id() == "env-run"


def test_run_scope_wins_over_the_env_var(monkeypatch: pytest.MonkeyPatch) -> None:
    """A scope takes priority even if NANNY_RUN_ID is also set — the scoped,
    per-thread id is always the more specific, more recent intent."""
    monkeypatch.setenv("NANNY_RUN_ID", "env-run")
    with nanny_sdk.run_scope("scoped-run"):
        assert client._run_id() == "scoped-run"


def test_run_scope_falls_through_to_env_var_when_unset(monkeypatch: pytest.MonkeyPatch) -> None:
    """The existing single-run-per-process behavior is completely unaffected
    by run_scope existing: with no scope ever entered, _run_id() resolves
    exactly as it always has."""
    monkeypatch.setenv("NANNY_RUN_ID", "env-run")
    assert client._run_id() == "env-run"


def test_concurrent_run_scopes_do_not_clobber_each_other() -> None:
    """The actual race this exists to fix: two threads, each inside its own
    run_scope(), must never observe the other's run id. This is the proof
    that a threaded host (e.g. a web server with one thread per tenant
    session) can safely run two governed runs at once in one process."""
    seen: dict[str, str | None] = {}
    barrier = threading.Barrier(2)

    def worker(name: str, run_id: str) -> None:
        with nanny_sdk.run_scope(run_id):
            barrier.wait()  # both threads are inside their own scope now
            time.sleep(0.05)  # give the other thread a chance to interleave
            seen[name] = client._run_id()

    t1 = threading.Thread(target=worker, args=("a", "run-a"))
    t2 = threading.Thread(target=worker, args=("b", "run-b"))
    t1.start()
    t2.start()
    t1.join()
    t2.join()

    assert seen["a"] == "run-a"
    assert seen["b"] == "run-b"
