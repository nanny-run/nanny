"""Public access to a run's own buffered bridge events, for a host that
wants to build its own per-tenant usage ledger instead of parsing the
CLI's flat NDJSON log file, which carries no run id and so can't be
attributed to one run/tenant among several sharing one ``--serve``
process.
"""

from __future__ import annotations

from nanny_sdk._client import get_run_events

__all__ = ["get_run_events"]
