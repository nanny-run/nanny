"""nanny_sdk.instrument — LLM token measurement via client wrapping.

Call once at agent startup to automatically submit LLM token usage to the
bridge. The bridge debits those tokens from the shared ledger — the same
ledger that ``@tool(tokens=N)`` charges against.

Usage::

    import openai
    import nanny_sdk

    client = openai.OpenAI()
    nanny_sdk.instrument(client)   # one line — done

    # From here on, every response's token usage is reported to the bridge.

Supported clients (detected by duck-typing — no provider package is imported):

- **OpenAI** ``openai.OpenAI`` / ``openai.AsyncOpenAI``
- **Groq** ``groq.Groq`` / ``groq.AsyncGroq``
- **Together AI** ``together.Together``
- **Azure OpenAI** (uses the OpenAI SDK)
- **LiteLLM** (normalises all providers to the OpenAI format)
- **Anthropic** ``anthropic.Anthropic`` / ``anthropic.AsyncAnthropic``
- **Mistral** ``mistralai.Mistral`` (uses ``chat.complete``, not ``chat.completions.create``)
- **Google Gemini** (``google-genai`` SDK, ``genai.Client``)
- **Cohere v2** ``cohere.ClientV2``

Any client whose ``chat.completions.create`` method returns a response with a
``.usage.prompt_tokens`` / ``.usage.completion_tokens`` attribute pair is also
covered automatically (OpenAI-compatible interface).

**Passthrough mode:** when the bridge is not present (i.e. ``NANNY_BRIDGE_SOCKET``,
``NANNY_BRIDGE_PORT``, and ``NANNY_BRIDGE_ADDR`` are all absent), ``instrument``
returns the client unchanged. No wrapping, no overhead.

**Non-intrusive:** ``instrument`` monkey-patches the client's completion method.
It does not inspect message content — only the numeric token counts from
the response usage object. No manifesto violation.
"""

from __future__ import annotations

import inspect
import threading
from typing import Any

import nanny_sdk._client as _bridge


# ---------------------------------------------------------------------------
# Public API
# ---------------------------------------------------------------------------


def instrument(client: Any) -> Any:
    """Wrap *client* so that token usage is automatically reported to the bridge.

    Returns the same client object (mutated in-place for easy one-liner use).
    When the bridge is not active, returns *client* unchanged.

    Supported providers are detected by duck-typing — no provider package is
    imported inside this function.

    :param client: Any supported LLM client instance.
    :returns: The same *client* object.
    """
    if _bridge.is_passthrough():
        return client

    _patch_client(client)
    return client


# ---------------------------------------------------------------------------
# Internal patching logic
# ---------------------------------------------------------------------------

# Guard against double-patching the same client.
_patched_clients: set[int] = set()
_patched_lock = threading.Lock()


def _patch_client(client: Any) -> None:
    """Monkey-patch the completion method(s) on *client* to submit usage."""
    client_id = id(client)
    with _patched_lock:
        if client_id in _patched_clients:
            return
        _patched_clients.add(client_id)

    # Detection order matters — see module docstring for rationale.
    _try_patch_openai(client)      # client.chat.completions.create
    _try_patch_anthropic(client)   # client.messages.create
    _try_patch_mistral(client)     # client.chat.complete (no .completions attr)
    _try_patch_gemini(client)      # client.models.generate_content
    _try_patch_cohere(client)      # callable(client.chat) — fallback, runs last


# ---------------------------------------------------------------------------
# Provider-specific patchers
# ---------------------------------------------------------------------------

def _try_patch_openai(client: Any) -> None:
    """Patch ``client.chat.completions.create`` — OpenAI / Groq / Together / LiteLLM."""
    try:
        completions = client.chat.completions  # type: ignore[attr-defined]
    except AttributeError:
        return

    _wrap_method(completions, "create", _extract_usage)


def _try_patch_anthropic(client: Any) -> None:
    """Patch ``client.messages.create`` — Anthropic."""
    try:
        messages = client.messages  # type: ignore[attr-defined]
    except AttributeError:
        return

    _wrap_method(messages, "create", _extract_usage)


def _try_patch_mistral(client: Any) -> None:
    """Patch ``client.chat.complete`` — Mistral AI.

    Guard: ``client.chat`` must have ``.complete`` but NOT ``.completions``.
    The absence of ``.completions`` distinguishes Mistral from OpenAI-style clients
    that also happen to expose ``.complete``.
    """
    try:
        chat = client.chat  # type: ignore[attr-defined]
    except AttributeError:
        return

    if not hasattr(chat, "complete") or hasattr(chat, "completions"):
        return

    _wrap_method(chat, "complete", _extract_usage)


def _try_patch_gemini(client: Any) -> None:
    """Patch ``client.models.generate_content`` — Google Gemini (google-genai SDK)."""
    try:
        models = client.models  # type: ignore[attr-defined]
    except AttributeError:
        return

    if not hasattr(models, "generate_content"):
        return

    _wrap_method(models, "generate_content", _extract_usage)


def _try_patch_cohere(client: Any) -> None:
    """Patch ``client.chat`` — Cohere v2 (``ClientV2``).

    Guard: ``client.chat`` must be *callable* directly. OpenAI's ``client.chat``
    is a namespace object (not callable), so this guard never fires for OpenAI,
    Groq, or any other OpenAI-compatible client. Runs last to act as a fallback.
    """
    chat = getattr(client, "chat", None)
    if chat is None or not callable(chat):
        return

    # For Cohere, client.chat IS the callable — we patch it on the client directly.
    original = chat

    if inspect.iscoroutinefunction(original):
        async def _async_chat(*args: Any, **kwargs: Any) -> Any:
            response = await original(*args, **kwargs)
            _submit(_extract_usage(response))
            return response

        client.chat = _async_chat
    else:
        def _sync_chat(*args: Any, **kwargs: Any) -> Any:
            response = original(*args, **kwargs)
            _submit(_extract_usage(response))
            return response

        client.chat = _sync_chat


# ---------------------------------------------------------------------------
# Generic method wrapper
# ---------------------------------------------------------------------------

def _wrap_method(obj: Any, method_name: str, extractor: Any) -> None:
    """Replace *obj.method_name* with a wrapper that submits usage after the call."""
    original = getattr(obj, method_name, None)
    if original is None:
        return

    if inspect.iscoroutinefunction(original):
        async def _async_wrapper(*args: Any, **kwargs: Any) -> Any:
            response = await original(*args, **kwargs)
            _submit(extractor(response))
            return response

        setattr(obj, method_name, _async_wrapper)
    else:
        def _sync_wrapper(*args: Any, **kwargs: Any) -> Any:
            response = original(*args, **kwargs)
            _submit(extractor(response))
            return response

        setattr(obj, method_name, _sync_wrapper)


# ---------------------------------------------------------------------------
# Usage extraction — tries all known field patterns
# ---------------------------------------------------------------------------

def _extract_usage(response: Any) -> tuple[int, int]:
    """Return ``(input_tokens, output_tokens)`` from any supported response object.

    Tries all known usage field patterns in order. Returns ``(0, 0)`` if none
    match or if anything raises — this function must never crash the agent.

    Patterns tried:
    1. ``response.usage.prompt_tokens`` + ``completion_tokens``
       — OpenAI, Groq, Together AI, Azure OpenAI, Mistral, LiteLLM
    2. ``response.usage.input_tokens`` + ``output_tokens``
       — Anthropic
    3. ``response.usage.prompt_tokens`` + ``response_tokens``
       — Cohere v2 (``response.usage`` path)
    4. ``response.usage_metadata.prompt_token_count`` + ``candidates_token_count``
       — Google Gemini (``google-genai`` SDK)
    """
    try:
        usage = getattr(response, "usage", None)
        if usage is not None:
            # Pattern 1 — OpenAI / Groq / Together / Mistral / LiteLLM
            pt = getattr(usage, "prompt_tokens", None)
            ct = getattr(usage, "completion_tokens", None)
            if pt is not None and ct is not None:
                return (int(pt) or 0, int(ct) or 0)

            # Pattern 2 — Anthropic
            it = getattr(usage, "input_tokens", None)
            ot = getattr(usage, "output_tokens", None)
            if it is not None and ot is not None:
                return (int(it) or 0, int(ot) or 0)

            # Pattern 3 — Cohere v2 (response_tokens instead of completion_tokens)
            rt = getattr(usage, "response_tokens", None)
            if pt is not None and rt is not None:
                return (int(pt) or 0, int(rt) or 0)

        # Pattern 4 — Google Gemini (usage_metadata, different field names)
        usage_meta = getattr(response, "usage_metadata", None)
        if usage_meta is not None:
            ptc = getattr(usage_meta, "prompt_token_count", None)
            ctc = getattr(usage_meta, "candidates_token_count", None)
            if ptc is not None or ctc is not None:
                return (int(ptc or 0), int(ctc or 0))

    except Exception:  # noqa: BLE001 — never crash the agent for telemetry
        pass

    return (0, 0)


# ---------------------------------------------------------------------------
# Bridge submission
# ---------------------------------------------------------------------------

def _submit(counts: tuple[int, int]) -> None:
    """POST /llm/usage to the bridge. Silently drops errors (never crash agent)."""
    input_tokens, output_tokens = counts
    if not input_tokens and not output_tokens:
        return
    try:
        with _bridge._make_client(timeout=5.0) as http:
            http.post(
                "/llm/usage",
                json={"input": input_tokens, "output": output_tokens},
                headers=_bridge._headers(),
            )
            # We don't raise on BudgetExhausted here — the agent may be mid-LLM
            # call. The bridge marks execution stopped; the next @tool call will
            # raise BudgetExhausted through the normal path.
    except Exception:  # noqa: BLE001
        pass
