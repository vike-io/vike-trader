//! A forward test held against its backtest: the same strategy over the same window, recorded twice.
//! The backtest's closed trades (a saved run's ledger) and the forward test's (a paper or demo
//! account's own fills, folded by [`crate::reconstruct_trades`]) go in; what the market did
//! differently comes out. Pure: every input is a `vike_model::Trade` the caller already holds.
//!
//! - [`round_trips`] merges the pieces one position was closed in into ONE round trip, so a forward
//!   exit filled in three parts and a backtest exit filled whole compare as one trade each.
//! - [`match_trades`] pairs each backtest round trip with the unpaired forward round trip of the same
//!   direction whose entry is nearest, within a tolerance. What stays unpaired is a signal one side
//!   traded and the other did not.
//! - [`ForwardTestTradePair`] holds a pair's differences: the entry delay, the slippage at entry and at exit
//!   in basis points (POSITIVE means the forward paid more than the backtest), and both net returns.
//! - [`realized_daily_equity`] puts a record on the window's own day grid, so two records' returns and
//!   Sharpes compare day for day: realized PnL net of fees, booked on the day a round trip closes (the
//!   live tearsheet's fold, [`crate::equity_curve_from_trades`] sampled by its daily rule).
//!
//! How many days or trades it takes before the two records can be told apart is
//! [`crate::overfit::minimum_track_record_length`]'s question.

use std::collections::HashMap;

use vike_model::{ContractValue, Trade};

/// Basis points in one.
const BASIS_POINTS: f64 = 10_000.0;

/// `trades` with the pieces of each position merged into one round trip, in the order each position's
/// first piece closed. A piece belongs to a position by `(symbol, is_long, entry_timestamp)`: the trade
/// fold stamps every piece a position closes in with the position's opening time. The merge keeps
/// `size`, `pnl` and `fees` as sums, both prices as size-weighted means, the LAST exit time, and the
/// worst `mae` and best `mfe`.
pub fn round_trips(trades: &[Trade]) -> Vec<Trade> {
    let mut merged: Vec<Trade> = Vec::with_capacity(trades.len());
    let mut position: HashMap<(&str, bool, i64), usize> = HashMap::new();
    for t in trades {
        let key = (t.symbol.as_str(), t.is_long, t.entry_timestamp);
        let Some(&at) = position.get(&key) else {
            position.insert(key, merged.len());
            merged.push(t.clone());
            continue;
        };
        let m = &mut merged[at];
        let size = m.size + t.size;
        if size != 0.0 {
            // shortcut: a `Trade` names no contract, so the pieces average linearly; an inverse
            // (coin-margined) record would average harmonically once the ledger carries its contract.
            let value = ContractValue::default();
            m.entry_price = value.average_price(m.size, m.entry_price, t.size, t.entry_price);
            m.exit_price = value.average_price(m.size, m.exit_price, t.size, t.exit_price);
        }
        m.size = size;
        m.pnl += t.pnl;
        m.fees += t.fees;
        m.exit_timestamp = m.exit_timestamp.max(t.exit_timestamp);
        m.mae = m.mae.max(t.mae);
        m.mfe = m.mfe.max(t.mfe);
    }
    merged
}

/// A trade's return on what it put at risk, net of fees: `(pnl − fees)` over its entry notional, the gross
/// notional `value` (the traded contract's) gives `size` at `entry_price`: `|size · entry_price|` for the
/// default linear 1.0, bit for bit, and `|size| · per_unit` (quote money, the unit an inverse trade's PnL is
/// booked in) for an inverse one. `NaN` for a trade with no notional.
pub fn trade_net_return(trade: &Trade, value: ContractValue) -> f64 {
    // `.abs()` over the product: `|size| · price · 1.0` is then `|size · price|` exactly, at any price's sign.
    let notional = value.gross_notional(trade.size, trade.entry_price).abs();
    if notional == 0.0 {
        return f64::NAN;
    }
    (trade.pnl - trade.fees) / notional
}

/// One backtest round trip and the forward round trip it was matched with.
#[derive(Debug, Clone, PartialEq)]
pub struct ForwardTestTradePair {
    /// The backtest trade's index in the slice [`match_trades`] was handed.
    pub backtest: usize,
    /// The forward trade's index in its slice.
    pub forward: usize,
    /// Forward entry time minus backtest entry time, in ms: how late the forward came in.
    pub entry_delay_ms: i64,
    /// What the forward paid over the backtest at entry, in basis points of the backtest's entry
    /// price: a long that bought higher, a short that sold lower, is positive.
    pub entry_slippage_bps: f64,
    /// The same at exit, of the backtest's exit price: a long that sold lower is positive.
    pub exit_slippage_bps: f64,
    /// [`trade_net_return`] of the backtest trade.
    pub backtest_return: f64,
    /// [`trade_net_return`] of the forward trade.
    pub forward_return: f64,
}

impl ForwardTestTradePair {
    fn of(backtest: usize, b: &Trade, forward: usize, f: &Trade, value: ContractValue) -> Self {
        let side = if b.is_long { 1.0 } else { -1.0 };
        ForwardTestTradePair {
            backtest,
            forward,
            entry_delay_ms: f.entry_timestamp.saturating_sub(b.entry_timestamp),
            entry_slippage_bps: side * (f.entry_price - b.entry_price) / b.entry_price
                * BASIS_POINTS,
            exit_slippage_bps: -side * (f.exit_price - b.exit_price) / b.exit_price * BASIS_POINTS,
            backtest_return: trade_net_return(b, value),
            forward_return: trade_net_return(f, value),
        }
    }

    /// Entry plus exit slippage: what the round trip cost the forward over the backtest, in basis points.
    pub fn slippage_bps(&self) -> f64 {
        self.entry_slippage_bps + self.exit_slippage_bps
    }
}

/// What [`match_trades`] found.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct ForwardTestTradeMatching {
    /// The pairs, in the backtest's entry order.
    pub pairs: Vec<ForwardTestTradePair>,
    /// Backtest trades no forward trade answered (indices, in entry order): signals the forward missed.
    pub backtest_only: Vec<usize>,
    /// Forward trades no backtest trade answered (indices, in entry order): trades the backtest never took.
    pub forward_only: Vec<usize>,
}

/// Pair each backtest trade, in entry order, with the unpaired forward trade of the same direction
/// (`is_long`) whose entry is nearest to its own, at most `tolerance_ms` away; ties go to the earlier
/// forward entry. Greedy in time: a pair once made is kept. Hand it ONE instrument's round trips
/// ([`round_trips`]) on each side; a symbol is not compared, because a single-symbol backtest names
/// none. `value` is that instrument's contract, which both net returns are taken on ([`trade_net_return`]).
pub fn match_trades(
    backtest: &[Trade],
    forward: &[Trade],
    tolerance_ms: i64,
    value: ContractValue,
) -> ForwardTestTradeMatching {
    let tolerance = tolerance_ms.max(0);
    let by_entry = |trades: &[Trade]| {
        let mut order: Vec<usize> = (0..trades.len()).collect();
        order.sort_by_key(|&i| (trades[i].entry_timestamp, i));
        order
    };
    let forward_order = by_entry(forward);
    let entries: Vec<i64> = forward_order.iter().map(|&j| forward[j].entry_timestamp).collect();
    let mut taken = vec![false; forward.len()];
    let mut matching = ForwardTestTradeMatching::default();
    for i in by_entry(backtest) {
        let b = &backtest[i];
        let from = entries.partition_point(|&t| t < b.entry_timestamp.saturating_sub(tolerance));
        let to = entries.partition_point(|&t| t <= b.entry_timestamp.saturating_add(tolerance));
        let nearest = forward_order[from..to]
            .iter()
            .copied()
            .filter(|&j| !taken[j] && forward[j].is_long == b.is_long)
            .min_by_key(|&j| forward[j].entry_timestamp.abs_diff(b.entry_timestamp));
        match nearest {
            Some(j) => {
                taken[j] = true;
                matching.pairs.push(ForwardTestTradePair::of(i, b, j, &forward[j], value));
            }
            None => matching.backtest_only.push(i),
        }
    }
    matching.forward_only = forward_order.into_iter().filter(|&j| !taken[j]).collect();
    matching
}

/// A record's realized equity, one sample per UTC day over `[start_ms, end_ms]`: `capital` at the
/// window's opening, then each day's close, from the trades that CLOSED inside the window (sorted by exit
/// time here). Two records sampled on one window share one day grid, so their daily returns line up and
/// a day without a trade is a zero-return day on both. No mark-to-market: a position still open counts
/// nothing until it closes, on either side.
pub fn realized_daily_equity(
    capital: f64,
    trades: &[Trade],
    start_ms: i64,
    end_ms: i64,
) -> (Vec<f64>, Vec<i64>) {
    let mut closed: Vec<Trade> = trades
        .iter()
        .filter(|t| t.exit_timestamp >= start_ms && t.exit_timestamp <= end_ms)
        .cloned()
        .collect();
    closed.sort_by_key(|t| t.exit_timestamp);
    let (mut equity, mut ts) = crate::equity_curve_from_trades(capital, &closed);
    // The seed opens the window, not the first close, and the curve runs to the window's end.
    ts[0] = start_ms;
    equity.push(equity[equity.len() - 1]);
    ts.push(end_ms.max(start_ms));
    crate::equity::daily_equity_curve(&equity, &ts)
}

#[path = "forward_test_tests.rs"]
#[cfg(test)]
mod forward_test_tests;
