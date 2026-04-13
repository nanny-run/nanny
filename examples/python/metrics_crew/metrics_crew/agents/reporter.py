"""Reporter agent — writes the final incident Markdown report.

Exports:
    reporter_agent  — CrewAI Agent with the write_report tool.
    reporter_task() — factory that returns the corresponding Task.
"""

from crewai import Agent, LLM, Task

from metrics_crew.config import DEFAULT_OUTPUT_DIR, MODEL, OLLAMA_BASE_URL
from metrics_crew.tools import write_report

_llm = LLM(model=f"ollama/{MODEL}", base_url=OLLAMA_BASE_URL)

reporter_agent = Agent(
    role="Incident Reporter",
    goal=(
        "Synthesise all findings into a complete incident report saved as Markdown. "
        "You always call write_report directly — you never describe or simulate the call."
    ),
    backstory=(
        "You are a technical writer embedded in an SRE team. You call write_report "
        "with the full Markdown content and return the saved file path."
    ),
    llm=_llm,
    tools=[write_report],
    verbose=True,
    allow_delegation=False,
)


def reporter_task(
    output_dir: str = DEFAULT_OUTPUT_DIR,
    context: list | None = None,
) -> Task:
    """Return the reporter Task."""
    return Task(
        description=(
            "Write a complete Markdown incident report using the findings from previous tasks. "
            "Call write_report with:\n"
            f"  output_dir='{output_dir}'\n"
            "  content=<full Markdown report>\n\n"
            "The report must include:\n"
            "1. Executive summary\n"
            "2. Incident timeline (start, peak, end)\n"
            "3. Affected signals with peak values\n"
            "4. Root cause analysis\n"
            "5. Chart references\n"
            "6. Recommended follow-up actions\n\n"
            "Return the file path of the saved report."
        ),
        expected_output="The file path of the saved incident report Markdown file.",
        agent=reporter_agent,
        context=context or [],
    )
