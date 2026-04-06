"""``@tool``, ``@rule``, ``@agent`` decorators.

Day 1: skeletons that work in passthrough mode.
Day 2: ``@tool`` bridge integration.
Day 3: ``@rule`` bridge registration.
Day 4: ``@agent`` scope enter/exit.
"""

from __future__ import annotations

import functools
import inspect
from collections.abc import Callable
from typing import Any, TypeVar

from nanny_sdk import _client

F = TypeVar("F", bound=Callable[..., Any])


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

        if inspect.iscoroutinefunction(fn):

            @functools.wraps(fn)
            async def async_wrapper(*args: Any, **kwargs: Any) -> Any:
                _client.call_tool(tool_name, cost, _str_args(args, kwargs))
                return await fn(*args, **kwargs)

            return async_wrapper  # type: ignore[return-value]

        @functools.wraps(fn)
        def wrapper(*args: Any, **kwargs: Any) -> Any:
            _client.call_tool(tool_name, cost, _str_args(args, kwargs))
            return fn(*args, **kwargs)

        return wrapper  # type: ignore[return-value]

    return decorator


def rule(name: str) -> Callable[[F], F]:
    """Register a policy rule function with the bridge.

    The decorated function receives a ``PolicyContext`` and returns ``bool``.
    ``False`` → ``RuleDenied(name)`` raised at the pending tool call site.

    Rules are registered at decoration time and called by the bridge during
    ``/tool/call`` evaluation. Bridge registration implemented in Day 3.
    """

    def decorator(fn: F) -> F:
        # Registration implemented in Day 3
        return fn

    return decorator


def agent(name: str) -> Callable[[F], F]:
    """Activate a named limit scope for the duration of the decorated function.

    Calls ``/agent/enter`` on entry and ``/agent/exit`` in a ``finally``
    block so the scope always exits even on exception. Supports both sync
    and async functions. Bridge integration implemented in Day 4.
    """

    def decorator(fn: F) -> F:
        if _client.is_passthrough():
            return fn

        if inspect.iscoroutinefunction(fn):

            @functools.wraps(fn)
            async def async_wrapper(*args: Any, **kwargs: Any) -> Any:
                # Scope enter/exit implemented in Day 4
                return await fn(*args, **kwargs)

            return async_wrapper  # type: ignore[return-value]

        @functools.wraps(fn)
        def wrapper(*args: Any, **kwargs: Any) -> Any:
            # Scope enter/exit implemented in Day 4
            return fn(*args, **kwargs)

        return wrapper  # type: ignore[return-value]

    return decorator
