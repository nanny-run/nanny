"""nanny_sdk.run_scope, scoping a governed run to a thread or task."""

from __future__ import annotations

import threading
import time

import pytest

import nanny_sdk
from nanny_sdk import _client as client


def test_fresh_run_is_gone() -> None:
    """fresh_run wrote a process-global env var and raced under any threaded
    host. run_scope replaces it in every case, so the old name is removed
    rather than deprecated: a name that still imports but no longer isolates
    would leave callers believing they had isolation they do not have.
    """
    assert not hasattr(nanny_sdk, "fresh_run")


def test_governed_requests_inside_a_scope_carry_its_id(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    """End to end: the id run_scope() sets is exactly what the next request
    sends as X-Nanny-Run-Id, the same header G3's existing tests cover for a
    manually-set NANNY_RUN_ID (see test_client.py).
    """
    monkeypatch.setenv("NANNY_SESSION_TOKEN", "tok")
    monkeypatch.delenv("NANNY_RUN_ID", raising=False)
    with nanny_sdk.run_scope() as run_id:
        assert client._headers()["X-Nanny-Run-Id"] == run_id


def test_a_scope_beats_the_env_var(monkeypatch: pytest.MonkeyPatch) -> None:
    """Resolution order: an active scope wins over NANNY_RUN_ID.

    The env var is how separate processes opt into a shared run; a scope is a
    narrower, deliberate statement inside one process, so it takes precedence.
    """
    monkeypatch.setenv("NANNY_SESSION_TOKEN", "tok")
    monkeypatch.setenv("NANNY_RUN_ID", "from-env")
    with nanny_sdk.run_scope("from-scope"):
        assert client._headers()["X-Nanny-Run-Id"] == "from-scope"
    # Restored on exit: the env var is visible again.
    assert client._headers()["X-Nanny-Run-Id"] == "from-env"


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
    # Falls back to the env var again once the scope exits: never leaks.
    assert client._run_id() == "env-run"


def test_run_scope_wins_over_the_env_var(monkeypatch: pytest.MonkeyPatch) -> None:
    """A scope takes priority even if NANNY_RUN_ID is also set, the scoped,
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

def test_a_run_id_is_typed_and_prefix_addressable() -> None:
    """Mirrors `nanny_config`'s own test: the two implementations write ids
    into the same log, so a drift in shape is a drift in the wire format."""
    from nanny_sdk.run import new_run_id

    rid = new_run_id()
    assert rid.startswith("run_")
    body = rid.removeprefix("run_")
    assert len(body) == 32, rid
    assert all(c in "0123456789abcdef" for c in body), rid


def test_run_ids_do_not_share_a_leading_prefix() -> None:
    """The property a short display form depends on. A time-ordered id would
    fail this, which is why one was rejected."""
    from nanny_sdk.run import new_run_id

    ids = [new_run_id() for _ in range(64)]
    assert len({rid[:12] for rid in ids}) == len(ids)
