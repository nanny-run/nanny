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
from nanny_sdk.exceptions import RuleDenied

F = TypeVar("F", bound=Callable[..., Any])

# ---------------------------------------------------------------------------
# Rule registry — populated at decoration time, evaluated before each tool call
# ---------------------------------------------------------------------------

# Ordered dict so rules are evaluated in registration order.
_RULES: dict[str, Callable[[PolicyContext], bool]] = {}


def tool(*, cost: int = 0) -> Callable[[F], F]:
    """Declare a Nanny-governed tool.

    Contacts the bridge before each call to enforce step, budget, timeout,
    allowlist, and rule limits. Charges ``cost`` units on each allowed call.

    In passthrough mode (no ``NANNY_BRIDGE_PORT``) the decorated function
    is returned unchanged — zero overhead, zero import errors.
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

        def _check_rules(str_args: dict[str, str]) -> None:
            """Evaluate all registered rules in registration order.

            Raises ``RuleDenied`` on the first rule that returns ``False``.
            The bridge is never contacted if a rule denies.
            """
            ctx = PolicyContext(last_tool_args=str_args, requested_tool=tool_name)
            for rule_name, rule_fn in _RULES.items():
                if not rule_fn(ctx):
                    raise RuleDenied(rule_name)

        if inspect.iscoroutinefunction(fn):

            @functools.wraps(fn)
            async def async_wrapper(*args: Any, **kwargs: Any) -> Any:
                str_args = _str_args(args, kwargs)
                _check_rules(str_args)
                _client.call_tool(tool_name, cost, str_args)
                return await fn(*args, **kwargs)

            return async_wrapper  # type: ignore[return-value]

        @functools.wraps(fn)
        def wrapper(*args: Any, **kwargs: Any) -> Any:
            str_args = _str_args(args, kwargs)
            _check_rules(str_args)
            _client.call_tool(tool_name, cost, str_args)
            return fn(*args, **kwargs)

        return wrapper  # type: ignore[return-value]

    return decorator


def rule(name: str) -> Callable[[F], F]:
    """Register a policy rule function.

    The decorated function receives a ``PolicyContext`` and returns ``bool``.
    ``False`` → ``RuleDenied(name)`` raised at the pending tool call site,
    before the bridge is ever contacted.

    Rules are evaluated in registration order. The first rule that returns
    ``False`` stops evaluation — remaining rules are not called.

    ``ctx.last_tool_args`` and ``ctx.requested_tool`` are always populated.
    ``ctx.step_count``, ``ctx.cost_units_spent``, and ``ctx.tool_call_history``
    reflect bridge-tracked state and are available via full context in v0.1.5+.
    """

    def decorator(fn: F) -> F:
        _RULES[name] = fn
        return fn

    return decorator


def agent(name: str) -> Callable[[F], F]:
    """Activate a named limit scope for the duration of the decorated function.

    Calls ``/agent/enter`` on entry and ``/agent/exit`` in a ``finally``
    block so the scope always exits even on exception. Supports both sync
    and async functions.

    ``/agent/enter`` is called **before** the ``try`` block — if the scope
    is not found (bridge returns 404), ``AgentNotFound`` propagates immediately
    and ``/agent/exit`` is never called (the scope was never activated).
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
