"""validate_schema — verify required columns exist in a metrics CSV."""

import pandas as pd
from crewai.tools import tool as crew_tool

from metrics_crew.config import REQUIRED_COLUMNS
from nanny_sdk import tool as nanny_tool


@crew_tool
@nanny_tool(cost=1)
def validate_schema(path: str) -> str:
    """Validate that the metrics CSV at path has all required columns.

    Returns 'OK' with the column list, or lists the missing columns.
    """
    df = pd.read_csv(path, nrows=0)  # header only, no data loaded
    present = set(df.columns)
    missing = [c for c in REQUIRED_COLUMNS if c not in present]
    if missing:
        return f"SCHEMA ERROR — missing columns: {missing}. Found: {sorted(present)}"
    return f"OK — all required columns present: {REQUIRED_COLUMNS}"
