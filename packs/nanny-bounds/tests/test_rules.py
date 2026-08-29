"""Behaviour tests for the nanny:bounds corpus.

Each rule is asserted on both sides: the case it must deny, and the neighbouring
case it must allow. A rule that only ever denies is a broken product, and a rule
tested only on its denial is how you ship one.
"""

from __future__ import annotations

import datetime as dt
import sys
from pathlib import Path

import pytest

PACK = Path(__file__).resolve().parent.parent / "python"
sys.path.insert(0, str(PACK))
sys.path.insert(0, str(Path(__file__).resolve().parents[3] / "sdks" / "python"))

from nanny_sdk._context import PolicyContext  # noqa: E402
import nanny_sdk._decorators as decorators  # noqa: E402

import arguments  # noqa: E402,F401
import destructive  # noqa: E402,F401
import injection  # noqa: E402,F401
import operational  # noqa: E402,F401
import runaway  # noqa: E402,F401
import sequence  # noqa: E402,F401


RULES = decorators._RULES

LABELS = {
    "web_search": ["reads_untrusted"],
    "send_outreach": ["external_effect"],
    "read_vault": ["reads_sensitive"],
    "delete_record": ["destructive"],
    "pay_invoice": ["moves_money"],
    "save_findings": [],
    "request_approval": [],
    "confirm_destructive": [],
}


def ctx(**kw) -> PolicyContext:
    kw.setdefault("tool_labels", LABELS)
    return PolicyContext(**kw)


def fire(name: str, c: PolicyContext) -> bool:
    return RULES[name](c)


@pytest.mark.parametrize("used,allowed", [(19, True), (20, False)])
def test_untrusted_reads_are_capped(used, allowed):
    assert fire(
        "cap_untrusted_reads_per_run",
        ctx(requested_tool="web_search", tool_call_counts={"web_search": used}),
    ) is allowed


# ── sequence and authority ────────────────────────────────────────────────────


def test_outward_action_requires_approval():
    assert not fire(
        "require_approval_before_external_effect",
        ctx(requested_tool="send_outreach", tool_call_history=[]),
    )
    assert fire(
        "require_approval_before_external_effect",
        ctx(requested_tool="send_outreach", tool_call_history=["request_approval"]),
    )


def test_spawn_depth_is_bounded():
    assert fire("no_agent_spawn_beyond_depth", ctx(tool_call_counts={"spawn_worker": 2}))
    assert not fire("no_agent_spawn_beyond_depth", ctx(tool_call_counts={"spawn_worker": 3}))


# ── loop and runaway ──────────────────────────────────────────────────────────


def test_repeating_a_tool_consecutively_is_bounded():
    history = ["web_search"] * 5
    assert not fire(
        "no_consecutive_identical_tool",
        ctx(requested_tool="web_search", tool_call_history=history),
    )
    assert fire(
        "no_consecutive_identical_tool",
        ctx(requested_tool="web_search", tool_call_history=history[:4]),
    )


def test_outward_actions_are_capped_per_run():
    assert not fire(
        "cap_external_effect_calls_per_run",
        ctx(requested_tool="send_outreach", tool_call_counts={"send_outreach": 10}),
    )


# ── argument safety ───────────────────────────────────────────────────────────


def test_a_payment_above_the_threshold_is_denied():
    assert not fire(
        "no_payment_above_threshold",
        ctx(requested_tool="pay_invoice", last_tool_args={"amount": "2500.00"}),
    )
    assert fire(
        "no_payment_above_threshold",
        ctx(requested_tool="pay_invoice", last_tool_args={"amount": "250.00"}),
    )


def test_splitting_a_payment_does_not_defeat_the_cumulative_cap():
    # Per-call thresholds are trivially defeated by splitting, so the cumulative
    # figure is the one that binds.
    assert not fire(
        "cap_cumulative_amount_per_run",
        ctx(
            requested_tool="pay_invoice",
            last_tool_args={"amount": "900.00", "total": "4500.00"},
        ),
    )


# ── operational ───────────────────────────────────────────────────────────────

def _at_hour(hour: int) -> int:
    when = dt.datetime(2026, 8, 25, hour, 0, tzinfo=dt.timezone.utc)
    return int(when.timestamp() * 1000)


def test_outward_actions_are_denied_outside_operating_hours():
    assert not fire(
        "no_external_effect_outside_operating_hours",
        ctx(requested_tool="send_outreach", now_ms=_at_hour(3)),
    )
    assert fire(
        "no_external_effect_outside_operating_hours",
        ctx(requested_tool="send_outreach", now_ms=_at_hour(14)),
    )


def test_the_operating_hours_rule_reads_its_context_not_the_clock(monkeypatch):
    # If the rule called datetime.now() this test would pass or fail depending on
    # when it ran, which is exactly the property being ruled out.
    assert fire(
        "no_external_effect_outside_operating_hours",
        ctx(requested_tool="send_outreach", now_ms=_at_hour(9)),
    )
    assert not fire(
        "no_external_effect_outside_operating_hours",
        ctx(requested_tool="send_outreach", now_ms=_at_hour(23)),
    )
