"""nanny_sdk.fresh_run / run_scope — start (or scope) a governed run.

Usage::

    import nanny_sdk

    nanny_sdk.fresh_run()   # everything governed after this point is a fresh run

Why this exists: a run is Nanny's real unit of governance — one cumulative
token/step counter, one stop state, "a stop is final." A named ``@agent(...)``
scope does *not* give you a second one of these: it only changes which
ceiling that same, single, ever-growing counter is compared against while
active (confirmed directly against ``crates/bridge/src/lib.rs``: entering a
scope swaps ``current_limits``, it never resets ``tokens_spent``). If your
process does several logically independent phases back to back — a research
phase handing off to a drafting phase, one HTTP request finishing before the
next one starts — and you want each phase to get its own clean budget rather
than inheriting whatever the previous phase already spent, this is how you
say that.

This used to only be possible by setting the ``NANNY_RUN_ID`` environment
variable directly, an internal implementation detail (the client reads it
fresh on every call, never documented as a public integration point). This
function exists so that pattern has a real, discoverable, public name
instead of every integrator needing to read the SDK's own source to find it,
exactly what happened before this existed.

``fresh_run()`` sets ``NANNY_RUN_ID`` in ``os.environ``, which is process-global.
That's exactly right for a short-lived, one-invocation-per-run CLI process,
but it breaks the moment two logically independent runs are ever in flight
*concurrently* in the same process — e.g. a threaded web server handling two
tenants' requests at once. Both threads would clobber the same env var,
corrupting each other's run id: one tenant's tokens get billed against the
other's budget, and a stop meant for one run lands on both. ``run_scope()``
below is the concurrent-safe form of the same idea, using a ``ContextVar``
(correctly isolated per thread and per asyncio task) instead of a process
global.
"""

from __future__ import annotations

import os
import uuid
from collections.abc import Iterator
from contextlib import contextmanager
from contextvars import ContextVar

__all__ = ["fresh_run", "run_scope"]

_SCOPED_RUN_ID: ContextVar[str | None] = ContextVar("nanny_scoped_run_id", default=None)
"""Set only while inside a ``run_scope()`` block. ``_client._run_id()`` prefers
this over ``NANNY_RUN_ID`` when present, so every existing caller that never
uses ``run_scope()`` (the CLI, every example app, GoTM's own current code) is
completely unaffected: with no scope active, resolution falls through to the
env var exactly as it always has.
"""


def _scoped_run_id() -> str | None:
    """Read the active ``run_scope()`` id, if any. Internal — used by
    ``_client._run_id()``, not part of the public API."""
    return _SCOPED_RUN_ID.get()


def fresh_run() -> str:
    """Start a new governed run in this process. Returns the new run id.

    Only meaningful when governed through a network server (``nanny run
    --serve`` / ``--join``): the server keys independent state per run id,
    so a stop or budget exhaustion in the run you just left has zero effect
    on the one you're starting. Under local ``nanny run`` (no ``--serve``),
    this is a no-op as far as governance is concerned — one local process is
    always exactly one run, the local bridge has no per-run-id state to
    switch between (confirmed directly: ``crates/bridge/src/lib.rs``'s local
    ``Bridge`` holds one shared, unkeyed state for its whole process
    lifetime) — but it's always safe to call regardless of mode, so code
    that might run under either doesn't need to branch on which one it's in.

    Without a bridge active at all (no ``NANNY_BRIDGE_ADDR``/``NANNY_BRIDGE_PORT``
    set — running the process directly, not under ``nanny run``), this still
    returns a fresh id and sets it, harmlessly: there's nothing governed to
    scope it to yet, so it has no observable effect either way.

    Sets the process-global env var, same as always — this function is for
    the sequential case ("this HTTP request finished, the next one wants a
    clean budget"). For concurrent runs sharing one process, use
    ``run_scope()`` instead.
    """
    run_id = str(uuid.uuid4())
    os.environ["NANNY_RUN_ID"] = run_id
    return run_id


@contextmanager
def run_scope(run_id: str | None = None) -> Iterator[str]:
    """Scope a governed run to the current thread/task, instead of the whole
    process.

    Use this when your process runs more than one governed run *concurrently*
    — a threaded or async web server serving many independent conversations
    at once is the motivating case. Each call gets its own run id, isolated
    via a ``ContextVar`` (correctly scoped per thread and per asyncio task),
    so two runs active at the same time in the same process never clobber
    each other's budget or stop state the way two concurrent ``fresh_run()``
    calls would (both write the same process-global env var).

    Usage::

        with nanny_sdk.run_scope() as run_id:
            ...  # every governed call in this thread/task uses this run_id

    Pass an explicit ``run_id`` to resume a specific run (e.g. reopening a
    conversation after a stop); omit it to mint a fresh one, same as
    ``fresh_run()``. On exit, the previous scope (or the plain env-var
    fallback, if none) is restored — nesting is safe.

    Every existing caller that never calls this is unaffected: with no scope
    active, ``_client._run_id()`` falls through to ``NANNY_RUN_ID`` exactly as
    it always has.
    """
    rid = run_id or str(uuid.uuid4())
    token = _SCOPED_RUN_ID.set(rid)
    try:
        yield rid
    finally:
        _SCOPED_RUN_ID.reset(token)
