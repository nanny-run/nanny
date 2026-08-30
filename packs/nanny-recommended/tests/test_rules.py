"""Behaviour tests for the nanny:recommended corpus.

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


def test_the_pack_registers_every_rule_it_declares():
    import tomllib

    manifest = tomllib.loads((Path(__file__).resolve().parent.parent / "pack.toml").read_text())
    missing = [r for r in manifest["rules"] if r not in RULES]
    assert not missing, f"declared but not registered: {missing}"


# ── injection and taint ───────────────────────────────────────────────────────


def test_outward_action_after_untrusted_read_is_denied():
    assert not fire(
        "no_external_effect_after_untrusted_read",
        ctx(requested_tool="send_outreach", tool_call_history=["web_search"]),
    )


def test_outward_action_on_a_clean_run_is_allowed():
    assert fire(
        "no_external_effect_after_untrusted_read",
        ctx(requested_tool="send_outreach", tool_call_history=["save_findings"]),
    )


def test_an_unlabelled_tool_is_ungoverned_by_taint():
    # "declared with no labels" and "never declared" are different answers, and
    # a rule must not silently govern a tool the operator never classified.
    assert fire(
        "no_external_effect_after_untrusted_read",
        ctx(requested_tool="save_findings", tool_call_history=["web_search"]),
    )


def test_untrusted_read_after_secrets_is_denied():
    assert not fire(
        "no_untrusted_read_after_secrets",
        ctx(requested_tool="web_search", tool_call_history=["read_vault"]),
    )


def test_a_repeated_argument_less_call_is_denied():
    assert not fire(
        "no_identical_repeat_call",
        ctx(requested_tool="get_status", tool_call_history=["get_status"]),
    )


def test_a_research_loop_is_not_a_repeat_call():
    # The regression this rule shipped with: it claimed to compare arguments,
    # had none to compare, and denied every consecutive call that carried any.
    # A second search with a different query is ordinary work, not a loop.
    assert fire(
        "no_identical_repeat_call",
        ctx(
            requested_tool="web_search",
            tool_call_history=["web_search"],
            last_tool_args={"query": "a different question"},
        ),
    )


def test_credentials_in_arguments_are_denied():
    assert not fire(
        "no_secret_patterns_in_args",
        ctx(requested_tool="send_outreach", last_tool_args={"body": "key sk-abcdefghijklmnop123"}),
    )
    assert fire(
        "no_secret_patterns_in_args",
        ctx(requested_tool="send_outreach", last_tool_args={"body": "hello there"}),
    )


def test_personal_data_is_not_sent_outward():
    assert not fire(
        "no_pii_to_external_effect_tools",
        ctx(requested_tool="send_outreach", last_tool_args={"body": "ssn 123-45-6789"}),
    )
    # The same string to a tool that does not act outward is not this rule's business.
    assert fire(
        "no_pii_to_external_effect_tools",
        ctx(requested_tool="save_findings", last_tool_args={"body": "ssn 123-45-6789"}),
    )


@pytest.mark.parametrize(
    "url",
    [
        "http://localhost:8080/admin",
        "http://127.0.0.1/",
        "http://169.254.169.254/latest/meta-data/",
        "http://10.0.0.5/",
        "http://192.168.1.1/",
        "http://172.16.0.1/",
    ],
)
def test_private_network_urls_are_denied(url):
    assert not fire("no_private_network_urls", ctx(requested_tool="send_outreach", last_tool_args={"url": url}))


def test_a_public_url_is_allowed():
    assert fire(
        "no_private_network_urls",
        ctx(requested_tool="send_outreach", last_tool_args={"url": "https://example.com/x"}),
    )


def test_the_ssrf_rule_never_resolves_a_hostname(monkeypatch):
    # Determinism: resolving would make the same call allowed on Monday and
    # denied on Tuesday. Any DNS lookup here is a defect, so make one fatal.
    import socket

    def explode(*a, **k):  # pragma: no cover - only runs if the rule regresses
        raise AssertionError("a rule resolved a hostname during enforcement")

    monkeypatch.setattr(socket, "gethostbyname", explode)
    monkeypatch.setattr(socket, "getaddrinfo", explode)
    assert fire(
        "no_private_network_urls",
        ctx(requested_tool="send_outreach", last_tool_args={"url": "https://internal.example/"}),
    )


def test_path_traversal_is_denied():
    assert not fire("no_path_traversal", ctx(last_tool_args={"path": "../../etc/passwd"}))
    assert fire("no_path_traversal", ctx(last_tool_args={"path": "reports/q3.csv"}))


def test_destructive_sql_is_denied():
    assert not fire("no_sql_patterns_in_args", ctx(last_tool_args={"q": "DROP TABLE users"}))
    assert fire("no_sql_patterns_in_args", ctx(last_tool_args={"q": "SELECT id FROM users"}))


# ── destructive and financial ─────────────────────────────────────────────────


def test_irreversible_actions_need_confirmation():
    assert not fire(
        "no_destructive_without_confirmation",
        ctx(requested_tool="delete_record", tool_call_history=[]),
    )
    assert fire(
        "no_destructive_without_confirmation",
        ctx(requested_tool="delete_record", tool_call_history=["confirm_destructive"]),
    )


