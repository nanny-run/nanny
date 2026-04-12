"""Request router — routes incoming requests to the correct handler."""

from auth import extract_user
from server import handle_request


def route(raw_request: dict) -> dict:
    """Authenticate then dispatch."""
    request = extract_user(raw_request)
    return handle_request(request)
