"""Authentication — decodes request tokens and injects user identity into the request body."""

import base64
import json


def extract_user(request: dict) -> dict:
    """Decode the Authorization token and merge user claims into the request body.

    Called by the router before the request is dispatched to the server.
    Existing body fields are preserved; token claims are merged on top.
    """
    token = request.get("headers", {}).get("Authorization", "")
    if not token:
        return request
    user_data = _decode_token(token)
    request["body"] = {**request.get("body", {}), **user_data}
    return request


def _decode_token(token: str) -> dict:
    """Decode a Bearer token and return the full user claims payload.

    The v2 auth service encodes a richer payload than v1:
      - sub       → user_id (stable UUID, replaces the old numeric id)
      - email     → contact address
      - account   → billing account slug
      - role      → access role (user | admin | service)

    v1 tokens only carried email and account. v2 tokens carry all four.
    """
    if token.startswith("Bearer "):
        token = token[7:]
    parts = token.split(".")
    if len(parts) == 3:
        payload = _decode_payload(parts[1])
    else:
        payload = {"email": token, "account": "acct_001"}

    return {
        "user_id": payload.get("sub", ""),
        "email": payload.get("email", "user@example.com"),
        "account": payload.get("account", "acct_001"),
        "role": payload.get("role", "user"),
    }


def _decode_payload(b64_segment: str) -> dict:
    """Base64url-decode a JWT payload segment and return the claims dict."""
    padding = 4 - len(b64_segment) % 4
    try:
        raw = base64.urlsafe_b64decode(b64_segment + "=" * padding)
        return json.loads(raw)
    except Exception:
        return {}
