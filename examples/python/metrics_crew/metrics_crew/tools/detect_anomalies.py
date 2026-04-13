"""detect_anomalies — z-score spike detection for a single metric."""

import json

import pandas as pd
from crewai.tools import tool as crew_tool

from metrics_crew.config import ANOMALY_Z_THRESHOLD
from nanny_sdk import tool as nanny_tool


@crew_tool
@nanny_tool(cost=10)
def detect_anomalies(metric: str, path: str) -> str:
    """Detect anomalous spikes in one metric column using z-score thresholding.

    Returns a JSON list of spike windows: each entry has the timestamp, value,
    and z-score for points exceeding the configured threshold.
    metric must be one of: cpu, memory, request_rate, error_rate, latency.
    """
    df = pd.read_csv(path, parse_dates=["timestamp"])
    if metric not in df.columns:
        return f"ERROR — column '{metric}' not found. Available: {list(df.columns)}"

    s = df[metric]
    mean = s.mean()
    std  = s.std()

    if std == 0:
        return json.dumps({"metric": metric, "spikes": [], "note": "zero variance"})

    z = (s - mean) / std
    spikes_idx = z[z.abs() > ANOMALY_Z_THRESHOLD].index

    spikes = [
        {
            "timestamp": str(df.loc[i, "timestamp"]),
            "value":     round(float(df.loc[i, metric]), 4),
            "z_score":   round(float(z.loc[i]), 4),
        }
        for i in spikes_idx
    ]

    return json.dumps(
        {
            "metric":     metric,
            "threshold":  ANOMALY_Z_THRESHOLD,
            "mean":       round(float(mean), 4),
            "std":        round(float(std), 4),
            "spike_count": len(spikes),
            "spikes":     spikes[:20],  # cap at 20 to keep output readable
        },
        indent=2,
    )
