"""generate_chart — Plotly time-series chart saved as a self-contained HTML file."""

from pathlib import Path

import pandas as pd
import plotly.graph_objects as go
from crewai.tools import tool as crew_tool

from nanny_sdk import tool as nanny_tool


@crew_tool
@nanny_tool(cost=8)
def generate_chart(metric: str, path: str, output_dir: str = "reports") -> str:
    """Generate a Plotly time-series line chart for one metric and save it as HTML.

    metric must be one of: cpu, memory, request_rate, error_rate, latency.
    The chart is saved to output_dir/chart_<metric>.html.
    Returns the path to the saved HTML file.
    """
    df = pd.read_csv(path, parse_dates=["timestamp"])
    if metric not in df.columns:
        return f"ERROR — column '{metric}' not found. Available: {list(df.columns)}"

    fig = go.Figure()
    fig.add_trace(go.Scatter(
        x=df["timestamp"],
        y=df[metric],
        mode="lines",
        name=metric,
        line={"width": 2},
    ))

    # Highlight anomaly region — any value > mean + 2*std
    mean = df[metric].mean()
    std  = df[metric].std()
    threshold = mean + 2 * std
    spikes = df[df[metric] > threshold]
    if not spikes.empty:
        fig.add_trace(go.Scatter(
            x=spikes["timestamp"],
            y=spikes[metric],
            mode="markers",
            name="anomaly",
            marker={"color": "red", "size": 8, "symbol": "x"},
        ))

    _LABELS = {
        "cpu":          "CPU Utilisation (%)",
        "memory":       "Memory Utilisation (%)",
        "request_rate": "Requests / second",
        "error_rate":   "Error Rate",
        "latency":      "Latency p50 (ms)",
    }
    fig.update_layout(
        title=f"{metric} over time",
        xaxis_title="Time (UTC)",
        yaxis_title=_LABELS.get(metric, metric),
        template="plotly_white",
        legend={"orientation": "h"},
    )

    out_dir = Path(output_dir)
    out_dir.mkdir(parents=True, exist_ok=True)
    out_path = out_dir / f"chart_{metric}.html"
    fig.write_html(str(out_path), include_plotlyjs="cdn")

    return str(out_path)
