"""dev_assist CLI.

Usage
-----
  dev debug --trace <file>             Diagnose a stack trace (ReAct mode).
  dev debug --trace <file> --mode plan Diagnose using Plan-and-Execute mode.
  dev debug                            Read a stack trace from stdin.
  nanny run                            Same, with execution limits enforced.

More commands coming: dev ask, dev commit, dev pr, dev remember, dev plan.
"""

from __future__ import annotations

import enum
import sys
from pathlib import Path
from typing import Annotated

import typer
from nanny_sdk import (
    BudgetExhausted,
    MaxStepsReached,
    NannyStop,
    RuleDenied,
    TimeoutExpired,
    ToolDenied,
)

from dev_assist.agents.debug import run_debug
from dev_assist.cli.output import console, render_diagnosis, render_stop, thinking

app = typer.Typer(
    name="dev",
    help="dev_assist — your local AI engineering assistant.",
    add_completion=False,
    no_args_is_help=True,
)


@app.callback()
def main() -> None:
    """dev_assist — diagnose errors, explore code, and automate dev tasks."""


class Mode(str, enum.Enum):
    react = "react"
    plan = "plan"


@app.command()
def debug(
    trace: Annotated[
        Path | None,
        typer.Option(
            "--trace",
            "-t",
            help="Path to a stack trace or error log file.",
            exists=True,
            readable=True,
            resolve_path=True,
        ),
    ] = None,
    mode: Annotated[
        Mode,
        typer.Option(
            "--mode",
            "-m",
            help=(
                "Execution mode. "
                "'react' (default): iterative Thought/Action loop. "
                "'plan': single planning call then deterministic execution."
            ),
        ),
    ] = Mode.react,
) -> None:
    """Diagnose a stack trace and propose a fix.

    Reads from --trace when given, otherwise reads from stdin.

    \b
    Examples:
      dev debug --trace ./error.log
      dev debug --trace ./error.log --mode plan
      cat error.log | dev debug
    """
    # --- read input ---
    if trace is not None:
        trace_text = trace.read_text(encoding="utf-8", errors="replace")
    else:
        if sys.stdin.isatty():
            console.print(
                "[yellow]Tip:[/yellow] pass a file with --trace, "
                "or pipe a stack trace via stdin."
            )
            raise typer.Exit(1)
        trace_text = sys.stdin.read()

    if not trace_text.strip():
        console.print("[red]Nothing to analyse — input is empty.[/red]")
        raise typer.Exit(1)

    # --- run agent ---
    try:
        label = "Planning and executing…" if mode == Mode.plan else "Analysing…"
        with thinking(label):
            output = run_debug(trace_text, mode=mode.value)

    except (ToolDenied, RuleDenied, BudgetExhausted, MaxStepsReached, TimeoutExpired) as exc:
        render_stop(exc)
        raise typer.Exit(2)

    except NannyStop as exc:
        render_stop(exc)
        raise typer.Exit(2)

    # --- render output ---
    render_diagnosis(output)


if __name__ == "__main__":
    app()
