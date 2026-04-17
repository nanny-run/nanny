"""API server — request handling entry point."""

from middleware import RequestMiddleware
from pipeline import Pipeline

_pipeline = Pipeline()
_middleware = RequestMiddleware(_pipeline)


def handle_request(request: dict) -> dict:
    """Route an authenticated request through the middleware and pipeline."""
    return _middleware.preprocess(request)
