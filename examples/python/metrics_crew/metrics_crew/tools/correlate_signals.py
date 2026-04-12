"""correlate_signals — Pearson correlation matrix for a set of metric columns."""

import json

import pandas as pd
from crewai.tools import tool as crew_tool

from nanny_sdk import tool as nanny_tool


@crew_tool
@nanny_tool(cost=15)
def correlate_signals(metrics: str, path: str) -> str:
    """Compute Pearson correlation between a set of metric columns.

    metrics is a comma-separated list of column names, e.g. "cpu,error_rate,latency".
    Returns the full correlation matrix as a JSON object.
    """
    df = pd.read_csv(path)
    cols = [c.strip() for c in metrics.split(",") if c.strip()]

    missing = [c for c in cols if c not in df.columns]
    if missing:
        return f"ERROR — columns not found: {missing}. Available: {list(df.columns)}"
    if len(cols) < 2:
        return "ERROR — provide at least 2 metric names separated by commas."

    corr = df[cols].corr().round(4)
    result = {
        "metrics":     cols,
        "correlation": {
            row: {col: float(corr.loc[row, col]) for col in cols}
            for row in cols
        },
    }
    return json.dumps(result, indent=2)
