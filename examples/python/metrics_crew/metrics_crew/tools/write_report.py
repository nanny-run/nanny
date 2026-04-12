"""write_report — save the final incident Markdown report to disk."""

from datetime import datetime, timezone
from pathlib import Path

from crewai.tools import tool as crew_tool

from nanny_sdk import tool as nanny_tool


@crew_tool
@nanny_tool(cost=5)
def write_report(content: str, output_dir: str = "reports") -> str:
    """Save a Markdown incident report to output_dir/incident_<timestamp>.md.

    content is the full Markdown text of the report.
    Returns the path of the saved file.
    """
    ts  = datetime.now(tz=timezone.utc).strftime("%Y%m%dT%H%M%SZ")
    out_dir = Path(output_dir)
    out_dir.mkdir(parents=True, exist_ok=True)
    out_path = out_dir / f"incident_{ts}.md"
    out_path.write_text(content, encoding="utf-8")
    return str(out_path)
