"""Analysis agent — per-signal statistics, anomaly detection, correlation, synthesis.

Five single-tool tasks + one synthesis task:
    compute_stats_task(metric)    — calls compute_stats for one metric.
    detect_anomalies_task(metric) — calls detect_anomalies for one metric.
    correlate_signals_task        — calls correlate_signals for all signals.
    synthesis_task                — pure LLM synthesis, no tool call.

Single-tool tasks mean the LLM has exactly one job per task. It cannot
hallucinate past a task description that says "call this one tool with
these exact arguments." CrewAI dispatches the call; @nanny_tool fires
on every dispatch — guaranteed enforcement regardless of model.

Exports:
    analysis_agent          — CrewAI Agent with compute_stats, detect_anomalies,
                              correlate_signals tools.
    compute_stats_task()    — factory for one compute_stats call.
    detect_anomalies_task() — factory for one detect_anomalies call.
    correlate_signals_task()— factory for the correlation call.
    synthesis_task()        — factory for the pure-synthesis reasoning task.
"""

from crewai import Agent, Task

from metrics_crew.config import make_llm
from metrics_crew.tools import compute_stats, correlate_signals, detect_anomalies

_llm = make_llm()

analysis_agent = Agent(
    role="Metrics Analyst",
    goal=(
        "Compute statistics, detect anomalies, and correlate signals to identify "
        "the incident window, affected metrics, and a root-cause hypothesis."
    ),
    backstory=(
        "You are a senior SRE analyst specialising in time-series anomaly detection. "
        "You call exactly one tool per task — you do not chain tool calls in a single task. "
        "After all data is collected in earlier tasks, you synthesise the findings "
        "into a structured incident analysis."
    ),
    llm=_llm,
    tools=[compute_stats, detect_anomalies, correlate_signals],
    verbose=True,
    allow_delegation=False,
)


def compute_stats_task(
    metric: str,
    data_path: str,
    context: list | None = None,
) -> Task:
    """Return a Task that calls compute_stats for one metric."""
    return Task(
        description=(
            f"Call compute_stats with metric='{metric}' and path='{data_path}'. "
            "Return the descriptive statistics JSON for this metric exactly as the "
            "tool returns it."
        ),
        expected_output=(
            f"Descriptive statistics JSON for '{metric}': "
            "mean, std, p25, p50, p75, p95, min, max, count."
        ),
        agent=analysis_agent,
        tools=[compute_stats],
        context=context or [],
    )


def detect_anomalies_task(
    metric: str,
    data_path: str,
    context: list | None = None,
) -> Task:
    """Return a Task that calls detect_anomalies for one metric."""
    return Task(
        description=(
            f"Call detect_anomalies with metric='{metric}' and path='{data_path}'. "
            "Return the anomaly detection JSON including spike count, timestamps, "
            "values, and z-scores exactly as the tool returns it."
        ),
        expected_output=(
            f"Anomaly detection JSON for '{metric}': "
            "threshold, mean, std, spike_count, and up to 20 spike entries "
            "with timestamp, value, and z_score."
        ),
        agent=analysis_agent,
        tools=[detect_anomalies],
        context=context or [],
    )


def correlate_signals_task(
    data_path: str,
    context: list | None = None,
) -> Task:
    """Return a Task that calls correlate_signals for cpu, error_rate, and latency."""
    return Task(
        description=(
            f"Call correlate_signals with metrics='cpu,error_rate,latency' "
            f"and path='{data_path}'. "
            "Return the Pearson correlation matrix JSON exactly as the tool returns it."
        ),
        expected_output=(
            "Pearson correlation matrix JSON for cpu, error_rate, and latency: "
            "a nested object mapping each metric pair to a correlation coefficient."
        ),
        agent=analysis_agent,
        tools=[correlate_signals],
        context=context or [],
    )


def synthesis_task(context: list | None = None) -> Task:
    """Return a pure-synthesis Task: LLM reasoning only, no tool call."""
    return Task(
        description=(
            "Using the statistics, anomaly detection results, and correlation matrix "
            "from the previous tasks, synthesise the incident analysis. "
            "Do not call any tools — reason from the data already collected.\n\n"
            "Produce a structured analysis covering:\n"
            "1. Incident start and end timestamps\n"
            "2. Affected signals with their peak values\n"
            "3. Key correlation findings between signals\n"
            "4. Root-cause hypothesis\n"
        ),
        expected_output=(
            "A structured incident analysis: "
            "start/end timestamps, affected signals with peak values, "
            "correlation findings, and a root-cause hypothesis."
        ),
        agent=analysis_agent,
        tools=[],
        context=context or [],
    )
