// FakeLedger — in-memory ledger for local mode.
//
// Fake money. Real stops.
//
// The numbers mean nothing outside this process.
// The enforcement is identical to what a real ledger will provide.
// When AP2 arrives, this implementation is swapped out.
// The executor, policy, and event log do not change at all.

use nanny_core::ledger::{Ledger, LedgerDecision, LedgerError, Receipt};

// ── FakeLedger ────────────────────────────────────────────────────────────────

/// In-memory ledger for local mode execution.
///
/// Initialized with a balance derived from `nanny.toml → limits.max_cost_units`.
/// Every debit reduces the balance. When balance hits zero, the policy stops
/// execution via `BudgetExhausted`.
///
/// No persistence. No network. No real money.
/// The receipts are real records — they just don't move actual funds.
pub struct FakeLedger {
    /// Current unspent balance.
    balance: u64,

    /// Running total of all units debited so far.
    total_debited: u64,
}

impl FakeLedger {
    /// Create a new FakeLedger with the given initial balance.
    ///
    /// Pass `limits.max_cost_units` from your nanny.toml here.
    /// That makes the budget limit and the ledger balance consistent.
    pub fn new(initial_balance: u64) -> Self {
        Self {
            balance: initial_balance,
            total_debited: 0,
        }
    }
}

impl Ledger for FakeLedger {
    /// Check whether a spend is possible without committing to it.
    ///
    /// Called by the executor to build PolicyContext.
    /// Does not change the balance.
    fn authorize(&self, amount: u64) -> LedgerDecision {
        if amount <= self.balance {
            LedgerDecision::Approved
        } else {
            LedgerDecision::InsufficientFunds {
                available: self.balance,
                requested: amount,
            }
        }
    }

    /// Spend `amount` units and return a receipt.
    ///
    /// Reduces the balance permanently.
    /// Returns an error if the balance is insufficient.
    fn debit(&mut self, amount: u64) -> Result<Receipt, LedgerError> {
        if amount > self.balance {
            return Err(LedgerError::InsufficientFunds {
                requested: amount,
                available: self.balance,
            });
        }

        self.balance -= amount;
        self.total_debited += amount;

        Ok(Receipt {
            amount,
            balance_after: self.balance,
        })
    }

    /// Current unspent balance.
    fn balance(&self) -> u64 {
        self.balance
    }

    /// Total units spent across all debits.
    fn total_debited(&self) -> u64 {
        self.total_debited
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn starts_with_full_balance() {
        let ledger = FakeLedger::new(100);
        assert_eq!(ledger.balance(), 100);
        assert_eq!(ledger.total_debited(), 0);
    }

    #[test]
    fn debit_reduces_balance() {
        let mut ledger = FakeLedger::new(100);
        let receipt = ledger.debit(30).unwrap();

        assert_eq!(receipt.amount, 30);
        assert_eq!(receipt.balance_after, 70);
        assert_eq!(ledger.balance(), 70);
        assert_eq!(ledger.total_debited(), 30);
    }

    #[test]
    fn multiple_debits_accumulate() {
        let mut ledger = FakeLedger::new(100);
        ledger.debit(10).unwrap();
        ledger.debit(20).unwrap();
        ledger.debit(30).unwrap();

        assert_eq!(ledger.balance(), 40);
        assert_eq!(ledger.total_debited(), 60);
    }

    #[test]
    fn debit_exact_balance_succeeds() {
        let mut ledger = FakeLedger::new(50);
        let receipt = ledger.debit(50).unwrap();

        assert_eq!(receipt.balance_after, 0);
        assert_eq!(ledger.balance(), 0);
    }

    #[test]
    fn debit_over_balance_fails() {
        let mut ledger = FakeLedger::new(10);
        let result = ledger.debit(11);

        assert!(result.is_err());
        // Balance must be unchanged after a failed debit.
        assert_eq!(ledger.balance(), 10);
        assert_eq!(ledger.total_debited(), 0);
    }

    #[test]
    fn authorize_approves_within_balance() {
        let ledger = FakeLedger::new(100);
        assert!(matches!(ledger.authorize(100), LedgerDecision::Approved));
    }

    #[test]
    fn authorize_denies_over_balance() {
        let ledger = FakeLedger::new(10);
        assert!(matches!(
            ledger.authorize(11),
            LedgerDecision::InsufficientFunds { available: 10, requested: 11 }
        ));
    }

    #[test]
    fn failed_debit_does_not_change_total_debited() {
        let mut ledger = FakeLedger::new(5);
        ledger.debit(3).unwrap();
        let _ = ledger.debit(10); // fails

        assert_eq!(ledger.total_debited(), 3); // only the successful debit counts
        assert_eq!(ledger.balance(), 2);
    }
}
