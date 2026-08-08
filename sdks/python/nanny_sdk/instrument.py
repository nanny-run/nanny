"""nanny_sdk.instrument — LLM token + attribution measurement via client wrapping.

Call once at agent startup to automatically submit LLM token usage — plus the
model, provider, and (auto-detected) harness — to the bridge on every call. The
bridge debits tokens from the shared ledger and records the attribution the cloud
uses for Fleet Intelligence (cost, model/provider/harness breakdowns).

Usage::

    import openai
    import nanny_sdk

    client = openai.OpenAI()
    nanny_sdk.instrument(client)   # one line — done

Supported clients (detected by duck-typing — no provider package is imported):

- **OpenAI** ``openai.OpenAI`` / ``openai.AsyncOpenAI`` (+ Groq / Together / Azure
  / LiteLLM / OpenRouter / DeepSeek via the OpenAI-compatible interface)
- **Anthropic** ``anthropic.Anthropic`` / ``anthropic.AsyncAnthropic``
- **Mistral** ``mistralai.Mistral``
- **Google Gemini** (``google-genai`` SDK, ``genai.Client``)
- **Cohere v2** ``cohere.ClientV2``

**Attribution captured on every call:**

- **model** — read off the response (``response.model``).
- **provider** — the client family; for the OpenAI-compatible bucket, refined from
  the client's ``base_url`` host (groq/together/openrouter/…), else ``openai``.
- **harness** — auto-detected from the call stack / imported frameworks
  (langgraph, crewai, opencode, …), cached after first detection and re-sent on
  every call (the bridge dedups). ``None`` when no known harness is present.
- **cache_read / cache_write** — a finer split of ``input`` tokens, for
  providers that report prompt-caching usage (OpenAI, Anthropic, DeepSeek,
  Gemini). Every provider names and shapes this differently — there is no
  shared field the way there is for input/output tokens — so each is handled
  by its own explicit branch in ``_extract_cache_usage``. Absent, not zero,
  for providers/responses that don't report it. Reporting only: never
  debited separately from ``input``, no pricing logic reads these fields in
  the engine.

**Passthrough mode:** when the bridge is not present (``NANNY_BRIDGE_SOCKET``,
``NANNY_BRIDGE_PORT``, ``NANNY_BRIDGE_ADDR`` all absent), ``instrument`` returns
the client unchanged. No wrapping, no overhead.

**Non-intrusive:** it monkey-patches the completion method only. It never
inspects message content — only numeric token counts and the model/provider
identifiers from the response. No manifesto violation.

**Streaming:** streamed responses are wrapped so usage is submitted once the
stream is fully consumed (the provider must emit usage in the stream, e.g. OpenAI
``stream_options={"include_usage": True}``).
"""

from __future__ import annotations

import inspect
import sys
import threading
from typing import Any
from weakref import WeakSet

import nanny_sdk._client as _bridge

# A usage record extracted from a response: (input_tokens, output_tokens, model,
# cache_read, cache_write). cache_read/cache_write are a finer split of
# input_tokens (never additional tokens beyond it), present only for
# providers that report prompt-caching usage at all — None, not 0, for
# providers/responses that don't.
UsageRecord = tuple[int, int, "str | None", "int | None", "int | None"]

# ---------------------------------------------------------------------------
# Public API
# ---------------------------------------------------------------------------


def instrument(client: Any) -> Any:
    """Wrap *client* so token usage + attribution is reported to the bridge.

    Returns the same client object (mutated in-place). No-op when the bridge is
    not active. Providers detected by duck-typing — no provider package imported.
    """
    if _bridge.is_passthrough():
        return client

    _patch_client(client)
    return client


# ---------------------------------------------------------------------------
# Provider resolution
# ---------------------------------------------------------------------------

# base_url host fragment → provider label, for the OpenAI-compatible bucket.
_OPENAI_HOSTS: dict[str, str] = {
    "api.openai.com": "openai",
    "openai.azure.com": "azure",
    "groq.com": "groq",
    "together.": "together_ai",
    "openrouter.ai": "openrouter",
    "mistral.ai": "mistral",
    "deepseek.com": "deepseek",
    "fireworks.ai": "fireworks_ai",
}


def _openai_provider(client: Any) -> str:
    """Refine the OpenAI-compatible provider from the client's base_url host."""
    try:
        host = str(getattr(client, "base_url", "") or "").lower()
    except Exception:  # noqa: BLE001
        host = ""
    for fragment, provider in _OPENAI_HOSTS.items():
        if fragment in host:
            return provider
    return "openai"


# ---------------------------------------------------------------------------
# Harness auto-detection (SDK-layer convenience — never in the engine)
# ---------------------------------------------------------------------------

# Known agentic harnesses / frameworks, by their top-level import name.
_HARNESS_ROOTS: dict[str, str] = {
    "opencode": "opencode",
    "cline": "cline",
    "langgraph": "langgraph",
    "langchain": "langchain",
    "crewai": "crewai",
    "llama_index": "llama-index",
    "autogen": "autogen",
    "haystack": "haystack",
    "dspy": "dspy",
    "smolagents": "smolagents",
    "pydantic_ai": "pydantic-ai",
    "agno": "agno",
}

_harness_lock = threading.Lock()
_harness_detected = False
_harness_value: str | None = None


def _detect_harness() -> str | None:
    """Best-effort harness name, cached after first detection.

    Prefers the call stack (which framework actually drove this call), then falls
    back to imported-module presence. Returns None when no known harness is found.
    Heuristic — that's why it lives in the SDK, not the engine.
    """
    global _harness_detected, _harness_value
    if _harness_detected:
        return _harness_value
    with _harness_lock:
        if _harness_detected:
            return _harness_value
        value: str | None = None
        try:
            for info in inspect.stack():
                root = (info.frame.f_globals.get("__name__", "") or "").split(".", 1)[0]
                if root in _HARNESS_ROOTS:
                    value = _HARNESS_ROOTS[root]
                    break
        except Exception:  # noqa: BLE001
            value = None
        if value is None:
            for root, name in _HARNESS_ROOTS.items():
                if root in sys.modules:
                    value = name
                    break
        _harness_value = value
        _harness_detected = True
        return _harness_value


# ---------------------------------------------------------------------------
# Internal patching logic
# ---------------------------------------------------------------------------

# Guard against double-patching the same client (WeakSet keyed on the object).
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
            # Not hashable / weak-referenceable — instrument anyway, no dedup.
            pass

    # Detection order matters — see module docstring for rationale.
    _try_patch_openai(client)            # client.chat.completions.create
    _try_patch_openai_responses(client)  # client.responses.create
    _try_patch_anthropic(client)         # client.messages.create
    _try_patch_mistral(client)     # client.chat.complete (no .completions)
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
    _wrap_method(completions, "create", _openai_provider(client))


def _try_patch_openai_responses(client: Any) -> None:
    """Patch ``client.responses.create`` — OpenAI's Responses API, the
    endpoint required for combining tool calls with real reasoning_effort
    on GPT-5.x reasoning models (confirmed live: /v1/chat/completions
    rejects that combination outright). A separate top-level attribute from
    ``client.chat.completions``, not a variant of it, so this is a second,
    independent patch, not a branch inside ``_try_patch_openai``. Only
    OpenAI itself exposes this endpoint (no Groq/Together/DeepSeek
    equivalent), but reuses ``_openai_provider`` for the same base_url-based
    attribution the chat.completions patch already uses, in case a future
    OpenAI-compatible host adds its own Responses-shaped endpoint.
    """
    try:
        responses = client.responses
    except AttributeError:
        return
    if not hasattr(responses, "create"):
        return
    _wrap_method(responses, "create", _openai_provider(client))


def _try_patch_anthropic(client: Any) -> None:
    """Patch ``client.messages.create`` — Anthropic."""
    try:
        messages = client.messages
    except AttributeError:
        return
    _wrap_method(messages, "create", "anthropic")


def _try_patch_mistral(client: Any) -> None:
    """Patch ``client.chat.complete`` — Mistral AI (has ``.complete``, no ``.completions``)."""
    try:
        chat = client.chat
    except AttributeError:
        return
    if not hasattr(chat, "complete") or hasattr(chat, "completions"):
        return
    _wrap_method(chat, "complete", "mistral")


def _try_patch_gemini(client: Any) -> None:
    """Patch ``client.models.generate_content`` — Google Gemini (google-genai SDK)."""
    try:
        models = client.models
    except AttributeError:
        return
    if not hasattr(models, "generate_content"):
        return
    _wrap_method(models, "generate_content", "gemini")


def _try_patch_cohere(client: Any) -> None:
    """Patch ``client.chat`` — Cohere v2 (``client.chat`` is directly callable)."""
    chat = getattr(client, "chat", None)
    if chat is None or not callable(chat):
        return
    original = chat

    if inspect.iscoroutinefunction(original):

        async def _async_chat(*args: Any, **kwargs: Any) -> Any:
            return _capture(await original(*args, **kwargs), "cohere")

        client.chat = _async_chat
    else:

        def _sync_chat(*args: Any, **kwargs: Any) -> Any:
            return _capture(original(*args, **kwargs), "cohere")

        client.chat = _sync_chat


# ---------------------------------------------------------------------------
# Generic method wrapper
# ---------------------------------------------------------------------------


def _wrap_method(obj: Any, method_name: str, provider: str) -> None:
    """Replace *obj.method_name* with a wrapper that submits usage after the call."""
    original = getattr(obj, method_name, None)
    if original is None:
        return

    if inspect.iscoroutinefunction(original):

        async def _async_wrapper(*args: Any, **kwargs: Any) -> Any:
            return _capture(await original(*args, **kwargs), provider)

        setattr(obj, method_name, _async_wrapper)
    else:

        def _sync_wrapper(*args: Any, **kwargs: Any) -> Any:
            return _capture(original(*args, **kwargs), provider)

        setattr(obj, method_name, _sync_wrapper)


# ---------------------------------------------------------------------------
# Stream-aware usage capture
# ---------------------------------------------------------------------------


def _capture(response: Any, provider: str) -> Any:
    """Submit usage for *response*, transparently handling streaming."""
    if hasattr(response, "__anext__"):
        return _AsyncUsageStream(response, provider)
    if hasattr(response, "__next__"):
        return _SyncUsageStream(response, provider)
    _submit(_extract_usage(response), provider)
    return response


class _SyncUsageStream:
    """Wraps a sync streaming response; submits usage once, at end of stream."""

    def __init__(self, stream: Any, provider: str) -> None:
        self._stream = stream
        self._provider = provider
        self._record: UsageRecord = (0, 0, None, None, None)
        self._flushed = False

    def __iter__(self) -> _SyncUsageStream:
        return self

    def __next__(self) -> Any:
        try:
            chunk = next(self._stream)
        except StopIteration:
            self._flush()
            raise
        rec = _extract_usage(chunk)
        if rec[0] or rec[1] or rec[2]:
            self._record = rec
        return chunk

    def _flush(self) -> None:
        if not self._flushed:
            self._flushed = True
            _submit(self._record, self._provider)

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

    def __init__(self, stream: Any, provider: str) -> None:
        self._stream = stream
        self._provider = provider
        self._record: UsageRecord = (0, 0, None, None, None)
        self._flushed = False

    def __aiter__(self) -> _AsyncUsageStream:
        return self

    async def __anext__(self) -> Any:
        try:
            chunk = await self._stream.__anext__()
        except StopAsyncIteration:
            self._flush()
            raise
        rec = _extract_usage(chunk)
        if rec[0] or rec[1] or rec[2]:
            self._record = rec
        return chunk

    def _flush(self) -> None:
        if not self._flushed:
            self._flushed = True
            _submit(self._record, self._provider)

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
# Usage + model extraction
# ---------------------------------------------------------------------------


def _extract_model(response: Any) -> str | None:
    """Best-effort model id from a response (``model`` or Gemini ``model_version``)."""
    for attr in ("model", "model_version"):
        value = getattr(response, attr, None)
        if isinstance(value, str) and value:
            return value
    return None


def _extract_cache_usage(usage: Any) -> tuple[int | None, int | None]:
    """Return ``(cache_read, cache_write)`` from a response's ``usage`` object,
    ``(None, None)`` if this provider/response doesn't report prompt-caching
    usage at all. Every provider uses its own field name and shape for this
    (confirmed against each provider's own docs — unlike input/output tokens,
    cache accounting never converged industry-wide), so unlike
    ``_extract_usage`` above there's no single shared field pattern to try;
    each provider gets its own explicit branch. Absent (``None``), not
    ``(0, 0)``, when nothing matches — a provider that doesn't report cache
    usage is a genuinely different case from one that reports zero cache
    hits, and downstream cost calculators need to be able to tell them apart.
    """
    try:
        # OpenAI (nested; read-only, no separate write-cost concept)
        details = getattr(usage, "prompt_tokens_details", None)
        if details is not None:
            cached = getattr(details, "cached_tokens", None)
            if cached is not None:
                return (int(cached), None)

        # Anthropic (top-level; write and read are both real, separate costs)
        cache_read = getattr(usage, "cache_read_input_tokens", None)
        cache_write = getattr(usage, "cache_creation_input_tokens", None)
        if cache_read is not None or cache_write is not None:
            return (
                int(cache_read) if cache_read is not None else None,
                int(cache_write) if cache_write is not None else None,
            )

        # DeepSeek (OpenAI-compatible shape, own top-level hit/miss fields).
        # Only "hit" maps to cache_read: a miss is priced at the normal input
        # rate, already counted in input_tokens — DeepSeek has no write-cost
        # concept the way Anthropic does, so cache_write is never set here.
        hit = getattr(usage, "prompt_cache_hit_tokens", None)
        if hit is not None:
            return (int(hit), None)

        # OpenAI Responses API (nested, but under input_tokens_details, not
        # prompt_tokens_details — a real, different attribute name from
        # Chat Completions despite being the same provider). Confirmed live
        # against a real gpt-5.6-terra call: ResponseUsage carries both
        # cached_tokens and cache_write_tokens (unlike Chat Completions,
        # which only ever reports the read side), so both are extracted
        # here, not just cached_tokens.
        details = getattr(usage, "input_tokens_details", None)
        if details is not None:
            cached = getattr(details, "cached_tokens", None)
            written = getattr(details, "cache_write_tokens", None)
            if cached is not None or written is not None:
                return (
                    int(cached) if cached is not None else None,
                    int(written) if written is not None else None,
                )

    except Exception:  # noqa: BLE001 — never crash the agent for telemetry
        pass

    return (None, None)


def _extract_usage(response: Any) -> UsageRecord:
    """Return ``(input_tokens, output_tokens, model, cache_read, cache_write)``
    from any supported response.

    Tries all known usage field patterns; returns ``(0, 0, model, None, None)``
    if none match or anything raises — this must never crash the agent.
    """
    model = _extract_model(response)
    try:
        usage = getattr(response, "usage", None)
        if usage is not None:
            # OpenAI / Groq / Together / Mistral / LiteLLM
            pt = getattr(usage, "prompt_tokens", None)
            ct = getattr(usage, "completion_tokens", None)
            if pt is not None and ct is not None:
                cache_read, cache_write = _extract_cache_usage(usage)
                return (int(pt) or 0, int(ct) or 0, model, cache_read, cache_write)

            # OpenAI Responses API. Shares the input_tokens/output_tokens
            # field names with Anthropic below (real, separate provider,
            # coincidentally same names), so this must be checked first and
            # disambiguated by input_tokens_details — a marker Anthropic's
            # own Usage object never has (see _extract_cache_usage) — not
            # by field name alone, or a Responses-API call would silently
            # fall into Anthropic's ADDITIVE total-input formula below and
            # double-count cached tokens, over-debiting the budget for
            # every cache-heavy Responses call. Confirmed live: like Chat
            # Completions, input_tokens here is already INCLUSIVE of both
            # cached_tokens and cache_write_tokens, so no addition needed —
            # the same "count includes cache" convention OpenAI uses
            # everywhere, unlike Anthropic's exclusive count.
            if hasattr(usage, "input_tokens_details"):
                it = getattr(usage, "input_tokens", None)
                ot = getattr(usage, "output_tokens", None)
                if it is not None and ot is not None:
                    cache_read, cache_write = _extract_cache_usage(usage)
                    return (int(it) or 0, int(ot) or 0, model, cache_read, cache_write)

            # Anthropic. Its own input_tokens is EXCLUSIVE of cache usage —
            # real total input = input_tokens + cache_read + cache_write
            # (confirmed against Anthropic's prompt-caching docs) — the
            # opposite of DeepSeek/OpenAI, where the base count already
            # includes cache hits. Add them in here so Nanny's own `input`
            # field keeps one universal meaning regardless of provider: the
            # true total, with cache_read/cache_write always a genuine
            # subset of it, never additional on top, matching what every
            # downstream consumer (enforcement, cost calculators) is told
            # to assume. Getting this wrong would under-debit the budget for
            # every cache-heavy Anthropic call, not just under-report cost.
            it = getattr(usage, "input_tokens", None)
            ot = getattr(usage, "output_tokens", None)
            if it is not None and ot is not None:
                cache_read, cache_write = _extract_cache_usage(usage)
                total_input = (int(it) or 0) + (cache_read or 0) + (cache_write or 0)
                return (total_input, int(ot) or 0, model, cache_read, cache_write)

            # Cohere v2 (response_tokens instead of completion_tokens)
            rt = getattr(usage, "response_tokens", None)
            if pt is not None and rt is not None:
                return (int(pt) or 0, int(rt) or 0, model, None, None)

        # Google Gemini (usage_metadata, different field names)
        usage_meta = getattr(response, "usage_metadata", None)
        if usage_meta is not None:
            ptc = getattr(usage_meta, "prompt_token_count", None)
            ctc = getattr(usage_meta, "candidates_token_count", None)
            if ptc is not None or ctc is not None:
                cached = getattr(usage_meta, "cached_content_token_count", None)
                cache_read = int(cached) if cached is not None else None
                return (int(ptc or 0), int(ctc or 0), model, cache_read, None)

    except Exception:  # noqa: BLE001 — never crash the agent for telemetry
        pass

    return (0, 0, model, None, None)


# ---------------------------------------------------------------------------
# Bridge submission
# ---------------------------------------------------------------------------


def _submit(record: UsageRecord, provider: str | None) -> None:
    """POST /llm/usage with tokens + model + provider + cache usage + harness.
    Errors dropped."""
    input_tokens, output_tokens, model, cache_read, cache_write = record
    if not input_tokens and not output_tokens:
        return
    body: dict[str, Any] = {"input": input_tokens, "output": output_tokens}
    if model:
        body["model"] = model
    if provider:
        body["provider"] = provider
    if cache_read is not None:
        body["cache_read"] = cache_read
    if cache_write is not None:
        body["cache_write"] = cache_write
    harness = _detect_harness()
    if harness:
        body["harness"] = {"name": harness}
    try:
        with _bridge._make_client(timeout=5.0) as http:
            http.post("/llm/usage", json=body, headers=_bridge._headers())
    except Exception:  # noqa: BLE001
        pass
