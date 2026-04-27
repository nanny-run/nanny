"""metrics_crew agents — one file per role, single-tool tasks per action."""

from metrics_crew.agents.analysis import (
    analysis_agent,
    compute_stats_task,
    correlate_signals_task,
    detect_anomalies_task,
    synthesis_task,
)
from metrics_crew.agents.ingestion import (
    ingestion_agent,
    load_metrics_task,
    validate_schema_task,
)
from metrics_crew.agents.reporter import reporter_agent, write_report_task
from metrics_crew.agents.visualization import generate_chart_task, viz_agent

__all__ = [
    # Agents
    "ingestion_agent",
    "analysis_agent",
    "viz_agent",
    "reporter_agent",
    # Ingestion tasks
    "validate_schema_task",
    "load_metrics_task",
    # Analysis tasks
    "compute_stats_task",
    "detect_anomalies_task",
    "correlate_signals_task",
    "synthesis_task",
    # Visualisation tasks
    "generate_chart_task",
    # Report task
    "write_report_task",
]
