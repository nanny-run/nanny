"""``@tool``, ``@rule``, ``@agent`` decorators.

Day 1: skeletons that work in passthrough mode.
Day 2: ``@tool`` bridge integration.
Day 3: ``@rule`` client-side rule evaluation.
Day 4: ``@agent`` scope enter/exit.
"""

from __future__ import annotations

import functools
import inspect
from collections.abc import Callable
from typing import Any, TypeVar

from nanny_sdk import _client
from nanny_sdk._context import PolicyContext
from nanny_sdk.exceptions import BridgeUnavailable, RuleDenied

F = TypeVar("F", bound=Callable[..., Any])

# ---------------------------------------------------------------------------
# Rule registry: populated at decoration time, evaluated before each tool call
# ---------------------------------------------------------------------------

# Ordered dict so rules are evaluated in registration order.
_RULES: dict[str, Callable[[PolicyContext], bool]] = {}

_declared = False


def _declare_rules_once() -> None:
    """Declare registered rules on the first governed call.

    Deferred to first use rather than done at import, because packs and local
    rules are still registering while the module graph loads. Declaring early
    would record a set smaller than the one that actually runs.
    """
    global _declared
    if _declared:
        return
    _declared = True
    from nanny_sdk.packs import declare_all

    _client.declare_rules(declare_all())


def tool() -> Callable[[F], F]:
    """Declare a Nanny-governed tool.

    Contacts the bridge before each call to enforce the tool allowlist,
    per-tool call caps, and rules.

    In passthrough mode (no ``NANNY_BRIDGE_PORT``) the decorated function
    is returned unchanged, zero overhead, zero import errors.
    """

    def decorator(fn: F) -> F:
        if _client.is_passthrough():
            return fn

        tool_name = fn.__name__
        sig = inspect.signature(fn)

        def _str_args(args: tuple[Any, ...], kwargs: dict[str, Any]) -> dict[str, str]:
            """Bind call-site args to parameter names and stringify the values."""
            bound = sig.bind(*args, **kwargs)
            bound.apply_defaults()
            return {k: str(v) for k, v in bound.arguments.items()}

        def _check_rules(str_args: dict[str, str]) -> list[str]:
            """Evaluate all registered rules in registration order.

            Fetches live counters from ``GET /status`` first so rules have
            access to ``tool_call_history``, ``tool_labels``, etc.

            If the bridge is unreachable, raises ``BridgeUnavailable``:
            silently continuing with zeroed counters would let the agent run
            ungoverned, violating the manifesto guarantee that Nanny fails
            closed.

            Raises ``RuleDenied`` on the first rule that returns ``False``.
            ``/tool/call`` is never reached if a rule denies.
            """
            _declare_rules_once()
            try:
                ctx = _client.get_status()
            except Exception:
                _client.report_stop("BridgeUnavailable")
                raise BridgeUnavailable()
            ctx.last_tool_args = str_args
            ctx.requested_tool = tool_name
            cleared: list[str] = []
            for rule_name, rule_fn in _RULES.items():
                if not rule_fn(ctx):
                    _client.report_stop_rule(tool_name, rule_name, cleared)
                    raise RuleDenied(rule_name)
                cleared.append(rule_name)
            return cleared

        if inspect.iscoroutinefunction(fn):

            @functools.wraps(fn)
            async def async_wrapper(*args: Any, **kwargs: Any) -> Any:
                str_args = _str_args(args, kwargs)
                cleared = _check_rules(str_args)
                _client.call_tool(tool_name, str_args, cleared)
                return await fn(*args, **kwargs)

            return async_wrapper  # type: ignore[return-value]

        @functools.wraps(fn)
        def wrapper(*args: Any, **kwargs: Any) -> Any:
            str_args = _str_args(args, kwargs)
            cleared = _check_rules(str_args)
            _client.call_tool(tool_name, str_args, cleared)
            return fn(*args, **kwargs)

        return wrapper  # type: ignore[return-value]

    return decorator


def rule(name: str) -> Callable[[F], F]:
    """Register a policy rule function.

    The decorated function receives a ``PolicyContext`` and returns ``bool``.
    ``False`` raises ``RuleDenied(name)`` at the pending tool call site,
    before the bridge is ever contacted.

    Rules are evaluated in registration order. The first rule that returns
    ``False`` stops evaluation, remaining rules are not called.

    ``ctx.last_tool_args`` and ``ctx.requested_tool`` are always populated.
    ``ctx.tool_labels``, ``ctx.tokens_spent``, and ``ctx.tool_call_history``
    reflect bridge-tracked state and are available via full context in v0.1.5+.
    """

    def decorator(fn: F) -> F:
        _RULES[name] = fn
        return fn

    return decorator


def agent(name: str) -> Callable[[F], F]:
    """Name a phase of the run for the duration of the decorated function.

    A scope does not change what the agent may do; it labels which phase each
    verdict belongs to, so the audit log can attribute one. Calls
    ``/agent/enter`` on entry and ``/agent/exit`` in a ``finally`` block so the
    scope always exits even on exception. Supports both sync and async
    functions.

    ``/agent/enter`` is called **before** the ``try`` block, so a transport
    failure there leaves no unmatched ``/agent/exit`` behind.
    """

    def decorator(fn: F) -> F:
        if _client.is_passthrough():
            return fn

        if inspect.iscoroutinefunction(fn):

            @functools.wraps(fn)
            async def async_wrapper(*args: Any, **kwargs: Any) -> Any:
                _client.agent_enter(name)
                try:
                    return await fn(*args, **kwargs)
                finally:
                    _client.agent_exit(name)

            return async_wrapper  # type: ignore[return-value]

        @functools.wraps(fn)
        def wrapper(*args: Any, **kwargs: Any) -> Any:
            _client.agent_enter(name)
            try:
                return fn(*args, **kwargs)
            finally:
                _client.agent_exit(name)

        return wrapper  # type: ignore[return-value]

    return decorator
