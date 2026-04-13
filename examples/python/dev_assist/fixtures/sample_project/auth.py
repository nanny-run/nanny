"""Auth middleware — extracts user identity from request headers."""


def extract_user(request: dict) -> dict:
    """Pull user fields from the Authorization header and inject into request body."""
    token = request.get("headers", {}).get("Authorization", "")
    if not token:
        return request

    # Decode token and inject user fields into the request body
    # so downstream pipeline stages can access them.
    user_data = _decode_token(token)
    request["body"] = {**request.get("body", {}), **user_data}
    return request


def _decode_token(token: str) -> dict:
    """Minimal JWT-lite decode — returns payload dict."""
    # Real implementation would verify signature.
    # Injects user_id, email, and account into the request body.
    return {
        "user_id": token.split(".")[1] if "." in token else token,
        "email": "user@example.com",
        "account": "acct_001",
    }
