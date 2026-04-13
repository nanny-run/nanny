"""Data processing pipeline."""

from normalizer import RecordNormalizer


class Pipeline:
    def __init__(self):
        self.normalizer = RecordNormalizer(
            key_map={
                "email": "contact_email",
                "name": "full_name",
                "account": "account_id",
                # BUG: "user_id" is missing from key_map.
                # Requests from the new auth service include "user_id"
                # but the normalizer was written before that field existed.
            }
        )

    def process(self, request: dict) -> dict:
        return self._run_stage(request)

    def _run_stage(self, data: dict) -> dict:
        return self.normalizer.apply(data)
