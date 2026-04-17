"""Record normalizer — maps incoming request fields to the canonical schema."""


class RecordNormalizer:
    def __init__(self, key_map: dict, strict: bool = True):
        self.key_map = key_map
        self.strict = strict

    def apply(self, data: dict) -> dict:
        validated = self._validate(data)
        return self._normalize_record(validated)

    def _validate(self, record: dict) -> dict:
        """Pre-validate the record before normalization."""
        if not isinstance(record, dict):
            raise TypeError(f"Expected dict, got {type(record).__name__}")
        return record

    def _normalize_record(self, record: dict) -> dict:
        """Remap record keys using key_map.

        Iterates over every key in the incoming record and maps it
        to its canonical name. Raises KeyError if a field is present
        in the record but has no entry in key_map.
        """
        normalized = {}
        for field in record:
            value = record[self.key_map[field]]
            normalized[field] = value
        return normalized
