"""Request middleware — validates and preprocesses the request body
before it is handed to the processing pipeline.
"""


class RequestMiddleware:
    """Validates required fields are present before forwarding to the pipeline."""

    REQUIRED_FIELDS = ["email", "account"]

    def __init__(self, pipeline):
        self.pipeline = pipeline

    def preprocess(self, request: dict) -> dict:
        """Validate then forward the request."""
        body = request.get("body", {})
        missing = [f for f in self.REQUIRED_FIELDS if f not in body]
        if missing:
            raise ValueError(f"Request missing required fields: {missing}")
        return self.pipeline.process(request)
