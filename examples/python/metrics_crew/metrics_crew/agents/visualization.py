"""Visualization agent — generates Plotly HTML charts for affected signals.

Exports:
    viz_agent  — CrewAI Agent with the generate_chart tool.
    viz_task() — factory that returns the corresponding Task.
"""

from crewai import Agent, LLM, Task

from metrics_crew.config import DEFAULT_OUTPUT_DIR, MODEL, OLLAMA_BASE_URL
from metrics_crew.tools import generate_chart

_llm = LLM(model=f"ollama/{MODEL}", base_url=OLLAMA_BASE_URL)

viz_agent = Agent(
    role="Visualization Engineer",
    goal=(
        "Generate a clear time-series chart for every affected signal. "
        "You always invoke generate_chart directly — you never describe it."
    ),
    backstory=(
        "You are a data visualisation engineer. You call generate_chart for each "
        "affected signal and return the list of file paths produced."
    ),
    llm=_llm,
    tools=[generate_chart],
    verbose=True,
    allow_delegation=False,
)


def viz_task(
    data_path: str,
    output_dir: str = DEFAULT_OUTPUT_DIR,
    context: list | None = None,
) -> Task:
    """Return the visualisation Task."""
    return Task(
        description=(
            f"Call generate_chart three times, once per signal:\n"
            f"1. generate_chart(metric='cpu', path='{data_path}', output_dir='{output_dir}')\n"
            f"2. generate_chart(metric='error_rate', path='{data_path}', output_dir='{output_dir}')\n"
            f"3. generate_chart(metric='latency', path='{data_path}', output_dir='{output_dir}')\n"
            f"Return the list of file paths that were saved."
        ),
        expected_output=(
            "A list of three file paths to the generated HTML chart files."
        ),
        agent=viz_agent,
        context=context or [],
    )
