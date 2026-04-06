"""Shared pytest fixtures.

``mock_bridge`` — spins up a fake Nanny bridge via pytest-httpserver and
sets ``NANNY_BRIDGE_PORT`` so _client routes to it. Because _client reads
env vars lazily (at call time), monkeypatch works without reloading modules.
"""

import pytest
from pytest_httpserver import HTTPServer


@pytest.fixture()
def mock_bridge(httpserver: HTTPServer, monkeypatch: pytest.MonkeyPatch) -> HTTPServer:
    """Fake Nanny bridge for unit tests.

    Sets ``NANNY_BRIDGE_PORT`` and ``NANNY_SESSION_TOKEN`` for the duration
    of the test, then restores the original environment on teardown.

    Returns the ``HTTPServer`` so tests can register expected requests::

        def test_something(mock_bridge):
            mock_bridge.expect_request("/health").respond_with_json({"status": "ok"})
            assert client.health() is True
    """
    monkeypatch.setenv("NANNY_BRIDGE_PORT", str(httpserver.port))
    monkeypatch.setenv("NANNY_SESSION_TOKEN", "test-token")
    return httpserver
