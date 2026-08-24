"""PolicyContext parity between the Python and Rust SDKs.

Locked decision 12: Rust leads, Python mirrors. A rule is written against
whatever fields its SDK exposes, so a field that exists on one side and not
the other means the same rule text behaves differently depending on which SDK
runs it. That is the worst kind of divergence, because both sides look green.

These tests read the Rust source directly rather than restating its field list
in a second place. A test that carried its own copy of the expected fields
would pass happily while both it and Python drifted away from Rust together.
"""

from __future__ import annotations

import re
from pathlib import Path

import pytest

from nanny_sdk._context import PolicyContext

# tests/ -> python/ -> sdks/ -> nanny/
_RUST_POLICY = (
    Path(__file__).resolve().parents[3] / "crates" / "core" / "src" / "policy" / "mod.rs"
)


def _rust_policy_context_fields() -> set[str]:
    """Field names on the Rust `PolicyContext` struct."""
    src = _RUST_POLICY.read_text()
    start = src.index("pub struct PolicyContext {")
    body = src[start : src.index("\n}", start)]
    return set(re.findall(r"^\s*pub (\w+):", body, re.MULTILINE))


def _rust_policy_context_methods() -> set[str]:
    """Public method names in the `impl PolicyContext` block."""
    src = _RUST_POLICY.read_text()
    start = src.index("impl PolicyContext {")
    body = src[start : src.index("\n}\n", start)]
    return set(re.findall(r"^\s*pub fn (\w+)\(", body, re.MULTILINE))


@pytest.mark.skipif(not _RUST_POLICY.exists(), reason="Rust source not present")
def test_field_names_match_rust_exactly() -> None:
    rust = _rust_policy_context_fields()
    python = set(PolicyContext.__dataclass_fields__)

    assert rust, "the Rust struct must have been parsed; the parser may have drifted"
    assert python == rust, (
        "PolicyContext fields diverged.\n"
        f"  only in Rust:   {sorted(rust - python)}\n"
        f"  only in Python: {sorted(python - rust)}"
    )


@pytest.mark.skipif(not _RUST_POLICY.exists(), reason="Rust source not present")
def test_label_helpers_match_rust_exactly() -> None:
    """`tool_has` and `tools_with` must exist on both sides under the same names.

    Rules reference these directly, so a name that differs is a rule that does
    not port.
    """
    rust = _rust_policy_context_methods()
    python = {
        name
        for name in vars(PolicyContext)
        if callable(getattr(PolicyContext, name)) and not name.startswith("_")
    } - {"from_dict"}

    assert rust, "the impl block must have been parsed; the parser may have drifted"
    assert python == rust, (
        "PolicyContext helpers diverged.\n"
        f"  only in Rust:   {sorted(rust - python)}\n"
        f"  only in Python: {sorted(python - rust)}"
    )


def test_removed_fields_are_absent() -> None:
    """The consumption fields are gone from the context, not merely unused.

    A field that still parses would let a rule read `ctx.step_count` and get a
    plausible zero forever.
    """
    fields = set(PolicyContext.__dataclass_fields__)
    for gone in ("step_count", "next_tool_tokens"):
        assert gone not in fields, f"{gone} must not be on PolicyContext"


def test_from_dict_ignores_a_field_the_bridge_no_longer_sends() -> None:
    """An older governor still sending `step` must not break parsing.

    Nanny fails closed on what it governs, but a stale extra key in a status
    response is not a policy question: refusing to parse it would stop runs
    over a field nothing reads.
    """
    ctx = PolicyContext.from_dict({"step": 7, "tokens_spent": 3, "tool_labels": {}})

    assert ctx.tokens_spent == 3
    assert not hasattr(ctx, "step")
