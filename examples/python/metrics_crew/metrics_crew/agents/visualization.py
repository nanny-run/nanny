"""Visualization agent — generates one chart per metric signal.

Three single-tool tasks, one per metric:
    generate_chart_task("cpu", ...)
    generate_chart_task("error_rate", ...)
    generate_chart_task("latency", ...)

Each task has exactly one tool (generate_chart) and one instruction.
The LLM calls generate_chart once and returns the file path.
CrewAI dispatches the call; @nanny_tool fires on every dispatch — guaranteed.

Exports:
    viz_agent             — CrewAI Agent with generate_chart tool.
    generate_chart_task() — factory for one chart generation task.
"""

from crewai import Agent, Task

from metrics_crew.config import DEFAULT_OUTPUT_DIR, make_llm
from metrics_crew.tools import generate_chart

_llm = make_llm()

viz_agent = Agent(
    role="Visualization Engineer",
    goal=(
        "Generate one time-series HTML chart per affected signal "
        "and return the file path of each chart."
    ),
    backstory=(
        "You are a data visualisation engineer embedded in an SRE team. "
        "You call generate_chart exactly once per task to produce one chart. "
        "You do not batch multiple charts in a single task."
    ),
    llm=_llm,
    tools=[generate_chart],
    verbose=True,
    allow_delegation=False,
)


def generate_chart_task(
    metric: str,
    data_path: str,
    output_dir: str = DEFAULT_OUTPUT_DIR,
    context: list | None = None,
) -> Task:
    """Return a Task that calls generate_chart for one metric."""
    return Task(
        description=(
            f"Call generate_chart with metric='{metric}', path='{data_path}', "
            f"and output_dir='{output_dir}'. "
            "Return the file path of the generated HTML chart."
        ),
        expected_output=(
            f"The file path of the generated chart for '{metric}', "
            f"e.g. {output_dir}/chart_{metric}.html"
        ),
        agent=viz_agent,
        tools=[generate_chart],
        context=context or [],
    )
