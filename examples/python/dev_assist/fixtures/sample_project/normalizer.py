"""Record normalizer — maps raw request fields to canonical schema names."""


class RecordNormalizer:
    def __init__(self, key_map: dict):
        self.key_map = key_map

    def apply(self, data: dict) -> dict:
        return self._normalize_record(data)

    def _normalize_record(self, record: dict) -> dict:
        """Remap record keys according to key_map.

        Iterates over every key in the incoming record and maps it
        to its canonical name. Raises KeyError if a field arrives
        that is not present in key_map.
        """
        normalized = {}
        for field in record:
            # This line raises KeyError when a field arrives that
            # was not anticipated in key_map — e.g. "user_id".
            value = record[self.key_map[field]]
            normalized[field] = value
        return normalized
