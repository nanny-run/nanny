"""nanny_sdk.run_scope: scope a governed run to the current thread or task.

Usage::

    import nanny_sdk

    with nanny_sdk.run_scope() as run_id:
        ...  # every governed call in this thread/task belongs to run_id

Why this exists: a run is Nanny's real unit of governance, one stop state,
one tool call history, "a stop is final." A named ``@agent(...)`` scope does
*not* give you a second one of these; it labels a phase within the current
one so the audit log can attribute each verdict. If your process runs several
logically independent runs, one long-lived server giving each incoming
request its own clean slate, this is how you say that.

The scope is a ``ContextVar``, isolated per thread and per asyncio task, so
two runs in flight at once never clobber each other. That matters more than
it used to: rules read ``tool_call_history``, so a leaked run id means one
tenant's untrusted read poisons another tenant's history. Under the authority
reframe that is a wrong security verdict, not a wrong number.

This replaces ``fresh_run()``, which wrote ``NANNY_RUN_ID`` into
``os.environ``, a process-global write that raced under any threaded host.
Mirrors ``nanny::run_scope`` on the Rust side.
"""

from __future__ import annotations

import uuid
from collections.abc import Iterator
from contextlib import contextmanager
from contextvars import ContextVar

__all__ = ["new_run_id", "run_scope"]

_SCOPED_RUN_ID: ContextVar[str | None] = ContextVar("nanny_scoped_run_id", default=None)


def _scoped_run_id() -> str | None:
    """The active `run_scope()` id, if any. Used by `_client._run_id()`."""
    return _SCOPED_RUN_ID.get()


#: Prefix on every run id, so an id is recognisable as one on sight. Matches the
#: shape ``app_`` already uses: a type prefix, then 32 hex characters, no dashes.
RUN_ID_PREFIX = "run_"


def new_run_id() -> str:
    """Mint a run id: ``run_`` plus 32 hex characters, 128 random bits.

    Mirrors ``nanny_config::new_run_id`` on the Rust side, and the two must not
    drift: a governor and the SDKs joining it write ids into the same log.

    Uniformly random on purpose, not time-ordered. A leading timestamp would
    make every short prefix identical for runs in the same millisecond, and a
    short id exists precisely so it can be read, typed and looked up, the way a
    short commit hash is.
    """
    return f"{RUN_ID_PREFIX}{uuid.uuid4().hex}"


@contextmanager
def run_scope(run_id: str | None = None) -> Iterator[str]:
    """Scope a governed run to the current thread or asyncio task, not the
    whole process.

    Each call's run id is isolated via a `ContextVar`, so two runs in flight
    at once never clobber each other's stop state or history. A threaded or
    async host serving several independent conversations gets one run each.

    Usage::

        with nanny_sdk.run_scope() as run_id:
            ...  # every governed call in this thread/task uses this run_id

    Pass an explicit `run_id` to resume a specific run; omit it to mint a
    fresh one. On exit, the previous scope (or the env-var fallback, if none)
    is restored, so nesting is safe.

    With no scope ever entered, `_client._run_id()` falls through to
    `NANNY_RUN_ID` exactly as before this existed.
    """
    rid = run_id or new_run_id()
    token = _SCOPED_RUN_ID.set(rid)
    try:
        yield rid
    finally:
        _SCOPED_RUN_ID.reset(token)
