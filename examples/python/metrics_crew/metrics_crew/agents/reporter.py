"""Reporter agent — drafts the incident report and saves it to disk.

One single-tool task:
    write_report_task — the LLM composes the full Markdown content from
    context, then calls write_report(content=..., output_dir=...) to save it.

Single tool, single responsibility. The LLM has two sequential jobs in one
task: reason (compose Markdown) → act (call write_report). This is correct
CrewAI pattern for a task that requires synthesis before a tool call.
@nanny_tool fires on the write_report call — guaranteed enforcement.

Exports:
    reporter_agent     — CrewAI Agent with write_report tool.
    write_report_task()— factory for the report drafting and saving task.
"""

from crewai import Agent, LLM, Task

from metrics_crew.config import DEFAULT_OUTPUT_DIR, MODEL
from metrics_crew.tools import write_report

_llm = LLM(model=MODEL)

reporter_agent = Agent(
    role="Incident Reporter",
    goal=(
        "Write a complete, well-structured Markdown incident report from all "
        "findings and save it to disk using write_report."
    ),
    backstory=(
        "You are a technical writer embedded in an SRE team. "
        "You receive synthesised analysis, chart paths, and ingestion summaries "
        "via task context. Your job is to compose the full Markdown incident "
        "report and save it — call write_report exactly once with the complete "
        "report content."
    ),
    llm=_llm,
    tools=[write_report],
    verbose=True,
    allow_delegation=False,
)


def write_report_task(
    output_dir: str = DEFAULT_OUTPUT_DIR,
    context: list | None = None,
) -> Task:
    """Return a Task that drafts and saves the incident report via write_report."""
    return Task(
        description=(
            "Using all findings from the context tasks (ingestion summary, "
            "incident analysis, and chart file paths), compose a complete "
            "Markdown incident report with the following sections:\n"
            "1. Executive summary\n"
            "2. Incident timeline (start, peak, end)\n"
            "3. Affected signals with peak values\n"
            "4. Root cause analysis\n"
            "5. Chart references (include the HTML file paths from the "
            "visualisation tasks)\n"
            "6. Recommended follow-up actions\n\n"
            f"Then call write_report with content=<your_markdown> and "
            f"output_dir='{output_dir}'. "
            "Return only the file path of the saved report."
        ),
        expected_output=(
            "The file path of the saved Markdown incident report, "
            f"e.g. {output_dir}/incident_<timestamp>.md"
        ),
        agent=reporter_agent,
        tools=[write_report],
        context=context or [],
    )
