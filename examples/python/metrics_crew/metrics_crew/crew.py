"""crew.py — assembles and runs the 4-agent CrewAI incident analysis pipeline.

Governance:
    @rule("no_analysis_loop")   — fires after 5 consecutive compute_stats calls
    @nanny_agent("analysis")    — activates [limits.analysis] scope for the run

Framework propagation note:
    NannyStop extends BaseException (not Exception), so it propagates through
    CrewAI's broad ``except Exception`` handlers naturally — no patching needed.
    The monkey-patching block below is preserved as a reference for the history
    of why this works, and for any framework that catches BaseException explicitly.
"""

from __future__ import annotations

from collections import deque
from dataclasses import dataclass, field
from pathlib import Path
from typing import Any

from crewai import Crew, Process

from nanny_sdk import agent as nanny_agent, rule
from nanny_sdk.exceptions import NannyStop

from metrics_crew.agents import (
    analysis_agent,
    analysis_task,
    ingestion_agent,
    ingestion_task,
    reporter_agent,
    reporter_task,
    viz_agent,
    viz_task,
)
from metrics_crew.config import DEFAULT_OUTPUT_DIR

# ── NannyStop propagation patches (reference only — no longer needed) ──────────
# NannyStop(BaseException) propagates through CrewAI's except Exception handlers
# without any patching. Kept here to document the prior approach and as a
# template if a future framework is found to catch BaseException explicitly.
#
# from crewai.tools.base_tool import BaseTool, Tool
# from crewai.tools.structured_tool import CrewStructuredTool
#
# class _NannySignal(BaseException):
#     """BaseException carrier — bypasses crewai's ``except Exception`` handlers."""
#     def __init__(self, stop: NannyStop) -> None:
#         super().__init__(str(stop))
#         self.stop = stop
#
# # Patch 1: BaseTool.run — used by experimental/agent_executor.py (crewai 1.14+)
# _orig_base_tool_run = BaseTool.run
# def _nanny_aware_run(self: BaseTool, *args: Any, **kwargs: Any) -> Any:
#     try:
#         return _orig_base_tool_run(self, *args, **kwargs)
#     except NannyStop as exc:
#         raise _NannySignal(exc) from None
# BaseTool.run = _nanny_aware_run  # type: ignore[method-assign]
# Tool.run = _nanny_aware_run      # type: ignore[method-assign]
#
# # Patch 2: CrewStructuredTool.invoke — used by the legacy tool_usage.py path
# _orig_crew_invoke = CrewStructuredTool.invoke
# def _nanny_aware_invoke(self, input, config=None, **kwargs):
#     try:
#         return _orig_crew_invoke(self, input, config=config, **kwargs)
#     except NannyStop as exc:
#         raise _NannySignal(exc) from None
# CrewStructuredTool.invoke = _nanny_aware_invoke  # type: ignore[method-assign]
# ──────────────────────────────────────────────────────────────────────────────

# ── Rule ──────────────────────────────────────────────────────────────────────

_call_window: deque[str] = deque(maxlen=5)


@rule("no_analysis_loop")
def check_no_analysis_loop(ctx: Any) -> bool:
    """Deny if the last 5 tool calls are all compute_stats (loop guard)."""
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


@nanny_agent("analysis")
def run_pipeline(
    data_path: str,
    output_dir: str = DEFAULT_OUTPUT_DIR,
) -> PipelineResult:
    """Run the full 4-agent CrewAI incident analysis pipeline.

    Builds tasks with context chaining, kicks off a sequential CrewAI Crew,
    and returns a ``PipelineResult`` with the report path and chart paths.

    Raises a ``NannyStop`` subclass if execution is stopped by governance.
    In passthrough mode (no bridge) the decorator is a no-op and runs directly.
    """
    t_ingest   = ingestion_task(data_path)
    t_analysis = analysis_task(data_path,   context=[t_ingest])
    t_viz      = viz_task(data_path, output_dir=output_dir, context=[t_analysis])
    t_report   = reporter_task(output_dir=output_dir, context=[t_ingest, t_analysis, t_viz])

    crew = Crew(
        agents=[ingestion_agent, analysis_agent, viz_agent, reporter_agent],
        tasks=[t_ingest, t_analysis, t_viz, t_report],
        process=Process.sequential,
        verbose=True,
    )

    # NannyStop(BaseException) propagates through crew.kickoff() naturally.
    result = crew.kickoff()

    chart_paths = sorted(str(p) for p in Path(output_dir).glob("chart_*.html"))
    # Glob for the newest report file — CrewAI 1.14+ stores the tool-call JSON
    # in tasks_output[-1].raw rather than the tool's return value (the file path).
    report_files = sorted(Path(output_dir).glob("incident_*.md"), key=lambda p: p.stat().st_mtime)
    report_path = str(report_files[-1]) if report_files else ""

    return PipelineResult(
        report_path=report_path,
        chart_paths=chart_paths,
        agents_run=4,
        raw_output=str(result.raw),
    )
