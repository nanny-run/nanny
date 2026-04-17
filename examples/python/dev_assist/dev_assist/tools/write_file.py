"""write_file tool — intentionally absent from [tools] allowed in nanny.toml.

Registered so the LLM can hallucinate it naturally; ToolDenied fires before
the file is ever written. This is how the ToolDenied stop reason is demonstrated.
"""

from langchain_core.tools import tool as lc_tool
from nanny_sdk import tool as nanny_tool


@lc_tool
@nanny_tool(cost=5)
def write_file(path: str, content: str) -> str:
    """Write content to a file at path."""
    with open(path, "w") as f:
        f.write(content)
    return f"Written {len(content)} bytes to {path}"
