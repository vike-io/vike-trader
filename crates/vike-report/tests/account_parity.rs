//! PARITY ANCHOR: `vike_analytics::reconstruct_trades` routes through `compute_fill` identically to
//! `vike_exec::Account::fold`, so the sum of reconstructed trade PnLs equals the sum of the PnLs
//! `Account` accumulates into `closed_pnls` over the SAME fill sequence. This is the
//! live == backtest guarantee made explicit.
//!
//! ⚠ It lived inside `trades.rs`'s unit tests until 2026-09-28, `#[cfg(feature = "journal")]`
//! there because `vike_exec` was an optional dependency that feature enabled. `trades.rs` then moved
//! to `vike-analytics`, which names nothing above `vike_model` — so this assertion stayed in the
//! crate that links BOTH halves, as an integration test over their public APIs. It is the one test
//! in this crate that reads no journal and no store; it is here for its dependencies, not its
//! subject. `crates/vike-analytics/src/trades.rs`'s module doc points here.

use vike_analytics::reconstruct_trades;
use vike_exec::{Account, BalanceMode};
use vike_model::events::{FillEvent, TradeId};

/// Build a `FillEvent` on a single venue/symbol/one-way position — the same helper
/// `crates/vike-analytics/src/trades.rs`'s unit tests use, spelled here because a helper in another
/// crate's `#[cfg(test)]` module is not reachable from this one.
fn fill(side: i32, qty: f64, px: f64, ts: i64, commission: f64) -> FillEvent {
    FillEvent {
        // minted here, not read off a wire — same `t<ts>-<side>` bytes as before
        trade_id: TradeId::prefixed("t", format_args!("{ts}-{side}")),
        client_order_id: String::new(),
        venue: "binance".into(),
        symbol: "BTCUSDT".into(),
        side,
        last_qty: qty,
        last_px: px,
        commission,
        commission_asset: String::new().into(),
        liquidity_side: "taker".into(),
        ts,
        mark_price: Some(px),
        position_side: "BOTH".into(),
    }
}

#[test]
fn realized_pnl_matches_account_fold() {
    // A mixed sequence: open, add, partial reduce, close, flip, close.
    let fills = vec![
        fill(1, 2.0, 100.0, 1, 0.0),
        fill(1, 1.0, 130.0, 2, 0.0),
        fill(-1, 1.0, 150.0, 3, 0.0),
        fill(-1, 2.0, 140.0, 4, 0.0), // closes remaining long 2
        fill(-1, 1.0, 120.0, 5, 0.0), // opens short 1
        fill(1, 1.0, 110.0, 6, 0.0),  // closes short 1
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
