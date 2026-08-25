"""PolicyContext: mirrors the Rust PolicyContext struct field-for-field.

Passed to every ``@rule`` function so it can inspect agent state before
deciding whether to allow or deny the pending tool call.
"""

from __future__ import annotations

from dataclasses import dataclass, field
from typing import Any


@dataclass
class PolicyContext:
    elapsed_ms: int = 0
    now_ms: int = 0
    """Wall-clock at evaluation, milliseconds since the Unix epoch.

    Present so a rule about *when* an action is permitted stays a pure function
    of its context. A rule calling ``datetime.now()`` would reach outside its
    inputs to decide, which is untestable and breaks the guarantee that
    identical inputs produce identical behaviour.
    """
    requested_tool: str | None = None
    tokens_spent: int = 0
    tool_call_counts: dict[str, int] = field(default_factory=dict)
    tool_call_history: list[str] = field(default_factory=list)
    tool_labels: dict[str, list[str]] = field(default_factory=dict)
    last_tool_args: dict[str, str] = field(default_factory=dict)

    @classmethod
    def from_dict(cls, data: dict[str, Any]) -> PolicyContext:
        """Parse a bridge response dict into a ``PolicyContext``.

        The ``/status`` endpoint is the only source of these values for an
        out-of-process SDK: it never reads nanny.toml, only the governor does.
        """
        return cls(
            elapsed_ms=data.get("elapsed_ms", 0),
            now_ms=data.get("now_ms", 0),
            requested_tool=data.get("requested_tool"),
            tokens_spent=data.get("tokens_spent", 0),
            tool_call_counts=data.get("tool_call_counts", {}),
            tool_call_history=data.get("tool_call_history", []),
            tool_labels=data.get("tool_labels", {}),
            last_tool_args=data.get("last_tool_args", {}),
        )

    def tool_has(self, tool: str, label: str) -> bool:
        """Does ``tool`` carry ``label``?

        The way rules are meant to read labels. Returns False for an unknown
        tool and for an unknown label, which is the correct default in both
        directions: a rule asking about a tool the operator never declared
        should not fire, and a rule asking about a misspelled label must not
        silently match everything.

        Rules that reference labels instead of tool names can govern any app
        whose operator has labelled their tools, which is what makes a shared
        rule corpus possible::

            @rule("no_external_effect_after_untrusted_read")
            def taint(ctx: PolicyContext) -> bool:
                pending = ctx.requested_tool
                if pending is None or not ctx.tool_has(pending, "external_effect"):
                    return True
                return not any(
                    ctx.tool_has(t, "reads_untrusted") for t in ctx.tool_call_history
                )
        """
        return label in self.tool_labels.get(tool, ())

    def tools_with(self, label: str) -> list[str]:
        """Every tool in the allowlist carrying ``label``.

        For rules that need the set rather than a yes/no on one tool, e.g.
        "cap the total calls across all money-moving tools". Sorted, so a rule
        built on it behaves identically run to run.
        """
        return sorted(name for name, labels in self.tool_labels.items() if label in labels)
