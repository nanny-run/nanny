"""metrics_crew CLI — Typer entry point.

Single subcommand: ``analyze``.  The ``@app.callback()`` prevents Typer from
promoting the lone command to root so future subcommands (``list``, ``watch``,
etc.) can be added without breaking the interface.
"""

from __future__ import annotations

import typer
from dotenv import load_dotenv

# Load .env if present — no-op when vars are already set (CI/CD, production).
# Developers: copy .env.example → .env and fill in OPENAI_API_KEY.
load_dotenv()

from nanny_sdk.exceptions import NannyStop

from metrics_crew.cli.output import render_stop, render_summary, thinking
from metrics_crew.config import DEFAULT_OUTPUT_DIR
from metrics_crew.crew import run_pipeline

app = typer.Typer(
    name="metrics-crew",
    add_completion=False,
    no_args_is_help=True,
)


@app.callback()
def main() -> None:
    """metrics-crew — multi-agent incident analysis pipeline."""


@app.command()
def analyze(
    data: str = typer.Option(
        "fixtures/sample_metrics.csv",
        "--data",
        help="Path to the metrics CSV file.",
        show_default=True,
    ),
    output: str = typer.Option(
        DEFAULT_OUTPUT_DIR,
        "--output",
        help="Directory for charts and the incident report.",
        show_default=True,
    ),
) -> None:
    """Analyze a metrics CSV and produce an incident report."""
    try:
        with thinking():
            result = run_pipeline(data, output)
        render_summary(result)
    except NannyStop as exc:
        render_stop(exc)
        raise typer.Exit(2)
