"""Ingestion agent — validates schema and loads metrics data.

Two single-tool tasks:
    validate_schema_task — calls validate_schema to confirm CSV structure.
    load_metrics_task    — calls load_metrics to summarise the dataset.

Each task has exactly one tool so the LLM has exactly one job.
CrewAI dispatches the tool call; @nanny_tool fires on every call — guaranteed.

Exports:
    ingestion_agent        — CrewAI Agent with validate_schema + load_metrics tools.
    validate_schema_task() — factory for the schema validation task.
    load_metrics_task()    — factory for the dataset loading task.
"""

from crewai import Agent, Task

from metrics_crew.config import make_llm
from metrics_crew.tools import load_metrics, validate_schema

_llm = make_llm()

ingestion_agent = Agent(
    role="Data Ingestion Specialist",
    goal=(
        "Load the metrics dataset and confirm it is valid and complete "
        "before any analysis begins."
    ),
    backstory=(
        "You are a data ingestion expert embedded in an SRE team. "
        "Your job is to confirm that raw telemetry data arrives clean "
        "and complete. You call one tool per task — no skipping ahead."
    ),
    llm=_llm,
    tools=[validate_schema, load_metrics],
    verbose=True,
    allow_delegation=False,
)


def validate_schema_task(data_path: str) -> Task:
    """Return a Task that calls validate_schema for the CSV at data_path."""
    return Task(
        description=(
            f"Call validate_schema with path='{data_path}' to confirm the metrics CSV "
            "has all required columns. Report whether the schema is valid or list "
            "any missing columns."
        ),
        expected_output=(
            "Schema validation result: 'OK — all required columns present' "
            "or a list of missing columns."
        ),
        agent=ingestion_agent,
        tools=[validate_schema],
    )


def load_metrics_task(data_path: str, context: list | None = None) -> Task:
    """Return a Task that calls load_metrics for the CSV at data_path."""
    return Task(
        description=(
            f"Call load_metrics with path='{data_path}' to load the metrics dataset. "
            "Return the JSON summary: row count, time range, column names, "
            "and per-column min/max/mean."
        ),
        expected_output=(
            "A JSON summary of the dataset: row count, time range (start/end), "
            "column names, and per-signal min/max/mean statistics."
        ),
        agent=ingestion_agent,
        tools=[load_metrics],
        context=context or [],
    )
