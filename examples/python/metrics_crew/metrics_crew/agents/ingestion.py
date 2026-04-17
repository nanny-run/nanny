"""Ingestion agent — loads and validates the metrics CSV.

Exports:
    ingestion_agent  — CrewAI Agent with load_metrics + validate_schema tools.
    ingestion_task() — factory that returns the corresponding Task.
"""

from crewai import Agent, LLM, Task

from metrics_crew.config import MODEL, OLLAMA_BASE_URL
from metrics_crew.tools import load_metrics, validate_schema

_llm = LLM(model=f"ollama/{MODEL}", base_url=OLLAMA_BASE_URL)

ingestion_agent = Agent(
    role="Data Ingestion Specialist",
    goal=(
        "Load the metrics CSV, validate its schema, and produce a concise summary "
        "of the dataset: row count, time range, and per-signal statistics."
    ),
    backstory=(
        "You are a data ingestion expert responsible for ensuring raw telemetry data "
        "arrives clean and complete before any analysis begins."
    ),
    llm=_llm,
    tools=[load_metrics, validate_schema],
    verbose=True,
    allow_delegation=False,
)


def ingestion_task(data_path: str) -> Task:
    """Return the ingestion Task for the given CSV path."""
    return Task(
        description=(
            f"You must call two tools in order:\n"
            f"1. Call validate_schema with path='{data_path}'\n"
            f"2. Call load_metrics with path='{data_path}'\n"
            f"Return the combined output from both tool calls."
        ),
        expected_output=(
            "The raw output from validate_schema followed by the raw output "
            "from load_metrics."
        ),
        agent=ingestion_agent,
    )
