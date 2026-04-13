"""API server — request handling entry point."""

from middleware import RequestMiddleware
from pipeline import Pipeline

pipeline = Pipeline()
middleware = RequestMiddleware(pipeline)


def handle_request(request: dict) -> dict:
    """Handle an incoming API request."""
    response = middleware.preprocess(request)
    return response
