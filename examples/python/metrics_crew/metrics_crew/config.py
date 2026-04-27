"""Centralised configuration for metrics_crew.

All agents and tools import from here — no magic strings scattered across files.
"""

from crewai import LLM

# ── LLM ───────────────────────────────────────────────────────────────────────
# Default: OpenAI gpt-4.1-nano — 1M context window, excellent tool calling.
# Requires: OPENAI_API_KEY=<your_key>  (platform.openai.com)
#
# Offline/local fallback — edit the two lines below:
#   MODEL = "ollama/qwen2.5:7b"
#   OLLAMA_BASE_URL = "http://localhost:11434"
# Then: ollama pull qwen2.5:7b && ollama serve
# And change make_llm(): LLM(model=MODEL) → LLM(model=MODEL, base_url=OLLAMA_BASE_URL)

MODEL = "gpt-4.1-nano"


def make_llm() -> LLM:
    return LLM(model=MODEL)

# ── Metrics constants ─────────────────────────────────────────────────────────


# Columns that every valid metrics CSV must contain.
REQUIRED_COLUMNS = ["timestamp", "cpu", "memory",
                    "request_rate", "error_rate", "latency"]

# Z-score threshold for anomaly detection: deviations above this are flagged.
ANOMALY_Z_THRESHOLD = 2.5

# Default output directory for charts and reports.
DEFAULT_OUTPUT_DIR = "reports"
