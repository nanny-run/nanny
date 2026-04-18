"""PolicyContext — mirrors the Rust PolicyContext struct field-for-field.

Passed to every ``@rule`` function so it can inspect agent state before
deciding whether to allow or deny the pending tool call.
"""

from __future__ import annotations

from dataclasses import dataclass, field
from typing import Any


@dataclass
class PolicyContext:
    step_count: int = 0
    elapsed_ms: int = 0
    requested_tool: str | None = None
    cost_units_spent: int = 0
    tool_call_counts: dict[str, int] = field(default_factory=dict)
    tool_call_history: list[str] = field(default_factory=list)
    last_tool_args: dict[str, str] = field(default_factory=dict)

    @classmethod
    def from_dict(cls, data: dict[str, Any]) -> PolicyContext:
        """Parse a bridge response dict into a ``PolicyContext``.

        Handles both the bridge wire format (``step``, ``cost_spent``) and the
        Python field names (``step_count``, ``cost_units_spent``) — the ``/status``
        endpoint uses the short wire names; direct dict construction uses the
        Python names.
        """
        return cls(
            # Bridge sends "step"; Python field is "step_count"
            step_count=data.get("step", data.get("step_count", 0)),
            elapsed_ms=data.get("elapsed_ms", 0),
            requested_tool=data.get("requested_tool"),
            # Bridge sends "cost_spent"; Python field is "cost_units_spent"
            cost_units_spent=data.get("cost_spent", data.get("cost_units_spent", 0)),
            tool_call_counts=data.get("tool_call_counts", {}),
            tool_call_history=data.get("tool_call_history", []),
            last_tool_args=data.get("last_tool_args", {}),
        )
