"""dev_assist CLI.

Usage
-----
  dev debug --trace <file>   Diagnose a stack trace.
  dev debug                  Read a stack trace from stdin.
  nanny run                  Same, with execution limits enforced.

More commands coming: dev ask, dev commit, dev pr, dev remember, dev plan.
"""

from __future__ import annotations

import sys
from pathlib import Path
from typing import Annotated

import typer
from dotenv import load_dotenv
from nanny_sdk import (
    BudgetExhausted,
    MaxStepsReached,
    NannyStop,
    RuleDenied,
    TimeoutExpired,
    ToolDenied,
)

# Load .env if present — no-op when vars are already set (CI/CD, production).
# Developers: copy .env.example → .env and fill in GROQ_API_KEY.
load_dotenv()

from dev_assist.agents.debug import run_debug  # noqa: E402 — after load_dotenv
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
      dev debug --trace ./error.log
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
        with thinking():
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
