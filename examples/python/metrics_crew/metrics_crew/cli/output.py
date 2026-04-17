"""Rich terminal rendering for metrics_crew.

Three concerns:
  spinner  — progress while the pipeline is running
  summary  — Rich table when the pipeline completes successfully
  stop     — user-facing message when the pipeline is cut short

All messages speak in metrics-crew's voice.  Stop reasons (budget, timeout,
rule) are operator concerns visible in the NDJSON event log, not user-facing.
"""

from __future__ import annotations

from collections.abc import Generator
from contextlib import contextmanager
from pathlib import Path

from rich.console import Console
from rich.panel import Panel
from rich.table import Table
from rich.text import Text

# All UI goes to stderr so stdout can be piped cleanly.
console = Console(stderr=True, highlight=False)


# ── Spinner ────────────────────────────────────────────────────────────────────


@contextmanager
def thinking(message: str = "Running incident analysis pipeline…") -> Generator[None, None, None]:
    """Show a spinner while the pipeline block executes."""
    with console.status(Text(message, style="bold cyan"), spinner="dots"):
        yield


# ── Summary ────────────────────────────────────────────────────────────────────


def render_summary(result: object) -> None:
    """Render a Rich table summarising the completed pipeline run."""
    from metrics_crew.crew import PipelineResult  # avoid circular at module level

    assert isinstance(result, PipelineResult)

    table = Table(show_header=False, box=None, padding=(0, 2))
    table.add_column("key",   style="dim",   no_wrap=True)
    table.add_column("value", style="white", no_wrap=False)

    table.add_row("Agents run",       str(result.agents_run))
    table.add_row("Charts produced",  str(len(result.chart_paths)))

    for path in result.chart_paths:
        table.add_row("", f"[dim]{path}[/dim]")

    report_display = result.report_path or "—"
    try:
        rp = Path(result.report_path) if result.report_path else None
        if rp and rp.exists():
            report_display = f"[link=file://{rp.resolve()}]{result.report_path}[/link]"
    except (OSError, ValueError):
        pass  # report_path may be a long JSON string from CrewAI internals
    table.add_row("Report", report_display)

    console.print()
    console.print(
        Panel(
            table,
            title="[bold green]metrics-crew — analysis complete[/bold green]",
            border_style="green",
            padding=(1, 2),
        )
    )


# ── Pipeline stopped ───────────────────────────────────────────────────────────


def render_stop(exc: Exception) -> None:
    """Render a user-facing message when the pipeline stops early."""
    kind = type(exc).__name__
    headline, hint = _stop_copy(exc, kind)

    body = Text()
    body.append(headline, style="bold")
    if hint:
        body.append(f"\n{hint}", style="dim")

    console.print()
    console.print(
        Panel(
            body,
            title="[bold yellow]metrics-crew — analysis stopped[/bold yellow]",
            border_style="yellow",
            padding=(0, 2),
        )
    )


def _stop_copy(exc: Exception, kind: str) -> tuple[str, str]:
    """Return (headline, hint) for each stop condition."""
    if kind == "BudgetExhausted":
        return (
            "Analysis stopped — the pipeline exhausted its compute budget.",
            "Try a narrower analysis window, or raise the cost limit in nanny.toml.",
        )
    if kind == "MaxStepsReached":
        return (
            "Analysis stopped — the pipeline exceeded its step limit.",
            "The model may be looping. Try reducing the number of signals or raise the step cap.",
        )
    if kind == "RuleDenied":
        rule_name = getattr(exc, "rule_name", "") or "unknown"
        if rule_name == "no_analysis_loop":
            return (
                "Analysis stopped — the pipeline was computing the same metric repeatedly.",
                "Try providing a more targeted dataset or narrowing the analysis scope.",
            )
        return (
            "Analysis stopped — a pipeline policy rule was triggered.",
            f'Rule: "{rule_name}"',
        )
    if kind == "ToolDenied":
        tool_name = getattr(exc, "tool_name", "") or "unknown"
        return (
            f'Analysis stopped — "{tool_name}" is not permitted in this pipeline.',
            "Check the tool configuration in nanny.toml.",
        )
    if kind == "TimeoutExpired":
        return (
            "Analysis stopped — the pipeline exceeded its time limit.",
            "Ollama may be under load, or the dataset is too large. Try again or raise the timeout.",
        )
    return (f"Analysis stopped unexpectedly: {exc}", "")
