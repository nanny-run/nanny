"""Day 3 — @rule decorator tests."""

import pytest
from pytest_httpserver import HTTPServer

from nanny_sdk import BridgeUnavailable, RuleDenied, rule, tool
from nanny_sdk._context import PolicyContext

# ---------------------------------------------------------------------------
# Helpers
# ---------------------------------------------------------------------------


def _allow() -> dict[str, str]:
    return {"status": "allowed"}


# ---------------------------------------------------------------------------
# Allow path — rule passes, bridge proceeds
# ---------------------------------------------------------------------------


def test_rule_allow_bridge_called(mock_bridge: HTTPServer) -> None:
    """Rule returning True: bridge is still called and function executes."""
    mock_bridge.expect_request("/tool/call", method="POST").respond_with_json(_allow())

    @rule("allow_all")
    def allow_all(ctx: PolicyContext) -> bool:
        return True

    @tool(tokens=10)
    def my_func() -> str:
        return "result"

    assert my_func() == "result"
    mock_bridge.check_assertions()


# ---------------------------------------------------------------------------
# Deny path — rule fires before bridge
# ---------------------------------------------------------------------------


def test_rule_deny_raises_rule_denied(mock_bridge: HTTPServer) -> None:
    """Rule returning False raises RuleDenied with the correct rule name."""

    @rule("no_everything")
    def no_everything(ctx: PolicyContext) -> bool:
        return False

    @tool(tokens=10)
    def my_func() -> str:
        return "result"

    with pytest.raises(RuleDenied) as exc_info:
        my_func()
    assert exc_info.value.rule_name == "no_everything"


def test_rule_deny_tool_call_never_made(mock_bridge: HTTPServer) -> None:
    """When a rule denies, /tool/call is never reached.

    /status is contacted to populate PolicyContext (and silently falls back
    to zeroed counters if the mock returns 500 for it), but /tool/call must
    never be registered or called.
    """

    @rule("always_deny")
    def always_deny(ctx: PolicyContext) -> bool:
        return False

    @tool(tokens=10)
    def my_func() -> str:
        return "result"

    with pytest.raises(RuleDenied):
        my_func()

    # No /tool/call handler registered — check_assertions() confirms it was
    # never expected (and therefore never reached).
    mock_bridge.check_assertions()


def test_rule_deny_function_body_never_runs(mock_bridge: HTTPServer) -> None:
    """When a rule denies, the wrapped function body must not execute."""
    executed = False

    @rule("deny_rule")
    def deny_rule(ctx: PolicyContext) -> bool:
        return False

    @tool(tokens=10)
    def my_func() -> str:
        nonlocal executed
        executed = True
        return "result"

    with pytest.raises(RuleDenied):
        my_func()
    assert not executed


# ---------------------------------------------------------------------------
# PolicyContext contents
# ---------------------------------------------------------------------------


def test_rule_ctx_last_tool_args(mock_bridge: HTTPServer) -> None:
    """ctx.last_tool_args contains the tool's call arguments."""
    captured: list[PolicyContext] = []
    mock_bridge.expect_request("/tool/call").respond_with_json(_allow())

    @rule("capture")
    def capture(ctx: PolicyContext) -> bool:
        captured.append(ctx)
        return True

    @tool(tokens=10)
    def read_file(path: str) -> str:
        return ""

    read_file("src/main.rs")
    assert captured[0].last_tool_args == {"path": "src/main.rs"}


def test_rule_ctx_requested_tool(mock_bridge: HTTPServer) -> None:
    """ctx.requested_tool is set to the decorated function's name."""
    captured: list[PolicyContext] = []
    mock_bridge.expect_request("/tool/call").respond_with_json(_allow())

    @rule("capture")
    def capture(ctx: PolicyContext) -> bool:
        captured.append(ctx)
        return True

    @tool(tokens=10)
    def search_web(query: str) -> str:
        return ""

    search_web("rust http clients")
    assert captured[0].requested_tool == "search_web"


def test_rule_ctx_bridge_fields_populated_from_status(mock_bridge: HTTPServer) -> None:
    """Bridge-tracked fields are populated from GET /status before rules run.

    Uses ``expect_oneshot_request`` so this custom response takes priority over
    the fixture's permanent zeroed-counter catch-all.
    """
    captured: list[PolicyContext] = []
    mock_bridge.expect_oneshot_request("/status", method="GET").respond_with_json({
        "state": "running",
        "tokens_spent": 70,
        "elapsed_ms": 3500,
        "tool_call_counts": {"file_reader": 7},
        "tool_call_history": ["file_reader"] * 7,
        "tool_labels": {"file_reader": ["reads_untrusted"]},
    })
    mock_bridge.expect_request("/tool/call", method="POST").respond_with_json(_allow())

    @rule("capture")
    def capture(ctx: PolicyContext) -> bool:
        captured.append(ctx)
        return True

    @tool(tokens=10)
    def file_reader(path: str) -> str:
        return ""

    file_reader("src/main.rs")
    ctx = captured[0]
    # Bridge-tracked counters come from /status
    assert ctx.tokens_spent == 70
    assert ctx.elapsed_ms == 3500
    assert ctx.tool_call_counts == {"file_reader": 7}
    assert ctx.tool_call_history == ["file_reader"] * 7
    assert ctx.tool_labels == {"file_reader": ["reads_untrusted"]}
    # These are always set by the decorator, not /status
    assert ctx.requested_tool == "file_reader"
    assert ctx.last_tool_args == {"path": "src/main.rs"}
    mock_bridge.check_assertions()


def test_rule_ctx_status_failure_fails_closed(mock_bridge: HTTPServer) -> None:
    """If GET /status fails, the tool call is blocked and BridgeUnavailable is raised.

    Silently continuing with zeroed counters would let the agent run ungoverned —
    a manifesto violation. The SDK must fail closed: bridge unreachable = stop.
    """
    # Override the default /status catch-all with a 500 response.
    # Oneshot handlers take priority over persistent handlers in pytest-httpserver.
    mock_bridge.expect_oneshot_request("/status", method="GET").respond_with_data(
        "internal error", status=500, content_type="text/plain"
    )
    # /tool/call should never be reached — no handler registered

    @rule("should_not_run")
    def should_not_run(ctx: PolicyContext) -> bool:  # pragma: no cover
        return True

    @tool(tokens=0)
    def my_func() -> str:  # pragma: no cover
        return "ok"

    with pytest.raises(BridgeUnavailable):
        my_func()


# ---------------------------------------------------------------------------
# Multiple rules
# ---------------------------------------------------------------------------


def test_multiple_rules_all_evaluated_when_passing(mock_bridge: HTTPServer) -> None:
    """All registered rules are called when all return True."""
    call_log: list[str] = []
    mock_bridge.expect_request("/tool/call").respond_with_json(_allow())

    @rule("rule_a")
    def rule_a(ctx: PolicyContext) -> bool:
        call_log.append("a")
        return True

    @rule("rule_b")
    def rule_b(ctx: PolicyContext) -> bool:
        call_log.append("b")
        return True

    @tool(tokens=10)
    def my_func() -> str:
        return "ok"

    my_func()
    assert set(call_log) == {"a", "b"}


def test_multiple_rules_first_deny_stops_evaluation(mock_bridge: HTTPServer) -> None:
    """Once a rule denies, remaining rules are not evaluated."""
    call_log: list[str] = []

    @rule("deny_first")
    def deny_first(ctx: PolicyContext) -> bool:
        call_log.append("first")
        return False

    @rule("should_not_run")
    def should_not_run(ctx: PolicyContext) -> bool:
        call_log.append("second")
        return True

    @tool(tokens=10)
    def my_func() -> str:
        return "ok"

    with pytest.raises(RuleDenied) as exc_info:
        my_func()

    assert call_log == ["first"]
    assert exc_info.value.rule_name == "deny_first"


def test_rules_evaluated_in_registration_order(mock_bridge: HTTPServer) -> None:
    """Rules are evaluated in the order they were registered."""
    call_log: list[str] = []
    mock_bridge.expect_request("/tool/call").respond_with_json(_allow())

    @rule("first")
    def first(ctx: PolicyContext) -> bool:
        call_log.append("first")
        return True

    @rule("second")
    def second(ctx: PolicyContext) -> bool:
        call_log.append("second")
        return True

    @rule("third")
    def third(ctx: PolicyContext) -> bool:
        call_log.append("third")
        return True

    @tool(tokens=10)
    def my_func() -> str:
        return "ok"

    my_func()
    assert call_log == ["first", "second", "third"]


# ---------------------------------------------------------------------------
# Passthrough — rules not evaluated
# ---------------------------------------------------------------------------


# ---------------------------------------------------------------------------
# /stop payload on rule denial — tool name and rule name must be sent
# ---------------------------------------------------------------------------


def test_rule_deny_stop_payload_contains_tool_and_rule_name(mock_bridge: HTTPServer) -> None:
    """When a rule fires, /stop is called with tool and rule_name in the payload.

    The bridge needs both fields to emit the RuleDenied NDJSON event — a bare
    {"reason":"RuleDenied"} payload leaves the bridge unable to populate the event.
    """
    mock_bridge.expect_oneshot_request(
        "/stop",
        method="POST",
        json={"reason": "RuleDenied", "tool": "read_file", "rule_name": "block_dotenv"},
    ).respond_with_json({"status": "ok"})

    @rule("block_dotenv")
    def block_dotenv(ctx: PolicyContext) -> bool:
        return False

    @tool(tokens=5)
    def read_file(path: str) -> str:
        return ""

    with pytest.raises(RuleDenied):
        read_file(".env")

    mock_bridge.check_assertions()


def test_rule_deny_stop_payload_uses_decorated_function_name(mock_bridge: HTTPServer) -> None:
    """The tool name in the /stop payload matches the decorated function's name."""
    captured_bodies: list[dict] = []

    def capture_stop(request):  # type: ignore[no-untyped-def]
        import json
        captured_bodies.append(json.loads(request.data))
        from werkzeug.wrappers import Response
        return Response('{"status":"ok"}', content_type="application/json")

    mock_bridge.expect_oneshot_request("/stop", method="POST").respond_with_handler(capture_stop)

    @rule("deny_all")
    def deny_all(ctx: PolicyContext) -> bool:
        return False

    @tool(tokens=5)
    def fetch_url(url: str) -> str:
        return ""

    with pytest.raises(RuleDenied):
        fetch_url("https://example.com")

    assert len(captured_bodies) == 1
    body = captured_bodies[0]
    assert body["reason"] == "RuleDenied"
    assert body["tool"] == "fetch_url"
    assert body["rule_name"] == "deny_all"


def test_passthrough_rules_not_evaluated(monkeypatch: pytest.MonkeyPatch) -> None:
    """In passthrough mode, rule functions are never called."""
    evaluated = False

    @rule("would_deny")
    def would_deny(ctx: PolicyContext) -> bool:
        nonlocal evaluated
        evaluated = True
        return False

    monkeypatch.delenv("NANNY_BRIDGE_PORT", raising=False)

    @tool(tokens=10)
    def my_func() -> str:
        return "direct"

    assert my_func() == "direct"
    assert not evaluated


# ---------------------------------------------------------------------------
# Tool labels — rules that name no tool
# ---------------------------------------------------------------------------


def test_a_label_driven_rule_denies_using_history(mock_bridge: HTTPServer) -> None:
    """The end-to-end path for tool classification, mirroring the Rust matrix.

    Labels declared in nanny.toml reach a rule through /status, and a rule that
    names no tool at all still denies. Without this, labels are decoration.
    """
    mock_bridge.expect_oneshot_request("/status", method="GET").respond_with_json({
        "state": "running",
        "tokens_spent": 0,
        "elapsed_ms": 0,
        "tool_call_counts": {"web_search": 1},
        "tool_call_history": ["web_search"],
        "tool_labels": {
            "web_search": ["reads_untrusted"],
            "send_outreach": ["external_effect"],
        },
    })

    @rule("no_external_effect_after_untrusted_read")
    def taint(ctx: PolicyContext) -> bool:
        pending = ctx.requested_tool
        if pending is None or not ctx.tool_has(pending, "external_effect"):
            return True
        return not any(ctx.tool_has(t, "reads_untrusted") for t in ctx.tool_call_history)

    @tool(tokens=10)
    def send_outreach() -> str:
        return "sent"

    with pytest.raises(RuleDenied) as exc_info:
        send_outreach()
    assert exc_info.value.rule_name == "no_external_effect_after_untrusted_read"


def test_the_same_rule_allows_when_the_tools_are_unlabelled(mock_bridge: HTTPServer) -> None:
    """Proves the denial came from the labels, not from the tool names."""
    mock_bridge.expect_oneshot_request("/status", method="GET").respond_with_json({
        "state": "running",
        "tokens_spent": 0,
        "elapsed_ms": 0,
        "tool_call_counts": {"web_search": 1},
        "tool_call_history": ["web_search"],
        "tool_labels": {"web_search": [], "send_outreach": []},
    })
    mock_bridge.expect_request("/tool/call", method="POST").respond_with_json(_allow())

    @rule("no_external_effect_after_untrusted_read_2")
    def taint(ctx: PolicyContext) -> bool:
        pending = ctx.requested_tool
        if pending is None or not ctx.tool_has(pending, "external_effect"):
            return True
        return not any(ctx.tool_has(t, "reads_untrusted") for t in ctx.tool_call_history)

    @tool(tokens=10)
    def send_outreach() -> str:
        return "sent"

    assert send_outreach() == "sent"


def test_tool_has_defaults_are_false_in_both_directions() -> None:
    """An unknown tool and an unknown label both answer False.

    A rule asking about a tool the operator never declared must not fire, and a
    rule asking about a misspelled label must not silently match everything.
    That second direction is the one that would fail open.
    """
    ctx = PolicyContext(tool_labels={"web_search": ["reads_untrusted"], "save": []})

    assert ctx.tool_has("web_search", "reads_untrusted")
    assert not ctx.tool_has("web_search", "moves_money")
    assert not ctx.tool_has("save", "external_effect"), "declared but unlabelled"
    assert not ctx.tool_has("ghost", "reads_untrusted"), "never declared"
    assert not ctx.tool_has("web_search", "reads_untrused"), "misspelled label"
    assert not PolicyContext().tool_has("anything", "destructive"), "no labels at all"


def test_tools_with_is_sorted() -> None:
    """Sorted, not dict order, so a rule built on it behaves the same each run."""
    ctx = PolicyContext(
        tool_labels={
            "zeta": ["moves_money"],
            "alpha": ["moves_money"],
            "mid": ["destructive"],
        }
    )

    assert ctx.tools_with("moves_money") == ["alpha", "zeta"]
    assert ctx.tools_with("nonexistent") == []


# ── Rule attribution ──────────────────────────────────────────────────────────


def test_an_allowed_call_reports_the_rules_that_cleared_it(monkeypatch, mock_bridge):
    """A rule that ran clean must leave evidence it operated.

    The engine is required to log every verdict, allow and refuse alike, and a
    rule returning allow is a verdict. Without this, a control that ran clean
    nine thousand times is indistinguishable from one never reached.
    """
    from nanny_sdk import _client, _decorators

    _decorators._RULES.clear()
    calls: dict[str, object] = {}

    @_decorators.rule("first_rule")
    def first(ctx):
        return True

    @_decorators.rule("second_rule")
    def second(ctx):
        return True

    monkeypatch.setattr(_client, "get_status", lambda: PolicyContext())
    monkeypatch.setattr(
        _client,
        "call_tool",
        lambda tool, tokens, args, cleared_by=None: calls.update(cleared=cleared_by),
    )

    @_decorators.tool()
    def send_outreach(to: str) -> str:
        return "sent"

    send_outreach("a@b.c")
    assert calls["cleared"] == ["first_rule", "second_rule"], "in evaluation order"


def test_a_denial_reports_only_the_rules_that_ran_before_it(monkeypatch, mock_bridge):
    """Evaluation short-circuits, so rules after the denier never ran.

    Listing them would claim a control operated when it did not, which is the
    one thing a compliance log must never do.
    """
    from nanny_sdk import _client, _decorators
    from nanny_sdk.exceptions import RuleDenied

    _decorators._RULES.clear()
    reported: dict[str, object] = {}

    @_decorators.rule("ran_first")
    def a(ctx):
        return True

    @_decorators.rule("denies")
    def b(ctx):
        return False

    @_decorators.rule("never_reached")
    def c(ctx):
        raise AssertionError("a rule after the denier must not be evaluated")

    monkeypatch.setattr(_client, "get_status", lambda: PolicyContext())
    monkeypatch.setattr(
        _client,
        "report_stop_rule",
        lambda tool, rule, cleared_by=None: reported.update(rule=rule, cleared=cleared_by),
    )
    monkeypatch.setattr(_client, "call_tool", lambda *a, **k: None)

    @_decorators.tool()
    def send_outreach(to: str) -> str:
        return "sent"

    with pytest.raises(RuleDenied):
        send_outreach("a@b.c")

    assert reported["rule"] == "denies"
    assert reported["cleared"] == ["ran_first"]


def test_wall_clock_reaches_rules_from_the_bridge():
    """``now_ms`` is an input, so a time rule stays a pure function."""
    ctx = PolicyContext.from_dict({"now_ms": 1_756_100_000_000})
    assert ctx.now_ms == 1_756_100_000_000
    assert PolicyContext.from_dict({}).now_ms == 0
