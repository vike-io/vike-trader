//! **The one fold of a currency group's blocks into one row of totals**: what
//! `vike_execution::AccountLedgerTotals::of` (the core's snapshot and the tradehub's reports over the blocks that
//! still run) and the CLI's `equity` line (over the wire's blocks, in a crate that links no `vike-execution`) all call,
//! so a report cannot add one group's money by another law.
//!
//! The law: every sum is `py_sum` in the blocks' order, except `margin_used`, a plain left fold from `+0.0`, as the
//! core's `margin_used_total` is. WHICH blocks is the caller's: one currency group, all of a core's or the current
//! ones. Cold: a report builds this, never the fold.

use crate::pysum::py_sum;

/// What one engine's block adds to its currency group's totals.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct AccountLedgerBlock {
    pub equity: f64,
    pub realized_pnl: f64,
    pub fees_paid: f64,
    pub funding_paid: f64,
    pub unrealized: f64,
    pub margin_used: f64,
}

/// One currency group's totals ([`ledger_sums`]).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct LedgerSums {
    pub equity: f64,
    pub realized_pnl: f64,
    pub fees_paid: f64,
    pub funding_paid: f64,
    pub unrealized: f64,
    pub margin_used: f64,
    /// `realized_pnl - fees_paid + funding_paid + unrealized` per block, then summed: the engine-side
    /// `resolved_own_pnl`'s term order, which a sum of the four totals would not reproduce bit for bit.
    pub pnl: f64,
}

/// Fold `blocks`, in the order given, into one row of totals.
#[must_use]
pub fn ledger_sums(blocks: impl IntoIterator<Item = AccountLedgerBlock>) -> LedgerSums {
    let blocks: Vec<AccountLedgerBlock> = blocks.into_iter().collect();
    LedgerSums {
        equity: py_sum(blocks.iter().map(|b| b.equity)),
        realized_pnl: py_sum(blocks.iter().map(|b| b.realized_pnl)),
        fees_paid: py_sum(blocks.iter().map(|b| b.fees_paid)),
        funding_paid: py_sum(blocks.iter().map(|b| b.funding_paid)),
        unrealized: py_sum(blocks.iter().map(|b| b.unrealized)),
        margin_used: blocks.iter().fold(0.0, |accumulator, b| accumulator + b.margin_used),
        pnl: py_sum(
            blocks.iter().map(|b| b.realized_pnl - b.fees_paid + b.funding_paid + b.unrealized),
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// **The law, on the numbers that tell the two folds apart**: `py_sum` keeps the 1.0 a left fold loses between
    /// 1e16 and -1e16, `margin_used` is that left fold, and `pnl` nets each block before it sums.
    #[test]
    fn every_total_is_py_sum_in_order_and_margin_is_the_left_fold() {
        let block = |x: f64| AccountLedgerBlock {
            equity: x,
            realized_pnl: x,
            fees_paid: x,
            funding_paid: x,
            unrealized: x,
            margin_used: x,
        };
        let sums = ledger_sums([block(1e16), block(1.0), block(-1e16)]);
        assert_eq!(sums.equity, 1.0, "py_sum");
        assert_eq!(sums.realized_pnl, 1.0);
        assert_eq!(sums.fees_paid, 1.0);
        assert_eq!(sums.funding_paid, 1.0);
        assert_eq!(sums.unrealized, 1.0);
        assert_eq!(sums.margin_used, 0.0, "the left fold loses it");
        let pnl = py_sum([1e16 - 1e16 + 1e16 + 1e16, 1.0 - 1.0 + 1.0 + 1.0, -2e16]);
        assert_eq!(sums.pnl.to_bits(), pnl.to_bits(), "netted per block, in that term order");
    }

    /// No block reads `+0.0` in every total, never an empty `Iterator::sum`'s `-0.0`.
    #[test]
    fn no_block_is_positive_zero() {
        let sums = ledger_sums([]);
        for x in [sums.equity, sums.realized_pnl, sums.margin_used, sums.pnl] {
            assert_eq!(x.to_bits(), 0.0_f64.to_bits());
        }
    }
}
