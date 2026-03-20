// The ledger contract.
//
// This module defines the shapes the ledger works with.
// Concrete implementations live in nanny-ledger.
//
// The same separation applies here as with the policy:
//   nanny-core defines the contract
//   nanny-ledger implements it
//   nanny-core's executor uses the contract
//
// The ledger tracks one thing: how much has been spent.
// It does not know about pricing, currencies, or payment rails.
// It enforces: balance reaches zero → execution stops.

use thiserror::Error;

// ── Receipt ───────────────────────────────────────────────────────────────────

/// Proof that a debit happened.
///
/// Emitted by the ledger on every successful debit.
/// Written into the event log as a CostDebited event.
/// The paper trail that makes every spend auditable.
#[derive(Debug, Clone)]
pub struct Receipt {
    /// How many units were spent in this debit.
    pub amount: u64,

    /// How many units remain after this debit.
    pub balance_after: u64,
}

// ── LedgerDecision ────────────────────────────────────────────────────────────

/// What the ledger says when asked if a spend is possible.
///
/// Used by the executor to build PolicyContext before each step.
/// The policy uses `cost_units_spent` to decide whether to allow or deny.
pub enum LedgerDecision {
    /// The balance covers the requested amount.
    Approved,

    /// The balance does not cover the requested amount.
    InsufficientFunds { available: u64, requested: u64 },
}

// ── LedgerError ───────────────────────────────────────────────────────────────

/// Errors that can occur during a debit operation.
///
/// A debit error means the executor attempted to spend more than available.
/// This should not happen if the policy is checking budget correctly —
/// but we handle it explicitly rather than panicking.
#[derive(Debug, Error)]
pub enum LedgerError {
    #[error("insufficient funds: requested {requested} units, only {available} available")]
    InsufficientFunds { requested: u64, available: u64 },
}

// ── Ledger trait ──────────────────────────────────────────────────────────────

/// The ledger contract.
///
/// Any type that implements this can track and enforce spending.
/// In local mode: FakeLedger — abstract units, no real money.
/// In managed mode: AP2-backed ledger — real currency, real settlement.
///
/// The implementation swaps. The contract never changes.
pub trait Ledger {
    /// Check whether a spend of `amount` units is possible.
    ///
    /// Does not mutate state. Safe to call speculatively.
    fn authorize(&self, amount: u64) -> LedgerDecision;

    /// Spend `amount` units and return a receipt.
    ///
    /// Returns an error if the balance is insufficient.
    /// On success the balance is permanently reduced by `amount`.
    fn debit(&mut self, amount: u64) -> Result<Receipt, LedgerError>;

    /// Current unspent balance.
    fn balance(&self) -> u64;

    /// Total units spent across all debits so far.
    ///
    /// Used by the executor to populate `PolicyContext.cost_units_spent`.
    fn total_debited(&self) -> u64;
}
