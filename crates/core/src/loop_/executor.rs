// The deterministic execution loop.
//
// The executor owns one job: run steps, ask the policy, stop when told.
// It does not know what agents are.
// It does not know what tools are.
// It does not know what limits exist — the policy knows that.
// It does not know what money is — the ledger knows that.
// It only knows: ask before every step, obey the answer, record everything.

use crate::agent::{
    limits::Limits,
    state::{ExecutionState, StopReason},
};
use crate::events::event::{ExecutionEvent, ExecutionId};
use crate::ledger::Ledger;
use crate::policy::{Policy, PolicyContext, PolicyDecision};
use chrono::Utc;
use std::time::Instant;
use uuid::Uuid;

/// Cost charged to the ledger for each completed step.
///
/// In local mode this is an abstract unit — 1 unit per step.
/// When tools are wired in (Day 8), tool calls will carry their own
/// declared costs on top of this baseline.
const COST_PER_STEP: u64 = 1;

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

    /// Total cost units spent across all steps.
    pub total_cost: u64,

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
    /// Before every step:
    ///   1. The executor builds a PolicyContext from current state
    ///   2. Asks the policy for a decision
    ///   3. If Deny → stop immediately
    ///   4. If Allow → run the step, debit the ledger, emit events
    ///
    /// Always returns an `ExecutionResult` with an explicit stop reason.
    /// The loop cannot be paused, retried, or soft-stopped.
    pub fn run<P, L, F>(&mut self, policy: &P, ledger: &mut L, mut step_fn: F) -> ExecutionResult
    where
        P: Policy,
        L: Ledger,
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
        let stop_reason = loop {

            // ── Build the policy context ──────────────────────────────────────
            //
            // `ledger.total_debited()` replaces the hardcoded 0 from Day 6.
            // The policy now sees real spend data and can enforce budget limits.
            let elapsed_ms = start.elapsed().as_millis() as u64;
            let context = PolicyContext {
                step_count: self.step_count,
                elapsed_ms,
                requested_tool: None, // tool system wired on Day 8
                cost_units_spent: ledger.total_debited(),
            };

            // ── Ask the policy ────────────────────────────────────────────────
            match policy.evaluate(&context) {
                PolicyDecision::Allow => {
                    // Policy says go. Fall through to run the step.
                }
                PolicyDecision::Deny { reason } => {
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

            // ── Debit the ledger ──────────────────────────────────────────────
            //
            // The step ran — charge for it.
            // If debit fails, the policy should have caught the budget issue
            // before this point. If we somehow get here with insufficient funds,
            // we stop as BudgetExhausted — the closest accurate reason.
            match ledger.debit(COST_PER_STEP) {
                Ok(receipt) => {
                    self.emit(ExecutionEvent::CostDebited {
                        execution_id: self.execution_id,
                        amount: receipt.amount,
                        balance_remaining: receipt.balance_after,
                        timestamp: Utc::now(),
                    });
                }
                Err(_) => {
                    break StopReason::BudgetExhausted;
                }
            }

            // ── Decide what happens next ──────────────────────────────────────
            match outcome {
                StepOutcome::Continue => {}
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
            total_cost: ledger.total_debited(),
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
    use crate::ledger::{LedgerDecision, LedgerError, Receipt};

    // ── Test policies ─────────────────────────────────────────────────────────

    struct AlwaysAllow;
    impl Policy for AlwaysAllow {
        fn evaluate(&self, _ctx: &PolicyContext) -> PolicyDecision {
            PolicyDecision::Allow
        }
    }

    struct MaxStepsPolicy(u32);
    impl Policy for MaxStepsPolicy {
        fn evaluate(&self, ctx: &PolicyContext) -> PolicyDecision {
            if ctx.step_count >= self.0 {
                PolicyDecision::Deny { reason: StopReason::MaxStepsReached }
            } else {
                PolicyDecision::Allow
            }
        }
    }

    struct TimeoutPolicy(u64);
    impl Policy for TimeoutPolicy {
        fn evaluate(&self, ctx: &PolicyContext) -> PolicyDecision {
            if ctx.elapsed_ms >= self.0 {
                PolicyDecision::Deny { reason: StopReason::TimeoutExpired }
            } else {
                PolicyDecision::Allow
            }
        }
    }

    struct BudgetPolicy(u64);
    impl Policy for BudgetPolicy {
        fn evaluate(&self, ctx: &PolicyContext) -> PolicyDecision {
            if ctx.cost_units_spent >= self.0 {
                PolicyDecision::Deny { reason: StopReason::BudgetExhausted }
            } else {
                PolicyDecision::Allow
            }
        }
    }

    // ── Test ledgers ──────────────────────────────────────────────────────────
    //
    // Same reasoning as test policies: nanny-core cannot depend on nanny-ledger.
    // Real code uses FakeLedger from nanny-ledger.

    /// A ledger with unlimited balance. Never denies.
    struct UnlimitedLedger {
        total_debited: u64,
    }
    impl UnlimitedLedger {
        fn new() -> Self { Self { total_debited: 0 } }
    }
    impl Ledger for UnlimitedLedger {
        fn authorize(&self, _: u64) -> LedgerDecision { LedgerDecision::Approved }
        fn debit(&mut self, amount: u64) -> Result<Receipt, LedgerError> {
            self.total_debited += amount;
            Ok(Receipt { amount, balance_after: u64::MAX })
        }
        fn balance(&self) -> u64 { u64::MAX }
        fn total_debited(&self) -> u64 { self.total_debited }
    }

    fn test_limits() -> Limits {
        Limits { max_steps: 5, max_cost_units: 1000, timeout_ms: 5000 }
    }

    #[test]
    fn stops_at_max_steps() {
        let mut executor = Executor::new(test_limits());
        let mut ledger = UnlimitedLedger::new();
        let result = executor.run(&MaxStepsPolicy(5), &mut ledger, |_| StepOutcome::Continue);

        assert_eq!(result.stop_reason, StopReason::MaxStepsReached);
        assert_eq!(result.total_steps, 5);
    }

    #[test]
    fn stops_when_agent_is_done() {
        let mut executor = Executor::new(test_limits());
        let mut ledger = UnlimitedLedger::new();
        let result = executor.run(&AlwaysAllow, &mut ledger, |step| {
            if step == 2 { StepOutcome::Done } else { StepOutcome::Continue }
        });

        assert_eq!(result.stop_reason, StopReason::AgentCompleted);
        assert_eq!(result.total_steps, 3);
    }

    #[test]
    fn stops_on_timeout() {
        let mut executor = Executor::new(test_limits());
        let mut ledger = UnlimitedLedger::new();
        let result = executor.run(&TimeoutPolicy(1), &mut ledger, |_| {
            std::thread::sleep(std::time::Duration::from_millis(10));
            StepOutcome::Continue
        });

        assert_eq!(result.stop_reason, StopReason::TimeoutExpired);
    }

    #[test]
    fn stops_on_budget_exhausted() {
        let mut executor = Executor::new(test_limits());
        let mut ledger = UnlimitedLedger::new();

        // Budget policy stops when 3 units have been spent.
        // Each step costs COST_PER_STEP (1), so stop after 3 steps.
        let result = executor.run(&BudgetPolicy(3), &mut ledger, |_| StepOutcome::Continue);

        assert_eq!(result.stop_reason, StopReason::BudgetExhausted);
        assert_eq!(result.total_steps, 3);
        assert_eq!(result.total_cost, 3);
    }

    #[test]
    fn ledger_is_debited_per_step() {
        let mut executor = Executor::new(test_limits());
        let mut ledger = UnlimitedLedger::new();
        let result = executor.run(&MaxStepsPolicy(3), &mut ledger, |_| StepOutcome::Continue);

        assert_eq!(result.total_cost, 3); // 3 steps × 1 unit each
    }

    #[test]
    fn event_log_contains_cost_debited_events() {
        let mut executor = Executor::new(test_limits());
        let mut ledger = UnlimitedLedger::new();
        let result = executor.run(&MaxStepsPolicy(3), &mut ledger, |_| StepOutcome::Continue);

        let debit_count = result.events.iter()
            .filter(|e| matches!(e, ExecutionEvent::CostDebited { .. }))
            .count();

        assert_eq!(debit_count, 3, "one CostDebited event per completed step");
    }

    #[test]
    fn event_log_starts_with_execution_started() {
        let mut executor = Executor::new(test_limits());
        let mut ledger = UnlimitedLedger::new();
        let result = executor.run(&AlwaysAllow, &mut ledger, |_| StepOutcome::Done);

        assert!(matches!(
            result.events.first(),
            Some(ExecutionEvent::ExecutionStarted { .. })
        ));
    }

    #[test]
    fn event_log_ends_with_execution_stopped() {
        let mut executor = Executor::new(test_limits());
        let mut ledger = UnlimitedLedger::new();
        let result = executor.run(&MaxStepsPolicy(5), &mut ledger, |_| StepOutcome::Continue);

        assert!(matches!(
            result.events.last(),
            Some(ExecutionEvent::ExecutionStopped { .. })
        ));
    }

    #[test]
    fn policy_deny_prevents_step_from_running() {
        struct DenyAll;
        impl Policy for DenyAll {
            fn evaluate(&self, _ctx: &PolicyContext) -> PolicyDecision {
                PolicyDecision::Deny { reason: StopReason::ManualStop }
            }
        }

        let mut executor = Executor::new(test_limits());
        let mut ledger = UnlimitedLedger::new();
        let mut step_was_called = false;

        let result = executor.run(&DenyAll, &mut ledger, |_| {
            step_was_called = true;
            StepOutcome::Continue
        });

        assert!(!step_was_called);
        assert_eq!(result.stop_reason, StopReason::ManualStop);
        assert_eq!(result.total_steps, 0);
        assert_eq!(result.total_cost, 0);
    }
}
