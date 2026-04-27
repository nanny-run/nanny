"""crew.py — assembles and runs the 4-agent CrewAI incident analysis pipeline.

Architecture: idiomatic CrewAI single-tool tasks.
    Each task has exactly one tool and one instruction. The LLM has one job
    per task — it cannot hallucinate past a description that says "call this
    one tool with these exact arguments." CrewAI dispatches the call;
    @nanny_tool fires on every dispatch — enforcement is guaranteed.

Task chain (sequential):
    Phase 1 — Ingestion:
        validate_schema_task         validate_schema
        load_metrics_task            load_metrics

    Phase 2 — Analysis:
        compute_stats_task(cpu)      compute_stats
        compute_stats_task(error)    compute_stats
        detect_anomalies_task(cpu)   detect_anomalies
        detect_anomalies_task(error) detect_anomalies
        correlate_signals_task       correlate_signals
        synthesis_task               (no tool — pure LLM reasoning)

    Phase 3 — Visualisation:
        generate_chart_task(cpu)     generate_chart
        generate_chart_task(error)   generate_chart
        generate_chart_task(latency) generate_chart

    Phase 4 — Report:
        write_report_task            write_report

Governance:
    @rule("no_analysis_loop")   — fires if compute_stats is called 5× in a row
                                  (safeguard against runaway analysis loops)
    @agent("analysis")    — activates [limits.analysis] scope for the run
"""

from __future__ import annotations

from collections import deque
from dataclasses import dataclass, field
from typing import Any

from crewai import Crew, Process

from nanny_sdk import agent, rule
from nanny_sdk.exceptions import NannyStop  # noqa: F401 — re-exported for callers

from metrics_crew.agents import (
    analysis_agent,
    compute_stats_task,
    correlate_signals_task,
    detect_anomalies_task,
    generate_chart_task,
    ingestion_agent,
    load_metrics_task,
    reporter_agent,
    synthesis_task,
    validate_schema_task,
    viz_agent,
    write_report_task,
)
from metrics_crew.config import DEFAULT_OUTPUT_DIR

# ── Rule ──────────────────────────────────────────────────────────────────────

_call_window: deque[str] = deque(maxlen=5)


@rule("no_analysis_loop")
def check_no_analysis_loop(ctx: Any) -> bool:
    """Deny if the last 5 tool calls are all compute_stats (loop guard).

    With single-tool tasks this should never fire in normal operation —
    compute_stats is called exactly twice per run (cpu + error_rate).
    The rule guards against future regressions where analysis tools might
    be re-introduced into a multi-tool task.
    """
    tool_name = getattr(ctx, "requested_tool", "") or ""
    _call_window.append(tool_name)
    if len(_call_window) == 5 and all(t == "compute_stats" for t in _call_window):
        return False
    return True


# ── Pipeline result ───────────────────────────────────────────────────────────


@dataclass
class PipelineResult:
    report_path: str
    chart_paths: list[str] = field(default_factory=list)
    agents_run: int = 4
    raw_output: str = ""


# ── Pipeline entry point ──────────────────────────────────────────────────────


@agent("analysis")
def run_pipeline(
    data_path: str,
    output_dir: str = DEFAULT_OUTPUT_DIR,
) -> PipelineResult:
    """Run the full 4-agent CrewAI incident analysis pipeline.

    Single-tool tasks guarantee enforcement: the LLM calls exactly one tool
    per task. Nanny intercepts every call via @nanny_tool — no model can
    hallucinate past a task with a single tool and a single instruction.

    Raises a NannyStop subclass if execution is stopped by governance.
    In passthrough mode (no bridge) all decorators are no-ops.
    """

    # ── Phase 1: Ingestion ────────────────────────────────────────────────────
    t_validate = validate_schema_task(data_path)
    t_load = load_metrics_task(data_path, context=[t_validate])

    # ── Phase 2: Analysis ─────────────────────────────────────────────────────
    t_stats_cpu = compute_stats_task("cpu",        data_path, context=[t_load])
    t_stats_error = compute_stats_task(
        "error_rate", data_path, context=[t_load])
    t_anomalies_cpu = detect_anomalies_task(
        "cpu",        data_path, context=[t_load])
    t_anomalies_error = detect_anomalies_task(
        "error_rate", data_path, context=[t_load])
    t_correlate = correlate_signals_task(data_path, context=[t_load])
    t_synthesis = synthesis_task(
        context=[
            t_stats_cpu, t_stats_error,
            t_anomalies_cpu, t_anomalies_error,
            t_correlate,
        ],
    )

    # ── Phase 3: Visualisation ────────────────────────────────────────────────
    t_chart_cpu = generate_chart_task(
        "cpu",        data_path, output_dir, context=[t_synthesis])
    t_chart_error = generate_chart_task(
        "error_rate", data_path, output_dir, context=[t_synthesis])
    t_chart_latency = generate_chart_task(
        "latency",    data_path, output_dir, context=[t_synthesis])

    # ── Phase 4: Report ───────────────────────────────────────────────────────
    t_report = write_report_task(
        output_dir=output_dir,
        context=[t_synthesis, t_chart_cpu, t_chart_error, t_chart_latency],
    )

    crew = Crew(
        agents=[ingestion_agent, analysis_agent, viz_agent, reporter_agent],
        tasks=[
            t_validate, t_load,
            t_stats_cpu, t_stats_error,
            t_anomalies_cpu, t_anomalies_error,
            t_correlate, t_synthesis,
            t_chart_cpu, t_chart_error, t_chart_latency,
            t_report,
        ],
        process=Process.sequential,
        verbose=True,
    )

    result = crew.kickoff()

    # The last task is write_report_task — its output is the saved file path.
    report_path_str = (
        result.tasks_output[-1].raw.strip()
        if result.tasks_output
        else ""
    )

    from pathlib import Path
    chart_paths = sorted(str(p) for p in Path(output_dir).glob("chart_*.html"))

    return PipelineResult(
        report_path=report_path_str,
        chart_paths=chart_paths,
        agents_run=4,
        raw_output=str(result.raw),
    )
