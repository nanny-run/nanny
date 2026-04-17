"""load_metrics — read a metrics CSV and return a summary dict."""

import json

import pandas as pd
from crewai.tools import tool as crew_tool

from nanny_sdk import tool as nanny_tool


@crew_tool
@nanny_tool(cost=3)
def load_metrics(path: str) -> str:
    """Load a metrics CSV from path and return a JSON summary.

    Returns row count, time range, column names, and per-column min/max/mean.
    """
    df = pd.read_csv(path, parse_dates=["timestamp"])
    summary = {
        "rows":       len(df),
        "columns":    list(df.columns),
        "time_range": {
            "start": str(df["timestamp"].min()),
            "end":   str(df["timestamp"].max()),
        },
        "stats": {
            col: {
                "min":  round(float(df[col].min()), 4),
                "max":  round(float(df[col].max()), 4),
                "mean": round(float(df[col].mean()), 4),
            }
            for col in df.select_dtypes("number").columns
        },
    }
    return json.dumps(summary, indent=2)
