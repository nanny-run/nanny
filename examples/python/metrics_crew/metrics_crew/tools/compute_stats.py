"""compute_stats — mean / std / p95 for a single metric column."""

import json

import pandas as pd
from crewai.tools import tool as crew_tool

from nanny_sdk import tool as nanny_tool


@crew_tool
@nanny_tool(cost=10)
def compute_stats(metric: str, path: str) -> str:
    """Compute descriptive statistics for one metric column in the CSV at path.

    Returns mean, std, p25, p50, p75, p95, min, and max as a JSON string.
    metric must be one of: cpu, memory, request_rate, error_rate, latency.
    """
    df = pd.read_csv(path)
    if metric not in df.columns:
        return f"ERROR — column '{metric}' not found. Available: {list(df.columns)}"
    s = df[metric]
    result = {
        "metric": metric,
        "count":  int(s.count()),
        "mean":   round(float(s.mean()), 4),
        "std":    round(float(s.std()), 4),
        "min":    round(float(s.min()), 4),
        "p25":    round(float(s.quantile(0.25)), 4),
        "p50":    round(float(s.quantile(0.50)), 4),
        "p75":    round(float(s.quantile(0.75)), 4),
        "p95":    round(float(s.quantile(0.95)), 4),
        "max":    round(float(s.max()), 4),
    }
    return json.dumps(result, indent=2)
