//! Buy/sell classification of a trade: which side the AGGRESSOR (the taker that crossed the
//! spread) was.
//!
//! [`TickTrade::is_buyer_maker`] is the Binance aggTrades convention: the buyer was the resting
//! maker (a bid), so the aggressor was the SELLER. [`classify`] turns that flag into a [`TickAggressor`] and
//! [`signed`] routes a trade's size to the buy or the sell total.
//
// Engineering notes (not rendered): it was born in `vike-orderflow` (where it is the load-bearing
// semantic of every builder), moved down to `vike-model`, and lives here beside the [`TickTrade`]
// it reads, so the vocabulary sits below every builder that routes by it. Ports no Python twin
// (new native).
use crate::TickTrade;

/// The aggressor side of a trade.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum TickAggressor {
    /// The buyer was the aggressor (a taker lifted the ask).
    Buy,
    /// The seller was the aggressor (a taker hit the bid).
    Sell,
}

/// The aggressor side of `t`: [`TickAggressor::Sell`] when `t.is_buyer_maker` is set, otherwise
/// [`TickAggressor::Buy`].
pub fn classify(t: &TickTrade) -> TickAggressor {
    if t.is_buyer_maker { TickAggressor::Sell } else { TickAggressor::Buy }
}

/// `(buy_add, sell_add)` for the full `size` of `t`, routed to the aggressor side: the size lands
/// in the first slot for a buy aggressor and in the second for a sell aggressor, the other is `0.0`.
pub fn signed(t: &TickTrade) -> (f64, f64) {
    match classify(t) {
        TickAggressor::Buy => (t.size, 0.0),
        TickAggressor::Sell => (0.0, t.size),
    }
}

#[path = "aggressor_tests.rs"]
#[cfg(test)]
mod aggressor_tests;
