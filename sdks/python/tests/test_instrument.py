"""Tests for ``nanny_sdk.instrument`` — LLM token measurement via client wrapping.

Uses the ``mock_bridge`` fixture: ``instrument`` patches a fake client, and each
wrapped call POSTs token usage to the fake bridge's ``/llm/usage``. Tests assert
on the JSON bodies the bridge received — the exact end-to-end contract.

Covers: each provider shape (OpenAI/Anthropic/Mistral/Gemini/Cohere), the four
usage-extraction patterns, sync + async, streaming (sync + async), passthrough
(no bridge = no wrapping), the double-instrument dedup guard, and zero-usage skip.
"""

from __future__ import annotations

from typing import Any

import pytest
from pytest_httpserver import HTTPServer

from nanny_sdk import instrument
from nanny_sdk.instrument import _extract_usage


def _usage_posts(bridge: HTTPServer) -> list[dict[str, Any]]:
    """JSON bodies POSTed to ``/llm/usage``, in order."""
    return [
        req.get_json()
        for req, _resp in bridge.log
        if req.path == "/llm/usage" and req.method == "POST"
    ]


def _expect_usage(bridge: HTTPServer) -> None:
    bridge.expect_request("/llm/usage", method="POST").respond_with_json({"status": "ok"})


# ── Fake response / usage objects ────────────────────────────────────────────


class _Usage:
    def __init__(self, **fields: int) -> None:
        for key, value in fields.items():
            setattr(self, key, value)


class _Resp:
    def __init__(self, usage: Any = None, usage_metadata: Any = None) -> None:
        if usage is not None:
            self.usage = usage
        if usage_metadata is not None:
            self.usage_metadata = usage_metadata


# ── Fake provider clients ─────────────────────────────────────────────────────


class FakeOpenAI:
    """``client.chat.completions.create`` → usage.prompt_tokens/completion_tokens."""

    class _Completions:
        def create(self, **kwargs: Any) -> Any:
            return _Resp(usage=_Usage(prompt_tokens=10, completion_tokens=5))

    class _Chat:
        def __init__(self) -> None:
            self.completions = FakeOpenAI._Completions()

    def __init__(self) -> None:
        self.chat = FakeOpenAI._Chat()


class FakeAnthropic:
    """``client.messages.create`` → usage.input_tokens/output_tokens."""

    class _Messages:
        def create(self, **kwargs: Any) -> Any:
            return _Resp(usage=_Usage(input_tokens=8, output_tokens=3))

    def __init__(self) -> None:
        self.messages = FakeAnthropic._Messages()


class FakeMistral:
    """``client.chat.complete`` (no ``.completions``) → prompt/completion."""

    class _Chat:
        def complete(self, **kwargs: Any) -> Any:
            return _Resp(usage=_Usage(prompt_tokens=7, completion_tokens=2))

    def __init__(self) -> None:
        self.chat = FakeMistral._Chat()


class FakeGemini:
    """``client.models.generate_content`` → usage_metadata token counts."""

    class _Models:
        def generate_content(self, **kwargs: Any) -> Any:
            return _Resp(usage_metadata=_Usage(prompt_token_count=6, candidates_token_count=4))

    def __init__(self) -> None:
        self.models = FakeGemini._Models()


class FakeCohere:
    """``client.chat`` is directly callable → usage.prompt_tokens/response_tokens."""

    def chat(self, **kwargs: Any) -> Any:
        return _Resp(usage=_Usage(prompt_tokens=9, response_tokens=1))


# ── Fake streaming clients ────────────────────────────────────────────────────


class _StreamChunk:
    def __init__(self, usage: Any = None) -> None:
        if usage is not None:
            self.usage = usage


class _SyncStream:
    def __init__(self, chunks: list[Any]) -> None:
        self._it = iter(chunks)

    def __iter__(self) -> _SyncStream:
        return self

    def __next__(self) -> Any:
        return next(self._it)


class _AsyncStream:
    def __init__(self, chunks: list[Any]) -> None:
        self._it = iter(chunks)

    def __aiter__(self) -> _AsyncStream:
        return self

    async def __anext__(self) -> Any:
        try:
            return next(self._it)
        except StopIteration:
            raise StopAsyncIteration from None


class FakeStreamingOpenAI:
    class _Completions:
        def create(self, **kwargs: Any) -> Any:
            return _SyncStream(
                [_StreamChunk(), _StreamChunk(_Usage(prompt_tokens=10, completion_tokens=5))]
            )

    class _Chat:
        def __init__(self) -> None:
            self.completions = FakeStreamingOpenAI._Completions()

    def __init__(self) -> None:
        self.chat = FakeStreamingOpenAI._Chat()


class FakeAsyncOpenAI:
    class _Completions:
        async def create(self, **kwargs: Any) -> Any:
            return _Resp(usage=_Usage(prompt_tokens=12, completion_tokens=6))

    class _Chat:
        def __init__(self) -> None:
            self.completions = FakeAsyncOpenAI._Completions()

    def __init__(self) -> None:
        self.chat = FakeAsyncOpenAI._Chat()


class FakeAsyncStreamingOpenAI:
    class _Completions:
        async def create(self, **kwargs: Any) -> Any:
            return _AsyncStream(
                [_StreamChunk(), _StreamChunk(_Usage(prompt_tokens=11, completion_tokens=4))]
            )

    class _Chat:
        def __init__(self) -> None:
            self.completions = FakeAsyncStreamingOpenAI._Completions()

    def __init__(self) -> None:
        self.chat = FakeAsyncStreamingOpenAI._Chat()


# ── Usage extraction (the four patterns) ──────────────────────────────────────


def test_extract_usage_patterns() -> None:
    # Returns (input, output, model, cache_read, cache_write); these fakes
    # carry no model and no cache usage → None, None, None.
    assert _extract_usage(_Resp(usage=_Usage(prompt_tokens=10, completion_tokens=5))) == (
        10,
        5,
        None,
        None,
        None,
    )
    assert _extract_usage(_Resp(usage=_Usage(input_tokens=8, output_tokens=3))) == (
        8,
        3,
        None,
        None,
        None,
    )
    assert _extract_usage(_Resp(usage=_Usage(prompt_tokens=9, response_tokens=1))) == (
        9,
        1,
        None,
        None,
        None,
    )
    assert _extract_usage(
        _Resp(usage_metadata=_Usage(prompt_token_count=6, candidates_token_count=4))
    ) == (6, 4, None, None, None)
    assert _extract_usage(object()) == (0, 0, None, None, None)


def test_extract_usage_cache_patterns() -> None:
    # OpenAI: nested prompt_tokens_details.cached_tokens, read-only.
    assert _extract_usage(
        _Resp(
            usage=_Usage(
                prompt_tokens=10,
                completion_tokens=5,
                prompt_tokens_details=_Usage(cached_tokens=4),
            )
        )
    ) == (10, 5, None, 4, None)

    # Anthropic: top-level, both read and write are real, and its own
    # input_tokens is exclusive of both — real total input is 8+2+6=16, not
    # the raw 8, since Nanny's `input` must stay the true total with
    # cache_read/cache_write as a genuine subset, matching every other
    # provider's semantics (Anthropic's own wire format is the one that's
    # the odd one out, not Nanny's).
    assert _extract_usage(
        _Resp(
            usage=_Usage(
                input_tokens=8,
                output_tokens=3,
                cache_read_input_tokens=2,
                cache_creation_input_tokens=6,
            )
        )
    ) == (16, 3, None, 2, 6)

    # DeepSeek: OpenAI-compatible shape, own top-level hit field, no write concept.
    assert _extract_usage(
        _Resp(
            usage=_Usage(
                prompt_tokens=10,
                completion_tokens=5,
                prompt_cache_hit_tokens=7,
            )
        )
    ) == (10, 5, None, 7, None)

    # Gemini: usage_metadata, own cached-content field.
    assert _extract_usage(
        _Resp(
            usage_metadata=_Usage(
                prompt_token_count=6,
                candidates_token_count=4,
                cached_content_token_count=3,
            )
        )
    ) == (6, 4, None, 3, None)


# ── Passthrough — no bridge means no wrapping ─────────────────────────────────


def test_passthrough_returns_client_unwrapped(monkeypatch: pytest.MonkeyPatch) -> None:
    for var in ("NANNY_BRIDGE_SOCKET", "NANNY_BRIDGE_PORT", "NANNY_BRIDGE_ADDR"):
        monkeypatch.delenv(var, raising=False)
    client = FakeOpenAI()
    assert instrument(client) is client
    # Not wrapped: wrapping sets `create` on the instance; passthrough does not.
    assert "create" not in vars(client.chat.completions)


# ── Provider coverage (sync) ──────────────────────────────────────────────────


def test_openai_reports_usage(mock_bridge: HTTPServer) -> None:
    _expect_usage(mock_bridge)
    client = instrument(FakeOpenAI())
    client.chat.completions.create(messages=[])
    assert _usage_posts(mock_bridge) == [{"input": 10, "output": 5, "provider": "openai"}]


def test_deepseek_reports_cache_usage(mock_bridge: HTTPServer) -> None:
    """DeepSeek's own hit/miss fields end up on the wire as generic cache_read
    — the whole point of the generic field design: no Nanny-side knowledge of
    DeepSeek's specific vocabulary, just the two neutral fields every
    provider's extraction converges on."""
    _expect_usage(mock_bridge)

    class _DeepSeekCompletions:
        def create(self, **kwargs: Any) -> Any:
            return _Resp(
                usage=_Usage(
                    prompt_tokens=10,
                    completion_tokens=5,
                    prompt_cache_hit_tokens=7,
                )
            )

    class _DeepSeekChat:
        def __init__(self) -> None:
            self.completions = _DeepSeekCompletions()

    class _DeepSeekClient:
        base_url = "https://api.deepseek.com/v1"

        def __init__(self) -> None:
            self.chat = _DeepSeekChat()

    client = instrument(_DeepSeekClient())
    client.chat.completions.create(messages=[])
    assert _usage_posts(mock_bridge) == [
        {"input": 10, "output": 5, "provider": "deepseek", "cache_read": 7}
    ]


def test_anthropic_reports_usage(mock_bridge: HTTPServer) -> None:
    _expect_usage(mock_bridge)
    client = instrument(FakeAnthropic())
    client.messages.create(messages=[])
    assert _usage_posts(mock_bridge) == [{"input": 8, "output": 3, "provider": "anthropic"}]


def test_mistral_reports_usage(mock_bridge: HTTPServer) -> None:
    _expect_usage(mock_bridge)
    client = instrument(FakeMistral())
    client.chat.complete(messages=[])
    assert _usage_posts(mock_bridge) == [{"input": 7, "output": 2, "provider": "mistral"}]


def test_gemini_reports_usage(mock_bridge: HTTPServer) -> None:
    _expect_usage(mock_bridge)
    client = instrument(FakeGemini())
    client.models.generate_content(contents="hi")
    assert _usage_posts(mock_bridge) == [{"input": 6, "output": 4, "provider": "gemini"}]


def test_cohere_reports_usage(mock_bridge: HTTPServer) -> None:
    _expect_usage(mock_bridge)
    client = instrument(FakeCohere())
    client.chat(messages=[])
    assert _usage_posts(mock_bridge) == [{"input": 9, "output": 1, "provider": "cohere"}]


# ── Model + provider + harness attribution ───────────────────────────────────


def test_reports_model_provider_and_harness(
    mock_bridge: HTTPServer, monkeypatch: pytest.MonkeyPatch
) -> None:
    _expect_usage(mock_bridge)
    import sys

    # `nanny_sdk.instrument` the attribute is the re-exported function; the module
    # object lives in sys.modules under the same dotted name.
    m = sys.modules["nanny_sdk.instrument"]

    # Force a known harness (detection is heuristic; pin it for a deterministic body).
    monkeypatch.setattr(m, "_detect_harness", lambda: "opencode")

    class _ModelResp:
        model = "gpt-4o"
        usage = _Usage(prompt_tokens=10, completion_tokens=5)

    class _Completions:
        def create(self, **kwargs: Any) -> Any:
            return _ModelResp()

    class _Chat:
        def __init__(self) -> None:
            self.completions = _Completions()

    class _GroqClient:
        # base_url refines the OpenAI-compatible provider to "groq".
        base_url = "https://api.groq.com/openai/v1"

        def __init__(self) -> None:
            self.chat = _Chat()

    client = instrument(_GroqClient())
    client.chat.completions.create(messages=[])
    assert _usage_posts(mock_bridge) == [
        {
            "input": 10,
            "output": 5,
            "model": "gpt-4o",
            "provider": "groq",
            "harness": {"name": "opencode"},
        }
    ]


# ── Streaming ─────────────────────────────────────────────────────────────────


def test_sync_stream_defers_then_reports_once(mock_bridge: HTTPServer) -> None:
    _expect_usage(mock_bridge)
    client = instrument(FakeStreamingOpenAI())
    stream = client.chat.completions.create(stream=True)
    assert _usage_posts(mock_bridge) == []  # nothing submitted until consumed
    chunks = list(stream)
    assert len(chunks) == 2
    assert _usage_posts(mock_bridge) == [{"input": 10, "output": 5, "provider": "openai"}]


async def test_async_reports_usage(mock_bridge: HTTPServer) -> None:
    _expect_usage(mock_bridge)
    client = instrument(FakeAsyncOpenAI())
    await client.chat.completions.create(messages=[])
    assert _usage_posts(mock_bridge) == [{"input": 12, "output": 6, "provider": "openai"}]


async def test_async_stream_reports_once(mock_bridge: HTTPServer) -> None:
    _expect_usage(mock_bridge)
    client = instrument(FakeAsyncStreamingOpenAI())
    stream = await client.chat.completions.create(stream=True)
    chunks = [chunk async for chunk in stream]
    assert len(chunks) == 2
    assert _usage_posts(mock_bridge) == [{"input": 11, "output": 4, "provider": "openai"}]


# ── Idempotency + zero-usage ──────────────────────────────────────────────────


def test_double_instrument_reports_once(mock_bridge: HTTPServer) -> None:
    _expect_usage(mock_bridge)
    client = FakeOpenAI()
    instrument(client)
    instrument(client)  # second call must be a no-op (no double-wrap)
    client.chat.completions.create(messages=[])
    assert _usage_posts(mock_bridge) == [{"input": 10, "output": 5, "provider": "openai"}]


def test_zero_usage_not_submitted(mock_bridge: HTTPServer) -> None:
    _expect_usage(mock_bridge)

    class _ZeroClient:
        class _Completions:
            def create(self, **kwargs: Any) -> Any:
                return _Resp(usage=_Usage(prompt_tokens=0, completion_tokens=0))

        class _Chat:
            def __init__(self) -> None:
                self.completions = _ZeroClient._Completions()

        def __init__(self) -> None:
            self.chat = _ZeroClient._Chat()

    client = instrument(_ZeroClient())
    client.chat.completions.create(messages=[])
    assert _usage_posts(mock_bridge) == []
