// The deterministic execution loop.
//
// The executor owns one job: run steps, ask the policy, stop when told.
// It does not know what agents are.
// It does not know what tools are.
// It does not know what limits exist — the policy knows that.
// It only knows: ask before every step, obey the answer.

use crate::agent::{
    limits::Limits,
    state::{ExecutionState, StopReason},
};
use crate::events::event::{ExecutionEvent, ExecutionId};
use crate::policy::{Policy, PolicyContext, PolicyDecision};
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
    /// Kept here so they can be written into the ExecutionStarted event.
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
    /// Before every step, the executor asks the policy for a decision.
    /// If the policy says Deny, execution stops immediately with that reason.
    /// If the policy says Allow, the step runs.
    ///
    /// Always returns an `ExecutionResult` with an explicit stop reason.
    /// The loop cannot be paused, retried, or soft-stopped.
    pub fn run<P, F>(&mut self, policy: &P, mut step_fn: F) -> ExecutionResult
    where
        P: Policy,
        F: FnMut(u32) -> StepOutcome,
    {
        // Record the wall-clock start time.
        // `Instant` is monotonic — unaffected by system clock changes.
        let start = Instant::now();

        // Move from Initialized → Running.
        self.state = ExecutionState::Running;

        // First event: announce execution has started with its full limits.
        self.emit(ExecutionEvent::ExecutionStarted {
            execution_id: self.execution_id,
            limits: self.limits.clone(),
            timestamp: Utc::now(),
        });

        // ── The loop ──────────────────────────────────────────────────────────
        //
        // Every iteration represents one potential step.
        // The loop only exits via `break`. Breaking with a value means
        // `stop_reason` receives that value when the loop ends.
        let stop_reason = loop {

            // ── Build the policy context ──────────────────────────────────────
            //
            // Snapshot everything the policy needs to make its decision.
            // No tool is being requested yet — that comes in Day 8 when
            // the tool system is wired in.
            let elapsed_ms = start.elapsed().as_millis() as u64;
            let context = PolicyContext {
                step_count: self.step_count,
                elapsed_ms,
                requested_tool: None,
                cost_units_spent: 0, // ledger wired on Day 7
            };

            // ── Ask the policy ────────────────────────────────────────────────
            //
            // The policy is the single authority on whether execution continues.
            // The executor does not second-guess this decision.
            match policy.evaluate(&context) {
                PolicyDecision::Allow => {
                    // Policy says go. Fall through to run the step.
                }
                PolicyDecision::Deny { reason } => {
                    // Policy says stop. Break immediately with the reason.
                    // No step runs. No further checks happen.
                    break reason;
                }
            }

            // ── Step begins ───────────────────────────────────────────────────
            let current_step = self.step_count;

            self.emit(ExecutionEvent::StepStarted {
                execution_id: self.execution_id,
                step: current_step,
                timestamp: Utc::now(),
            });

            // ── Hand off to the agent ─────────────────────────────────────────
            let outcome = step_fn(current_step);

            // Step is done. Increment the counter.
            self.step_count += 1;

            self.emit(ExecutionEvent::StepCompleted {
                execution_id: self.execution_id,
                step: current_step,
                timestamp: Utc::now(),
            });

            // ── Decide what happens next ──────────────────────────────────────
            match outcome {
                StepOutcome::Continue => {
                    // Keep going. Policy will be checked at the top of the
                    // next iteration before any work begins.
                }
                StepOutcome::Done => {
                    break StopReason::AgentCompleted;
                }
            }
        };
        // ── End of loop ───────────────────────────────────────────────────────

        // Move to terminal state.
        self.state = ExecutionState::Stopped {
            reason: stop_reason.clone(),
        };

        // Final event: always the last entry in a complete execution log.
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
    /// Private — only the executor emits events.
    fn emit(&mut self, event: ExecutionEvent) {
        self.events.push(event);
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::limits::Limits;

    // ── Test policies ─────────────────────────────────────────────────────────
    //
    // Minimal Policy implementations used only in tests.
    // They live here because nanny-core cannot depend on nanny-policy
    // (that would be a circular dependency).
    // Real code uses LimitsPolicy from nanny-policy.

    /// Always allows every step.
    struct AlwaysAllow;
    impl Policy for AlwaysAllow {
        fn evaluate(&self, _ctx: &PolicyContext) -> PolicyDecision {
            PolicyDecision::Allow
        }
    }

    /// Stops after a fixed number of steps.
    struct MaxStepsPolicy(u32);
    impl Policy for MaxStepsPolicy {
        fn evaluate(&self, ctx: &PolicyContext) -> PolicyDecision {
            if ctx.step_count >= self.0 {
                PolicyDecision::Deny {
                    reason: StopReason::MaxStepsReached,
                }
            } else {
                PolicyDecision::Allow
            }
        }
    }

    /// Stops when elapsed time exceeds a threshold in milliseconds.
    struct TimeoutPolicy(u64);
    impl Policy for TimeoutPolicy {
        fn evaluate(&self, ctx: &PolicyContext) -> PolicyDecision {
            if ctx.elapsed_ms >= self.0 {
                PolicyDecision::Deny {
                    reason: StopReason::TimeoutExpired,
                }
            } else {
                PolicyDecision::Allow
            }
        }
    }

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
        let result = executor.run(&MaxStepsPolicy(5), |_step| StepOutcome::Continue);

        assert_eq!(result.stop_reason, StopReason::MaxStepsReached);
        assert_eq!(result.total_steps, 5);
    }

    #[test]
    fn stops_when_agent_is_done() {
        let mut executor = Executor::new(test_limits());
        let result = executor.run(&AlwaysAllow, |step| {
            if step == 2 {
                StepOutcome::Done
            } else {
                StepOutcome::Continue
            }
        });

        assert_eq!(result.stop_reason, StopReason::AgentCompleted);
        assert_eq!(result.total_steps, 3);
    }

    #[test]
    fn stops_on_timeout() {
        let mut executor = Executor::new(test_limits());
        let result = executor.run(&TimeoutPolicy(1), |_step| {
            std::thread::sleep(std::time::Duration::from_millis(10));
            StepOutcome::Continue
        });

        assert_eq!(result.stop_reason, StopReason::TimeoutExpired);
    }

    #[test]
    fn event_log_starts_with_execution_started() {
        let mut executor = Executor::new(test_limits());
        let result = executor.run(&AlwaysAllow, |_| StepOutcome::Done);

        assert!(
            matches!(
                result.events.first(),
                Some(ExecutionEvent::ExecutionStarted { .. })
            ),
            "first event must always be ExecutionStarted"
        );
    }

    #[test]
    fn event_log_ends_with_execution_stopped() {
        let mut executor = Executor::new(test_limits());
        let result = executor.run(&MaxStepsPolicy(5), |_| StepOutcome::Continue);

        assert!(
            matches!(
                result.events.last(),
                Some(ExecutionEvent::ExecutionStopped { .. })
            ),
            "last event must always be ExecutionStopped"
        );
    }

    #[test]
    fn policy_deny_prevents_step_from_running() {
        struct DenyAll;
        impl Policy for DenyAll {
            fn evaluate(&self, _ctx: &PolicyContext) -> PolicyDecision {
                PolicyDecision::Deny {
                    reason: StopReason::ManualStop,
                }
            }
        }

        let mut executor = Executor::new(test_limits());
        let mut step_was_called = false;

        let result = executor.run(&DenyAll, |_step| {
            step_was_called = true;
            StepOutcome::Continue
        });

        assert!(!step_was_called, "step must not run when policy denies");
        assert_eq!(result.stop_reason, StopReason::ManualStop);
        assert_eq!(result.total_steps, 0);
    }
}
