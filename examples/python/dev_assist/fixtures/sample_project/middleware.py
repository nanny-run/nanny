"""Request middleware — validates, sanitises, and forwards requests to the pipeline."""


class RequestMiddleware:
    """Validates required fields are present before forwarding to the pipeline."""

    REQUIRED_FIELDS = ["email", "account"]
    OPTIONAL_FIELDS = ["name", "role"]

    def __init__(self, pipeline):
        self.pipeline = pipeline

    def preprocess(self, request: dict) -> dict:
        """Strip internal fields, validate required fields, then forward."""
        body = self._extract_body(request)
        self._validate(body)
        clean_request = {**request, "body": body}
        return self.pipeline.process(clean_request)

    def _extract_body(self, request: dict) -> dict:
        """Return the request body, stripping any keys prefixed with underscore."""
        body = request.get("body", {})
        return {k: v for k, v in body.items() if not k.startswith("_")}

    def _validate(self, body: dict) -> None:
        missing = [f for f in self.REQUIRED_FIELDS if f not in body]
        if missing:
            raise ValueError(f"Request missing required fields: {missing}")
