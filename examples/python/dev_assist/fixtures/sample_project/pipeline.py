"""Data processing pipeline."""

from normalizer import RecordNormalizer


class Pipeline:
    _SCHEMA = {
        "email": "contact_email",
        "name": "full_name",
        "account": "account_id",
        # new auth service fields added in v2.3 migration
        "session": "session_token",
    }

    def __init__(self, strict: bool = True):
        self.normalizer = RecordNormalizer(
            key_map=self._SCHEMA,
            strict=strict,
        )
        self._processed = 0

    def process(self, request: dict) -> dict:
        body = request.get("body", request)
        return self._run_stage(body)

    def _run_stage(self, data: dict) -> dict:
        self._processed += 1
        return self.normalizer.apply(data)
