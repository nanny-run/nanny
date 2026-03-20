// The deterministic execution loop.
//
// The Executor owns one job: run steps, check limits, stop when required.
// It does not know what agents are.
// It does not know what tools are.
// It does not know what LLMs are.
// It enforces hard limits around whatever the caller passes as a step function.

use crate::agent::{
    limits::Limits,
    state::{ExecutionState, StopReason},
};
use crate::events::event::{ExecutionEvent, ExecutionId};
use chrono::Utc;
use std::time::Instant;
use uuid::Uuid;

// ── StepOutcome ───────────────────────────────────────────────────────────────

/// What the agent reports back after a single step.
///
/// The executor does not look inside the step — it only sees this outcome.
/// This is how the executor stays agent-agnostic.
pub enum StepOutcome {
    /// The agent has more work to do. The loop will run another step.
    Continue,

    /// The agent has finished its task. The loop will stop cleanly.
    Done,
}

// ── ExecutionResult ───────────────────────────────────────────────────────────

/// The complete record of a finished execution.
///
/// Always produced — no execution ends without a result.
/// The stop reason is always explicit and typed.
/// The event log is complete and in order.
pub struct ExecutionResult {
    /// Unique ID for this execution run.
    pub execution_id: ExecutionId,

    /// Why execution stopped. Never absent. Never a free-text string.
    pub stop_reason: StopReason,

    /// How many steps completed before the stop.
    pub total_steps: u32,

    /// The full ordered sequence of events emitted during this execution.
    /// Append-only — nothing is ever removed or modified.
    pub events: Vec<ExecutionEvent>,
}

// ── Executor ──────────────────────────────────────────────────────────────────

/// The execution loop.
///
/// Create one per agent run. Call `run()`. Receive an `ExecutionResult`.
/// The executor is not reusable — one execution, one executor.
pub struct Executor {
    /// Stable identity for this run. Written into every event.
    execution_id: ExecutionId,

    /// The hard limits governing this execution.
    /// Set once at creation. Never mutated.
    limits: Limits,

    /// The current state of execution.
    /// Transitions: Initialized → Running → Stopped.
    state: ExecutionState,

    /// How many steps have completed so far.
    step_count: u32,

    /// The append-only event log for this execution.
    events: Vec<ExecutionEvent>,
}

impl Executor {
    /// Create a new executor.
    ///
    /// Generates a fresh execution ID.
    /// State starts as `Initialized` — nothing runs until `run()` is called.
    pub fn new(limits: Limits) -> Self {
        Self {
            execution_id: Uuid::new_v4(),
            limits,
            state: ExecutionState::Initialized,
            step_count: 0,
            events: Vec::new(),
        }
    }

    /// Run the execution loop.
    ///
    /// Calls `step_fn` with the current step number on every iteration.
    /// Checks all limits before each step — never after.
    /// Stops immediately when any limit is exceeded.
    /// Always returns an `ExecutionResult` with an explicit stop reason.
    ///
    /// The loop cannot be paused, retried, or soft-stopped.
    /// When it stops, it is stopped.
    pub fn run<F>(&mut self, mut step_fn: F) -> ExecutionResult
    where
        F: FnMut(u32) -> StepOutcome,
    {
        // Record the wall-clock start time.
        // `Instant` is monotonic — it only moves forward and cannot be
        // affected by system clock changes.
        let start = Instant::now();

        // Move from Initialized → Running.
        self.state = ExecutionState::Running;

        // First event: announce the execution has started with its full limits.
        self.emit(ExecutionEvent::ExecutionStarted {
            execution_id: self.execution_id,
            limits: self.limits.clone(),
            timestamp: Utc::now(),
        });

        // ── The loop ──────────────────────────────────────────────────────────
        //
        // `loop` in Rust is an infinite loop that only exits via `break`.
        // We break with a `StopReason` value — that value becomes `stop_reason`.
        // This means the loop can only end by stating why it ended.
        let stop_reason = loop {

            // ── Limit check 1: steps ──────────────────────────────────────────
            //
            // If the step counter has reached the max, stop before doing any work.
            // We check this BEFORE starting the step, not after.
            // This means max_steps = 3 allows steps 0, 1, 2 — then stops.
            if self.step_count >= self.limits.max_steps {
                break StopReason::MaxStepsReached;
            }

            // ── Limit check 2: timeout ────────────────────────────────────────
            //
            // `start.elapsed()` returns how long it has been since `start` was recorded.
            // `.as_millis()` converts that duration to milliseconds.
            // `as u64` casts it to match our `timeout_ms` type.
            // If we have been running too long, stop before doing any work.
            let elapsed_ms = start.elapsed().as_millis() as u64;
            if elapsed_ms >= self.limits.timeout_ms {
                break StopReason::TimeoutExpired;
            }

            // ── Step begins ───────────────────────────────────────────────────
            //
            // Capture the current step number before incrementing.
            // This is the step we are about to run.
            let current_step = self.step_count;

            self.emit(ExecutionEvent::StepStarted {
                execution_id: self.execution_id,
                step: current_step,
                timestamp: Utc::now(),
            });

            // ── Hand off to the caller ────────────────────────────────────────
            //
            // The executor does not know what happens inside `step_fn`.
            // It calls it, waits, and receives a `StepOutcome`.
            // The executor's job resumes after this line.
            let outcome = step_fn(current_step);

            // Increment the step counter now that the step has completed.
            self.step_count += 1;

            self.emit(ExecutionEvent::StepCompleted {
                execution_id: self.execution_id,
                step: current_step,
                timestamp: Utc::now(),
            });

            // ── Decide what happens next ──────────────────────────────────────
            //
            // `match` on the outcome. Two cases only.
            // Any new outcome must be added here explicitly — no catch-all.
            match outcome {
                StepOutcome::Continue => {
                    // Nothing to do. The loop will iterate again,
                    // hit the limit checks at the top, and either
                    // stop or run the next step.
                }
                StepOutcome::Done => {
                    // The agent says it has finished.
                    // This is a clean, successful stop.
                    break StopReason::AgentCompleted;
                }
            }
        };
        // ── End of loop ───────────────────────────────────────────────────────

        // Move to terminal state.
        self.state = ExecutionState::Stopped {
            reason: stop_reason.clone(),
        };

        // Final event: the execution is over. Reason is recorded.
        // This is always the last event in any complete execution log.
        self.emit(ExecutionEvent::ExecutionStopped {
            execution_id: self.execution_id,
            reason: stop_reason.clone(),
            total_steps: self.step_count,
            timestamp: Utc::now(),
        });

        ExecutionResult {
            execution_id: self.execution_id,
            stop_reason,
            total_steps: self.step_count,
            events: self.events.clone(),
        }
    }

    /// Append one event to the log.
    ///
    /// Private — only the executor itself emits events.
    /// Events are never modified after being appended.
    fn emit(&mut self, event: ExecutionEvent) {
        self.events.push(event);
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn test_limits() -> Limits {
        Limits {
            max_steps: 5,
            max_cost_units: 1000,
            timeout_ms: 5000,
        }
    }

    #[test]
    fn stops_at_max_steps() {
        let mut executor = Executor::new(test_limits());

        // Agent always wants to continue — limits must stop it.
        let result = executor.run(|_step| StepOutcome::Continue);

        assert_eq!(result.stop_reason, StopReason::MaxStepsReached);
        assert_eq!(result.total_steps, 5);
    }

    #[test]
    fn stops_when_agent_is_done() {
        let mut executor = Executor::new(test_limits());

        // Agent completes on step 2.
        let result = executor.run(|step| {
            if step == 2 {
                StepOutcome::Done
            } else {
                StepOutcome::Continue
            }
        });

        assert_eq!(result.stop_reason, StopReason::AgentCompleted);
        assert_eq!(result.total_steps, 3); // steps 0, 1, 2 completed
    }

    #[test]
    fn stops_on_timeout() {
        let tight_limits = Limits {
            max_steps: 10_000,
            max_cost_units: 1000,
            timeout_ms: 1, // 1ms — will expire immediately
        };

        let mut executor = Executor::new(tight_limits);

        // Sleep inside the step to guarantee the timeout fires.
        let result = executor.run(|_step| {
            std::thread::sleep(std::time::Duration::from_millis(10));
            StepOutcome::Continue
        });

        assert_eq!(result.stop_reason, StopReason::TimeoutExpired);
    }

    #[test]
    fn event_log_starts_with_execution_started() {
        let mut executor = Executor::new(test_limits());
        let result = executor.run(|_| StepOutcome::Done);

        assert!(
            matches!(result.events.first(), Some(ExecutionEvent::ExecutionStarted { .. })),
            "first event must always be ExecutionStarted"
        );
    }

    #[test]
    fn event_log_ends_with_execution_stopped() {
        let mut executor = Executor::new(test_limits());
        let result = executor.run(|_| StepOutcome::Continue);

        assert!(
            matches!(result.events.last(), Some(ExecutionEvent::ExecutionStopped { .. })),
            "last event must always be ExecutionStopped"
        );
    }
}
