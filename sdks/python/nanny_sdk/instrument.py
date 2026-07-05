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

**Streaming:** streamed responses are wrapped so usage is submitted once the
stream is fully consumed. The provider must emit token usage in the stream
(e.g. OpenAI ``stream_options={"include_usage": True}``); without it, a streamed
call reports 0 tokens.
"""

from __future__ import annotations

import inspect
import threading
from collections.abc import Callable
from typing import Any
from weakref import WeakSet

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

# Guard against double-patching the same client. Keyed on the client OBJECT via
# a WeakSet — not id(), which Python recycles after GC (a recycled id would
# wrongly cause a brand-new client to be treated as already patched, so its
# usage would never be reported).
_patched_clients: WeakSet[Any] = WeakSet()
_patched_lock = threading.Lock()


def _patch_client(client: Any) -> None:
    """Monkey-patch the completion method(s) on *client* to submit usage."""
    with _patched_lock:
        try:
            if client in _patched_clients:
                return
            _patched_clients.add(client)
        except TypeError:
            # Client isn't hashable / weak-referenceable — instrument it anyway
            # rather than fail to, forgoing only the dedup guard.
            pass

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
        completions = client.chat.completions
    except AttributeError:
        return

    _wrap_method(completions, "create", _extract_usage)


def _try_patch_anthropic(client: Any) -> None:
    """Patch ``client.messages.create`` — Anthropic."""
    try:
        messages = client.messages
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
        chat = client.chat
    except AttributeError:
        return

    if not hasattr(chat, "complete") or hasattr(chat, "completions"):
        return

    _wrap_method(chat, "complete", _extract_usage)


def _try_patch_gemini(client: Any) -> None:
    """Patch ``client.models.generate_content`` — Google Gemini (google-genai SDK)."""
    try:
        models = client.models
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
            return _capture(await original(*args, **kwargs), _extract_usage)

        client.chat = _async_chat
    else:
        def _sync_chat(*args: Any, **kwargs: Any) -> Any:
            return _capture(original(*args, **kwargs), _extract_usage)

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
            return _capture(await original(*args, **kwargs), extractor)

        setattr(obj, method_name, _async_wrapper)
    else:
        def _sync_wrapper(*args: Any, **kwargs: Any) -> Any:
            return _capture(original(*args, **kwargs), extractor)

        setattr(obj, method_name, _sync_wrapper)


# ---------------------------------------------------------------------------
# Stream-aware usage capture
# ---------------------------------------------------------------------------

def _capture(response: Any, extractor: Callable[[Any], tuple[int, int]]) -> Any:
    """Submit usage for *response*, transparently handling streaming.

    A non-stream response carries ``.usage`` immediately — extract and submit.
    A streaming response is an iterator (``__next__`` / ``__anext__``) whose
    usage only appears in the final chunk, so wrap it and submit once the stream
    is fully consumed. (Requires the provider to emit usage in the stream — e.g.
    OpenAI ``stream_options={"include_usage": True}``; otherwise 0 is reported.)
    """
    if hasattr(response, "__anext__"):
        return _AsyncUsageStream(response, extractor)
    if hasattr(response, "__next__"):
        return _SyncUsageStream(response, extractor)
    _submit(extractor(response))
    return response


class _SyncUsageStream:
    """Wraps a sync streaming response; submits usage once, at end of stream.

    Delegates all other attribute access + the context-manager protocol to the
    underlying stream, so callers using ``with`` / ``.close()`` still work.
    """

    def __init__(self, stream: Any, extractor: Callable[[Any], tuple[int, int]]) -> None:
        self._stream = stream
        self._extractor = extractor
        self._counts: tuple[int, int] = (0, 0)
        self._flushed = False

    def __iter__(self) -> _SyncUsageStream:
        return self

    def __next__(self) -> Any:
        try:
            chunk = next(self._stream)
        except StopIteration:
            self._flush()
            raise
        counts = self._extractor(chunk)
        if counts[0] or counts[1]:
            self._counts = counts
        return chunk

    def _flush(self) -> None:
        if not self._flushed:
            self._flushed = True
            _submit(self._counts)

    def __enter__(self) -> _SyncUsageStream:
        enter = getattr(self._stream, "__enter__", None)
        if enter is not None:
            enter()
        return self

    def __exit__(self, *exc_info: object) -> bool:
        self._flush()
        exit_ = getattr(self._stream, "__exit__", None)
        return bool(exit_(*exc_info)) if exit_ is not None else False

    def __getattr__(self, name: str) -> Any:
        return getattr(self._stream, name)


class _AsyncUsageStream:
    """Wraps an async streaming response; submits usage once, at end of stream."""

    def __init__(self, stream: Any, extractor: Callable[[Any], tuple[int, int]]) -> None:
        self._stream = stream
        self._extractor = extractor
        self._counts: tuple[int, int] = (0, 0)
        self._flushed = False

    def __aiter__(self) -> _AsyncUsageStream:
        return self

    async def __anext__(self) -> Any:
        try:
            chunk = await self._stream.__anext__()
        except StopAsyncIteration:
            self._flush()
            raise
        counts = self._extractor(chunk)
        if counts[0] or counts[1]:
            self._counts = counts
        return chunk

    def _flush(self) -> None:
        if not self._flushed:
            self._flushed = True
            _submit(self._counts)

    async def __aenter__(self) -> _AsyncUsageStream:
        aenter = getattr(self._stream, "__aenter__", None)
        if aenter is not None:
            await aenter()
        return self

    async def __aexit__(self, *exc_info: object) -> bool:
        self._flush()
        aexit = getattr(self._stream, "__aexit__", None)
        if aexit is not None:
            return bool(await aexit(*exc_info))
        return False

    def __getattr__(self, name: str) -> Any:
        return getattr(self._stream, name)


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
