"""Centralised configuration for metrics_crew.

All agents and tools import from here — no magic strings scattered across files.
"""

# ── LLM ───────────────────────────────────────────────────────────────────────

MODEL = "llama3.1:8b"
OLLAMA_BASE_URL = "http://localhost:11434"

# ── Metrics constants ─────────────────────────────────────────────────────────

# Columns that every valid metrics CSV must contain.
REQUIRED_COLUMNS = ["timestamp", "cpu", "memory", "request_rate", "error_rate", "latency"]

# Z-score threshold for anomaly detection: deviations above this are flagged.
ANOMALY_Z_THRESHOLD = 2.5

# Default output directory for charts and reports.
DEFAULT_OUTPUT_DIR = "reports"
