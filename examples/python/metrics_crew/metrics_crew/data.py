"""Synthetic metrics data generator.

Produces a realistic 6-hour time series with one injected spike incident.
The spike is deterministic — same seed, same data — so the sample CSV
can be committed and the analysis agents always find the same anomaly.

Usage
-----
    # Generate and save the sample fixture (run once):
    python -m metrics_crew.data

    # Load in agent/tool code:
    from metrics_crew.data import load_csv
    df = load_csv("fixtures/sample_metrics.csv")
"""

from __future__ import annotations

import random
from pathlib import Path

import pandas as pd

# ── Constants ─────────────────────────────────────────────────────────────────

# Spike injected at minute 210 out of 360 (3h30m into a 6-hour window).
SPIKE_MINUTE = 210
SEED = 42


# ── Generator ─────────────────────────────────────────────────────────────────


def generate_sample_metrics(seed: int = SEED) -> pd.DataFrame:
    """Generate a 6-hour metrics time series at 1-minute resolution.

    Returns a DataFrame with columns:
        timestamp     — ISO-8601 UTC string, one minute apart
        cpu           — CPU utilisation (0–100 %)
        memory        — Memory utilisation (0–100 %)
        request_rate  — Requests per second
        error_rate    — Error rate (0.0–1.0)
        latency       — p50 response time in milliseconds

    A single incident spike is injected at ``SPIKE_MINUTE``:
        - cpu bumped to ~92 %
        - error_rate jumps to ~0.35
        - latency jumps to ~820 ms
        - request_rate drops ~40 %
    """
    rng = random.Random(seed)
    minutes = 360  # 6 hours

    base_time = pd.Timestamp("2024-11-14 08:00:00", tz="UTC")
    timestamps = [base_time + pd.Timedelta(minutes=i) for i in range(minutes)]

    rows = []
    for i in range(minutes):
        is_spike = (SPIKE_MINUTE <= i < SPIKE_MINUTE + 12)  # 12-minute incident window

        cpu = rng.gauss(45, 8) + (48 if is_spike else 0)
        memory = rng.gauss(60, 5) + (10 if is_spike else 0)
        request_rate = rng.gauss(120, 15) - (50 if is_spike else 0)
        error_rate = rng.gauss(0.02, 0.005) + (0.33 if is_spike else 0)
        latency = rng.gauss(180, 20) + (640 if is_spike else 0)

        rows.append({
            "timestamp":    timestamps[i].isoformat(),
            "cpu":          round(max(0.0, min(100.0, cpu)), 2),
            "memory":       round(max(0.0, min(100.0, memory)), 2),
            "request_rate": round(max(0.0, request_rate), 2),
            "error_rate":   round(max(0.0, min(1.0, error_rate)), 4),
            "latency":      round(max(0.0, latency), 2),
        })

    return pd.DataFrame(rows)


# ── I/O helpers ───────────────────────────────────────────────────────────────


def save_csv(df: pd.DataFrame, path: str | Path) -> None:
    """Write ``df`` to ``path`` as a UTF-8 CSV (no index column)."""
    Path(path).parent.mkdir(parents=True, exist_ok=True)
    df.to_csv(path, index=False)


def load_csv(path: str | Path) -> pd.DataFrame:
    """Load a metrics CSV from ``path``, parsing the timestamp column."""
    return pd.read_csv(path, parse_dates=["timestamp"])


# ── CLI entry point ───────────────────────────────────────────────────────────


if __name__ == "__main__":
    out = Path("fixtures/sample_metrics.csv")
    df = generate_sample_metrics()
    save_csv(df, out)
    spike_start = df.iloc[SPIKE_MINUTE]["timestamp"]
    print(f"Generated {len(df)} rows → {out}")
    print(f"Spike window: rows {SPIKE_MINUTE}–{SPIKE_MINUTE + 11} (starts {spike_start})")
    print(f"Shape: {df.shape}")
    print(df.describe().to_string())
