"""Centralised configuration for metrics_crew.

All agents and tools import from here — no magic strings scattered across files.
"""

# ── LLM ───────────────────────────────────────────────────────────────────────
# Default: Groq free tier — reliable structured function calling, no cost.
# Requires: export GROQ_API_KEY=<your_key>  (console.groq.com, no credit card)
#
# Offline/local fallback — edit the two lines below:
#   MODEL = "ollama/qwen2.5:7b"
#   OLLAMA_BASE_URL = "http://localhost:11434"
# Then: ollama pull qwen2.5:7b && ollama serve
# And in each agent file change: LLM(model=MODEL) → LLM(model=MODEL, base_url=OLLAMA_BASE_URL)

MODEL = "groq/llama-3.3-70b-versatile"

# ── Metrics constants ─────────────────────────────────────────────────────────

# Columns that every valid metrics CSV must contain.
REQUIRED_COLUMNS = ["timestamp", "cpu", "memory", "request_rate", "error_rate", "latency"]

# Z-score threshold for anomaly detection: deviations above this are flagged.
ANOMALY_Z_THRESHOLD = 2.5

# Default output directory for charts and reports.
DEFAULT_OUTPUT_DIR = "reports"
