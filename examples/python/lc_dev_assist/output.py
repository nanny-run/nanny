"""Rich terminal rendering for dev_assist.

Three concerns:
  spinner    — show progress while the agent is thinking / calling tools
  diagnosis  — render the agent's final output (root cause + fix)
  stop       — render a clean, user-facing message when analysis is cut short

All messages speak in dev_assist's voice.  The underlying reason an analysis
stopped (budget, timeout, rule) is an operator concern visible in the NDJSON
event log — not something the developer using ``dev`` needs to know about.
"""

from __future__ import annotations

from collections.abc import Generator
from contextlib import contextmanager

from rich.console import Console
from rich.markdown import Markdown
from rich.panel import Panel
from rich.text import Text

# Single console for the whole app.  stderr=True keeps the diagnosis on stderr
# so callers can pipe stdout cleanly if needed.
console = Console(stderr=True, highlight=False)


# ---------------------------------------------------------------------------
# Spinner
# ---------------------------------------------------------------------------


@contextmanager
def thinking(message: str = "Analysing trace…") -> Generator[None, None, None]:
    """Context manager: show a spinner while the block executes.

    Usage::

        with thinking("Reading files…"):
            result = run_debug(trace_text)
    """
    with console.status(
        Text(message, style="bold cyan"),
        spinner="dots",
    ):
        yield


# ---------------------------------------------------------------------------
# Diagnosis output
# ---------------------------------------------------------------------------


def render_diagnosis(text: str) -> None:
    """Render the agent's diagnosis in a styled panel.

    Treats the content as Markdown so bullet lists, bold text,
    and code fences render properly in the terminal.
    """
    console.print()
    console.print(
        Panel(
            Markdown(text),
            title="[bold green]dev — diagnosis[/bold green]",
            border_style="green",
            padding=(1, 2),
        )
    )


# ---------------------------------------------------------------------------
# Analysis stopped
# ---------------------------------------------------------------------------


def render_stop(exc: Exception) -> None:
    """Render a user-facing message when analysis stops early.

    Maps internal stop conditions to plain language the developer can act on.
    The exception type determines the message — the word "Nanny" never appears.
    """
    kind = type(exc).__name__
    message, hint = _stop_copy(exc, kind)

    body = Text()
    body.append(message, style="bold")
    if hint:
        body.append(f"\n{hint}", style="dim")

    console.print()
    console.print(
        Panel(
            body,
            title="[bold yellow]dev — analysis stopped[/bold yellow]",
            border_style="yellow",
            padding=(0, 2),
        )
    )


def _stop_copy(exc: Exception, kind: str) -> tuple[str, str]:
    """Return (headline, hint) for each stop condition."""
    if kind == "ToolDenied":
        tool = getattr(exc, "tool_name", "") or "unknown"
        return (
            f'The agent tried to call "{tool}", which is not allowed.',
            "This usually means the model went off-script. Re-run or narrow the query.",
        )
    if kind == "RuleDenied":
        rule = getattr(exc, "rule_name", "") or "unknown"
        if rule == "no_read_loop":
            return (
                "The agent got stuck reading the same files repeatedly.",
                "Try a more specific trace or a narrower file path.",
            )
        return (
            "The agent was stopped by a policy rule.",
            f'Rule: "{rule}"',
        )
    if kind == "BudgetExhausted":
        return (
            "Analysis stopped — the agent reached its cost limit.",
            "Try narrowing the query, or raise the cost limit in nanny.toml.",
        )
    if kind == "MaxStepsReached":
        return (
            "Analysis stopped — the agent hit its step cap.",
            "The model may be looping. Try a more focused trace.",
        )
    if kind == "TimeoutExpired":
        return (
            "Analysis timed out.",
            "Ollama may be slow or the trace is too large. Try again or raise the timeout.",
        )
    return (f"Analysis stopped unexpectedly: {exc}", "")
