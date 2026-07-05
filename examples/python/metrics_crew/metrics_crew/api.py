"""api.py — FastAPI wrapper for the metrics_crew incident analysis pipeline.

Exposes three endpoints:
    GET  /health          → liveness check for Fly.io / k8s / Railway
    POST /analyze         → upload a metrics CSV, returns a job_id
    GET  /jobs/{job_id}   → poll for status and results

Architecture:
    - One analysis run at a time (asyncio.Lock serialises concurrent requests)
    - CrewAI's synchronous kickoff() runs in a ThreadPoolExecutor so it does
      not block the uvicorn event loop
    - Job state is in-memory — lost on container restart, acceptable for MVP
    - Report content is returned in the job response so callers don't need
      filesystem access (Fly.io containers have an ephemeral filesystem)

Usage (local):
    uvicorn metrics_crew.api:app --reload --port 8080

Usage (under Nanny — set in nanny.toml [start] cmd):
    uvicorn metrics_crew.api:app --host 0.0.0.0 --port 8080
"""

from __future__ import annotations

import asyncio
import os
import tempfile
import uuid
from concurrent.futures import ThreadPoolExecutor
from pathlib import Path
from typing import Any, Optional

from dotenv import load_dotenv
from fastapi import BackgroundTasks, FastAPI, HTTPException, Query, UploadFile

# Load .env if present — no-op when vars are already set (production / CI).
load_dotenv()

app = FastAPI(title="metrics-crew", version="0.1.0")

# One worker: serialise analysis runs so per-role Nanny limits apply cleanly
# to one crew execution at a time.
_lock = asyncio.Lock()
_executor = ThreadPoolExecutor(max_workers=1)
_jobs: dict[str, dict[str, Any]] = {}


# ── Endpoints ─────────────────────────────────────────────────────────────────


@app.get("/health")
def health() -> dict[str, str]:
    """Liveness probe — required by Fly.io health checks."""
    return {"status": "ok"}


@app.post("/analyze")
async def analyze(
    bg: BackgroundTasks,
    file: UploadFile,
    scenario: Optional[str] = Query(
        None,
        description=(
            "Demo governance scenario. Omit for a normal full run. "
            "Options: 'demo-budget' (triggers BudgetExhausted), "
            "'demo-steps' (triggers MaxStepsReached)."
        ),
    ),
) -> dict[str, str]:
    """Accept a metrics CSV upload and queue an analysis run.

    Returns a job_id. Poll GET /jobs/{job_id} for status and results.
    The CSV must contain columns: timestamp, cpu, memory, request_rate,
    error_rate, latency.

    Pass ?scenario=demo-budget or ?scenario=demo-steps to trigger a Nanny
    governance stop deliberately — useful for testing how your application
    handles stopped runs without waiting for a real limit to be hit.
    """
    valid_scenarios = {None, "demo-budget", "demo-steps"}
    if scenario not in valid_scenarios:
        raise HTTPException(
            status_code=400,
            detail=f"Unknown scenario {scenario!r}. Valid values: demo-budget, demo-steps",
        )

    if not file.filename or not file.filename.lower().endswith(".csv"):
        raise HTTPException(status_code=400, detail="Expected a .csv file")

    contents = await file.read()
    if not contents:
        raise HTTPException(status_code=400, detail="Uploaded file is empty")

    # Write to a temp file — run_pipeline expects a filesystem path.
    tmp = tempfile.NamedTemporaryFile(delete=False, suffix=".csv")
    tmp.write(contents)
    tmp.close()

    job_id = str(uuid.uuid4())
    _jobs[job_id] = {"status": "queued", "scenario": scenario or "normal"}
    bg.add_task(_run, job_id, tmp.name, scenario)
    return {"job_id": job_id}


@app.get("/jobs/{job_id}")
def get_job(job_id: str) -> dict[str, Any]:
    """Return the current status and results of an analysis job.

    status values:
        "queued"   — waiting for the current run to finish
        "running"  — crew is executing
        "done"     — completed; results are in the response
        "stopped"  — Nanny governance stop (BudgetExhausted, RuleDenied, etc.)
        "failed"   — unexpected error
    """
    job = _jobs.get(job_id)
    if job is None:
        raise HTTPException(status_code=404, detail=f"Job {job_id!r} not found")
    return job


# ── Background execution ──────────────────────────────────────────────────────


async def _run(job_id: str, data_path: str, scenario: Optional[str] = None) -> None:
    """Acquire the lock, then run the pipeline in the thread pool."""
    async with _lock:
        _jobs[job_id]["status"] = "running"
        loop = asyncio.get_event_loop()
        await loop.run_in_executor(_executor, _run_sync, job_id, data_path, scenario)


def _run_sync(job_id: str, data_path: str, scenario: Optional[str] = None) -> None:
    """Synchronous pipeline execution — runs inside the ThreadPoolExecutor.

    Imports are deferred so the module loads quickly at server startup;
    CrewAI and model clients are only initialised when a job actually runs.

    When `scenario` is set, we skip the full CrewAI crew and call tools
    directly under a tight named-limit scope so Nanny fires a governed stop.
    This demonstrates that:
      1. The server stays up after a stop — only the job is stopped, not the process.
      2. The job endpoint returns {"status": "stopped", "reason": "...", "detail": "..."}
      3. The Nanny NDJSON event log captures the stop event in real time.

    Tools are called as plain Python functions — @tool fires on every
    call regardless of caller (CrewAI, LangGraph, or direct Python).
    """
    output_dir = f"reports/{job_id}"
    os.makedirs(output_dir, exist_ok=True)

    try:
        from nanny_sdk.exceptions import NannyStop

        if scenario in ("demo-budget", "demo-steps"):
            _run_demo_scenario(job_id, data_path, scenario)
            return

        from metrics_crew.crew import run_pipeline

        result = run_pipeline(data_path=data_path, output_dir=output_dir)

        # Read the report file so the content travels in the API response —
        # callers should not rely on the filesystem path being accessible.
        report_content = ""
        if result.report_path and Path(result.report_path).exists():
            report_content = Path(result.report_path).read_text(encoding="utf-8")

        _jobs[job_id] = {
            "status": "done",
            "report_path": result.report_path,
            "report_content": report_content,
            "chart_paths": result.chart_paths,
            "agents_run": result.agents_run,
            # Truncate raw LLM output — can be very large.
            "summary": (result.raw_output or "")[:3000],
        }

    except NannyStop as exc:
        _jobs[job_id] = {
            "status": "stopped",
            "reason": type(exc).__name__,
            "detail": str(exc),
        }
    except Exception as exc:  # noqa: BLE001
        _jobs[job_id] = {
            "status": "failed",
            "error": type(exc).__name__,
            "detail": str(exc),
        }
    finally:
        # Remove the temp CSV — it has been read by the pipeline.
        try:
            os.unlink(data_path)
        except OSError:
            pass


def _run_demo_scenario(job_id: str, data_path: str, scenario: str) -> None:
    """Run tools directly under a tight demo scope to trigger a Nanny stop.

    This bypasses the full CrewAI crew so the demo-budget / demo-steps named
    limit set in nanny.toml is the ONLY active scope — no @agent("analysis")
    scope inside to override it.

    Sequence for demo-budget (token ceiling = 100):
        validate_schema  tokens=20  → total=20   (allowed)
        load_metrics     tokens=60  → total=80   (allowed)
        compute_stats    tokens=200 → total=280 > 100 → BudgetExhausted ✓

    Sequence for demo-steps (step ceiling = 4):
        validate_schema  step=1  (allowed)
        load_metrics     step=2  (allowed)
        compute_stats    step=3  (allowed)
        compute_stats    step=4  (allowed — this is the last permitted step)
        detect_anomalies step=5  → MaxStepsReached ✓
    """
    from nanny_sdk import agent as nanny_agent
    from nanny_sdk.exceptions import NannyStop

    # Import tools directly — @tool fires on every call.
    from metrics_crew.tools import compute_stats, detect_anomalies, load_metrics, validate_schema

    @nanny_agent(scenario)
    def _scoped() -> None:
        # .func accesses the nanny-decorated function beneath the @crew_tool
        # wrapper. @crew_tool returns a Tool object (not a plain callable);
        # .func is the callable that @tool wraps — enforcement fires.
        validate_schema.func(path=data_path)
        load_metrics.func(path=data_path)
        compute_stats.func(metric="cpu", path=data_path)
        compute_stats.func(metric="error_rate", path=data_path)
        # demo-budget stops before this line; demo-steps reaches it
        detect_anomalies.func(metric="cpu", path=data_path)

    # NannyStop propagates out of _scoped() and is caught by the caller's
    # except NannyStop block — no special handling needed here.
    _scoped()

    # If we reach this line the demo scenario did not fire — unexpected.
    _jobs[job_id] = {
        "status": "failed",
        "error": "ScenarioDidNotFire",
        "detail": f"Scenario {scenario!r} completed without a Nanny stop. "
                  "Check that the named limits are set in nanny.toml.",
    }
