"""Analysis agent — statistical analysis and anomaly detection.

Exports:
    analysis_agent  — CrewAI Agent with compute_stats, detect_anomalies,
                      and correlate_signals tools.
    analysis_task() — factory that returns the corresponding Task.
"""

from crewai import Agent, LLM, Task

from metrics_crew.config import MODEL, OLLAMA_BASE_URL
from metrics_crew.tools import compute_stats, correlate_signals, detect_anomalies

_llm = LLM(model=f"ollama/{MODEL}", base_url=OLLAMA_BASE_URL)

analysis_agent = Agent(
    role="Metrics Analyst",
    goal=(
        "Identify the incident window, pinpoint the affected signals, determine "
        "their correlations, and produce a clear root-cause hypothesis."
    ),
    backstory=(
        "You are a senior SRE analyst specialising in time-series anomaly detection. "
        "You use tools to gather data — you never describe or simulate tool calls, "
        "you always invoke them directly."
    ),
    llm=_llm,
    tools=[compute_stats, detect_anomalies, correlate_signals],
    verbose=True,
    allow_delegation=False,
)


def analysis_task(data_path: str, context: list | None = None) -> Task:
    """Return the analysis Task for the given CSV path."""
    return Task(
        description=(
            f"Call these tools in order, one at a time. Use path='{data_path}' for all.\n"
            f"1. compute_stats(metric='cpu', path='{data_path}')\n"
            f"2. compute_stats(metric='error_rate', path='{data_path}')\n"
            f"3. detect_anomalies(metric='cpu', path='{data_path}')\n"
            f"4. detect_anomalies(metric='error_rate', path='{data_path}')\n"
            f"5. correlate_signals(metrics='cpu,error_rate,latency', path='{data_path}')\n"
            f"After all tool calls complete, summarise: incident window, affected signals, "
            f"correlations, and root cause hypothesis."
        ),
        expected_output=(
            "A structured incident analysis containing: incident start/end timestamps, "
            "affected signals with peak values, correlation findings, "
            "and a root-cause hypothesis."
        ),
        agent=analysis_agent,
        context=context or [],
    )
