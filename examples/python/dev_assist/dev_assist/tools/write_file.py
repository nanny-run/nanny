"""write_file tool — intentionally absent from [tools] allowed in nanny.toml.

Registered so the agent can attempt it; ToolDenied fires before
the file is ever written. Demonstrates allowlist enforcement.
"""

from nanny_sdk import tool


@tool(cost=5)
def write_file(path: str, content: str) -> str:
    """Write content to a file at path."""
    with open(path, "w") as f:
        f.write(content)
    return f"Written {len(content)} bytes to {path}"
