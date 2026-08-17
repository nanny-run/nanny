"""nanny_sdk.fresh_run: start a new governed run within the current process.

Usage::

    import nanny_sdk

    nanny_sdk.fresh_run()   # everything governed after this point is a fresh run

Why this exists: a run is Nanny's real unit of governance, one cumulative
token/step counter, one stop state, "a stop is final." A named ``@agent(...)``
scope does *not* give you a second one of these: it only changes which
ceiling that same, single, ever-growing counter is compared against while
active (confirmed directly against ``crates/bridge/src/lib.rs``: entering a
scope swaps ``current_limits``, it never resets ``tokens_spent``). If your
process does several logically independent phases back to back, a research
phase handing off to a drafting phase, one HTTP request finishing before the
next one starts, and you want each phase to get its own clean budget rather
than inheriting whatever the previous phase already spent, this is how you
say that.

This used to only be possible by setting the ``NANNY_RUN_ID`` environment
variable directly, an internal implementation detail (the client reads it
fresh on every call, never documented as a public integration point). This
function exists so that pattern has a real, discoverable, public name
instead of every integrator needing to read the SDK's own source to find it,
exactly what happened before this existed.

``fresh_run()`` writes ``NANNY_RUN_ID`` into ``os.environ``, which is
process-global. Fine for a short-lived, one-run-per-process caller, but two
runs active *concurrently* in the same process (a threaded host serving more
than one independent conversation at once) would race on that same write.
``run_scope()`` is the concurrent-safe form: a ``ContextVar``, isolated per
thread and per asyncio task, instead of a process global.
"""

from __future__ import annotations

import os
import uuid
from collections.abc import Iterator
from contextlib import contextmanager
from contextvars import ContextVar

__all__ = ["fresh_run", "run_scope"]

_SCOPED_RUN_ID: ContextVar[str | None] = ContextVar("nanny_scoped_run_id", default=None)


def _scoped_run_id() -> str | None:
    """The active `run_scope()` id, if any. Used by `_client._run_id()`."""
    return _SCOPED_RUN_ID.get()


def fresh_run() -> str:
    """Start a new governed run in this process. Returns the new run id.

    Only meaningful when governed through a network server (``nanny run
    --serve`` / ``--join``): the server keys independent state per run id,
    so a stop or budget exhaustion in the run you just left has zero effect
    on the one you're starting. Under local ``nanny run`` (no ``--serve``),
    this is a no-op as far as governance is concerned, one local process is
    always exactly one run, the local bridge has no per-run-id state to
    switch between (confirmed directly: ``crates/bridge/src/lib.rs``'s local
    ``Bridge`` holds one shared, unkeyed state for its whole process
    lifetime), but it's always safe to call regardless of mode, so code
    that might run under either doesn't need to branch on which one it's in.

    Without a bridge active at all (no ``NANNY_BRIDGE_ADDR``/``NANNY_BRIDGE_PORT``
    set, running the process directly, not under ``nanny run``), this still
    returns a fresh id and sets it, harmlessly: there's nothing governed to
    scope it to yet, so it has no observable effect either way.

    For two runs active at once in one process, use ``run_scope()`` instead:
    this function's env var is process-global and will race.
    """
    run_id = str(uuid.uuid4())
    os.environ["NANNY_RUN_ID"] = run_id
    return run_id


@contextmanager
def run_scope(run_id: str | None = None) -> Iterator[str]:
    """Scope a governed run to the current thread or asyncio task, not the
    whole process.

    Use this instead of `fresh_run()` when more than one governed run is
    active at the same time in one process, a threaded or async host serving
    several independent conversations concurrently. Each call's run id is
    isolated via a `ContextVar`, so two runs in flight at once never clobber
    each other's budget or stop state the way two concurrent `fresh_run()`
    calls would, since both would write the same process-global env var.

    Usage::

        with nanny_sdk.run_scope() as run_id:
            ...  # every governed call in this thread/task uses this run_id

    Pass an explicit `run_id` to resume a specific run; omit it to mint a
    fresh one. On exit, the previous scope (or the env-var fallback, if none)
    is restored, so nesting is safe.

    With no scope ever entered, `_client._run_id()` falls through to
    `NANNY_RUN_ID` exactly as before this existed.
    """
    rid = run_id or str(uuid.uuid4())
    token = _SCOPED_RUN_ID.set(rid)
    try:
        yield rid
    finally:
        _SCOPED_RUN_ID.reset(token)
