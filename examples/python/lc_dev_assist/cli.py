"""dev_assist CLI.

Usage
-----
  python cli.py --trace <file>    Diagnose a stack trace.
  python cli.py                   Read a stack trace from stdin.
  nanny run                       Same, with execution limits enforced.

Note: Typer promotes a single command to the root.  When more commands are
added (``dev commit``, ``dev memory``, etc.) the subcommand structure emerges
automatically.
"""

from __future__ import annotations

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

from agent import run_debug
from output import console, render_diagnosis, render_stop, thinking

app = typer.Typer(
    name="dev",
    help="dev_assist — diagnose stack traces and debug errors with a local LLM.",
    add_completion=False,
)


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
) -> None:
    """Diagnose a stack trace and propose a fix.

    Reads from --trace when given, otherwise reads from stdin.

    \b
    Examples:
      dev --trace ./error.log
      cat error.log | dev
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
        with thinking("Analysing trace…"):
            output = run_debug(trace_text)

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
