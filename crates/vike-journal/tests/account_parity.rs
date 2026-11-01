//! PARITY ANCHOR: `vike_analytics::reconstruct_trades` routes through `compute_fill` identically to
//! `vike_exec::Account::fold`, so the sum of reconstructed trade PnLs equals the sum of the PnLs
//! `Account` accumulates into `closed_pnls` over the SAME fill sequence. This is the
//! live == backtest guarantee made explicit.
//!
//! It sits in this crate (it was `vike-report`'s, dissolved 2026-10-12) for its dependencies, not
//! its subject: it needs `vike_exec` and
//! `vike_analytics` together, and `vike-analytics` names nothing above `vike_model`
//! (`crates/vike-analytics/src/trades.rs`'s module doc points here).

use vike_analytics::reconstruct_trades;
use vike_exec::{Account, BalanceMode};

use vike_model::test_support::reports::fill_event;

#[test]
fn realized_pnl_matches_account_fold() {
    // A mixed sequence: open, add, partial reduce, close, flip, close.
    let fills = vec![
        fill_event(1, 2.0, 100.0, 1, 0.0),
        fill_event(1, 1.0, 130.0, 2, 0.0),
        fill_event(-1, 1.0, 150.0, 3, 0.0),
        fill_event(-1, 2.0, 140.0, 4, 0.0), // closes remaining long 2
        fill_event(-1, 1.0, 120.0, 5, 0.0), // opens short 1
        fill_event(1, 1.0, 110.0, 6, 0.0),  // closes short 1
    ];

    let trades = reconstruct_trades(&fills);
    let trades_pnl: f64 = trades.iter().map(|t| t.pnl).sum();

    // Fold the SAME fills through Account (same venue, default multiplier 1.0, same keying).
    let mut acct = Account::new(1.0, "binance", None, BalanceMode::Delta);
    for f in &fills {
        acct.apply_fill(f);
    }
    let account_pnl: f64 = acct.closed_pnls.iter().sum();

    assert_eq!(trades_pnl, account_pnl, "trade PnLs must equal Account::fold's closed_pnls");
}
