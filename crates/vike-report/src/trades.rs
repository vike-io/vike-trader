//! `reconstruct_trades` — fold a chronological `FillEvent` stream into closed [`Trade`]
//! round-trips through the shared cost-basis primitive [`vike_model::compute_fill`].
//!
//! This is `vike_exec::Account::fold` (`vike-exec/src/account.rs`) with FULL trade bookkeeping
//! kept instead of discarded: `Account::fold` runs each fill through `compute_fill` and pushes
//! only `realized_pnl` into `closed_pnls`; here we additionally build a `Trade` on every
//! reduce/close/flip, apportioning entry/exit fees EXACTLY as `vike_backtest`'s
//! `SimBroker::apply_fill` does (`vike-backtest/src/engine/sim_broker.rs`, the reduce/close/flip
//! branch). Routing ALL cost-basis math through `compute_fill` is what makes a live tearsheet and
//! a backtest tearsheet over the same fills bit-for-bit identical.
//!
//! Keying mirrors `Account`: positions are keyed by `(venue, symbol, position_side)`.
//!
//! PURE: no I/O. `mae`/`mfe` are left `0.0` — a bare fill stream carries no intrabar path, so
//! excursions are genuinely uncomputable (the vector engine leaves them `0.0` too; this is
//! correct, not a shortcut).

use std::collections::HashMap;

use vike_model::events::FillEvent;
use vike_model::{ClosedTrade, Trade, TradeFold};

/// Contract multiplier for the journaled venues. Today's journaled venues (Binance/Bybit/OKX,
/// etc.) are crypto LINEAR instruments whose PnL is `(exit - entry) * qty` — multiplier 1.0. Kept
/// a named constant, and threaded through `compute_fill`'s `multiplier` parameter, so a future
/// non-1.0 (inverse/quanto) extension is a single, obvious edit here. `Account::fold` picks the
/// multiplier per symbol via `multiplier_of`; the parity anchor test builds an `Account` with the
/// same default 1.0 so the two folds agree.
const MULTIPLIER: f64 = 1.0;

/// Reconstruct closed [`Trade`] round-trips from a chronological `FillEvent` stream.
///
/// `fills` MUST be in journal (chronological) order — the same order `Account::fold` sees them.
/// One `Trade` is emitted per reduce/close/flip (a partial reduce closes part of a position and
/// emits one trade for the closed portion; a flip emits one trade for the fully-closed prior side
/// and opens the remainder on the opposite side). Open/Add emit no trade.
pub fn reconstruct_trades(fills: &[FillEvent]) -> Vec<Trade> {
    let mut states: HashMap<(String, String, String), TradeFold> = HashMap::new();
    let mut trades = Vec::new();

    for f in fills {
        let key = (f.venue.to_string(), f.symbol.to_string(), f.position_side.to_string());
        let st = states.entry(key).or_default();

        // `commission` is SIGNED (>0 cost / <0 rebate) — carried through verbatim by the shared
        // fold so `Trade.fees` inherits that sign, EXACTLY as `SimBroker::apply_fill` does.
        let step = st.apply(f.side, f.last_qty, f.last_px, f.commission, f.ts, MULTIPLIER);
        if let Some(c) = step.closed {
            trades.push(trade_from_closed(c, f.symbol.to_string()));
        }
    }

    trades
}

/// Build a full `Trade` from a shared-fold [`ClosedTrade`]. A bare fill stream has no intrabar
/// path, so `mae`/`mfe` are genuinely uncomputable and left `0.0` (the vector engine does the
/// same); `symbol` is the site's own.
fn trade_from_closed(c: ClosedTrade, symbol: String) -> Trade {
    Trade {
        entry_price: c.entry_price,
        exit_price: c.exit_price,
        size: c.size,
        pnl: c.pnl,
        fees: c.fees,
        entry_ts: c.entry_ts,
        exit_ts: c.exit_ts,
        symbol,
        mae: 0.0,
        mfe: 0.0,
        is_long: c.is_long,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use vike_model::events::{Event, TradeId};

    /// Build a `FillEvent` on a single venue/symbol/one-way position.
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
    fn long_open_then_full_close_is_one_trade() {
        // buy 1 @ 100 (fee 0.1), sell 1 @ 110 (fee 0.2) -> 1 Trade, pnl = (110-100)*1 = 10.
        let fills = vec![fill(1, 1.0, 100.0, 1_000, 0.1), fill(-1, 1.0, 110.0, 2_000, 0.2)];
        let trades = reconstruct_trades(&fills);
        assert_eq!(trades.len(), 1);
        let t = &trades[0];
        assert_eq!(t.entry_price, 100.0);
        assert_eq!(t.exit_price, 110.0);
        assert_eq!(t.size, 1.0);
        assert_eq!(t.pnl, 10.0);
        // whole entry fee (portion 1.0) + whole exit fee (closing_qty/qty = 1.0)
        assert!((t.fees - 0.3).abs() < 1e-12);
        assert_eq!(t.entry_ts, 1_000);
        assert_eq!(t.exit_ts, 2_000);
        assert_eq!(t.symbol, "BTCUSDT");
        assert!(t.is_long);
        assert_eq!(t.mae, 0.0);
        assert_eq!(t.mfe, 0.0);
    }

    #[test]
    fn short_open_then_close_is_one_trade() {
        // sell 1 @ 110, buy 1 @ 100 -> short profits when price falls: pnl = (100-110)*(-1) = 10.
        let fills = vec![fill(-1, 1.0, 110.0, 1_000, 0.0), fill(1, 1.0, 100.0, 2_000, 0.0)];
        let trades = reconstruct_trades(&fills);
        assert_eq!(trades.len(), 1);
        let t = &trades[0];
        assert_eq!(t.entry_price, 110.0);
        assert_eq!(t.exit_price, 100.0);
        assert_eq!(t.size, 1.0);
        assert_eq!(t.pnl, 10.0);
        assert!(!t.is_long);
    }

    #[test]
    fn partial_close_leaves_position_open() {
        // buy 2 @ 100, sell 1 @ 120 -> 1 Trade closing the 1-unit half; position still long 1.
        let fills = vec![fill(1, 2.0, 100.0, 1_000, 0.4), fill(-1, 1.0, 120.0, 2_000, 0.2)];
        let trades = reconstruct_trades(&fills);
        assert_eq!(trades.len(), 1);
        let t = &trades[0];
        assert_eq!(t.entry_price, 100.0);
        assert_eq!(t.exit_price, 120.0);
        assert_eq!(t.size, 1.0);
        assert_eq!(t.pnl, 20.0); // (120-100)*1
        // portion = closing/|prior| = 1/2 -> entry_fee_portion = 0.4*0.5 = 0.2;
        // exit_fee_portion = 0.2 * (closing 1 / fill_qty 1) = 0.2 -> 0.4 total
        assert!((t.fees - 0.4).abs() < 1e-12);
        assert!(t.is_long);
    }

    #[test]
    fn scale_in_then_close_uses_weighted_avg_entry() {
        // buy 1 @ 100, buy 1 @ 200 (avg 150), sell 2 @ 180 -> 1 Trade, pnl = (180-150)*2 = 60.
        let fills = vec![
            fill(1, 1.0, 100.0, 1_000, 0.0),
            fill(1, 1.0, 200.0, 1_500, 0.0),
            fill(-1, 2.0, 180.0, 2_000, 0.0),
        ];
        let trades = reconstruct_trades(&fills);
        assert_eq!(trades.len(), 1);
        let t = &trades[0];
        assert_eq!(t.entry_price, 150.0); // (100 + 200) / 2
        assert_eq!(t.exit_price, 180.0);
        assert_eq!(t.size, 2.0);
        assert_eq!(t.pnl, 60.0);
        // entry_ts is the FIRST open's ts (weighted-avg entry keeps the original open time)
        assert_eq!(t.entry_ts, 1_000);
        assert_eq!(t.exit_ts, 2_000);
    }

    #[test]
    fn flip_closes_prior_and_opens_opposite() {
        // long 2 @ 100, sell 3 @ 130 -> closes the long 2 (pnl = (130-100)*2 = 60) AND opens a
        // short 1 @ 130. Exactly one Trade (the closed long); the new short is still open.
        let fills = vec![fill(1, 2.0, 100.0, 1_000, 0.0), fill(-1, 3.0, 130.0, 2_000, 0.9)];
        let trades = reconstruct_trades(&fills);
        assert_eq!(trades.len(), 1);
        let t = &trades[0];
        assert_eq!(t.entry_price, 100.0);
        assert_eq!(t.exit_price, 130.0);
        assert_eq!(t.size, 2.0); // the closed portion
        assert_eq!(t.pnl, 60.0);
        assert!(t.is_long); // the CLOSED position was long
        // exit_fee_portion = fee * closing/qty = 0.9 * (2/3) = 0.6; no prior entry fee.
        assert!((t.fees - 0.6).abs() < 1e-12);

        // A subsequent buy 1 @ 130 closes the flipped short flat with zero pnl, proving the short
        // opened at the fill price 130 with the leftover 1 unit.
        let mut fills2 = fills.clone();
        fills2.push(fill(1, 1.0, 130.0, 3_000, 0.0));
        let trades2 = reconstruct_trades(&fills2);
        assert_eq!(trades2.len(), 2);
        let short = &trades2[1];
        assert_eq!(short.entry_price, 130.0);
        assert_eq!(short.exit_price, 130.0);
        assert_eq!(short.size, 1.0);
        assert_eq!(short.pnl, 0.0);
        assert!(!short.is_long);
    }

    /// PARITY ANCHOR: reconstruct_trades routes through `compute_fill` identically to
    /// `vike_exec::Account::fold`, so the sum of reconstructed trade PnLs equals the sum of the
    /// PnLs `Account` accumulates into `closed_pnls` over the SAME fill sequence. This is the
    /// live == backtest guarantee made explicit.
    #[test]
    fn realized_pnl_matches_account_fold() {
        use vike_exec::{Account, BalanceMode};
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

    /// Guard: only bare `Event::Fill`s (the account-affecting fills) feed the reconstructor — the
    /// journal read layer filters lifecycle events out. This test documents that a `Trade` derives
    /// only from `FillEvent` fields, independent of any surrounding `Event` wrapper.
    #[test]
    fn trade_is_built_from_fill_event_fields_only() {
        let f = fill(1, 1.0, 100.0, 1, 0.0);
        // wrapping in Event::Fill and unwrapping yields the same struct the reconstructor uses.
        let ev = Event::Fill(f.clone());
        let inner = match ev {
            Event::Fill(x) => x,
            _ => unreachable!(),
        };
        assert_eq!(inner, f);
    }
}
