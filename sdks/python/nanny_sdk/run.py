"""nanny_sdk.fresh_run — start a new governed run within the current process.

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
"""

from __future__ import annotations

import os
import uuid

__all__ = ["fresh_run"]


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
    """
    run_id = str(uuid.uuid4())
    os.environ["NANNY_RUN_ID"] = run_id
    return run_id
