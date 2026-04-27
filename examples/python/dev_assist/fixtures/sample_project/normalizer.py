Root cause: The 'user_id' field is present in the incoming request but has no entry in the key_map dictionary used by the RecordNormalizer class.
Files involved: fixtures/sample_project/normalizer.py, fixtures/sample_project/pipeline.py, fixtures/sample_project/auth.py
Fix: Add 'user_id' to the _SCHEMA dictionary in pipeline.py with its corresponding canonical name, for example: 'user_id': 'user_identifier'. The updated _SCHEMA dictionary would look like this:
```python
_SCHEMA = {
    "email": "contact_email",
    "name": "full_name",
    "account": "account_id",
    "session": "session_token",
    "user_id": "user_identifier",
}
```