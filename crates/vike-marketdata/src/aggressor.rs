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

/// The TICK RULE: the aggressor side of a trade estimated from its PRICE, for a tape whose trades
/// carry no aggressor flag. A trade above `previous_price` was a buy, one below it a sell, and one at
/// the same price takes `previous`, the side of the trade before it. Start a tape with `previous`
/// [`TickAggressor::Buy`] and a `NaN` `previous_price`: López de Prado sets the first side to a buy
/// "arbitrarily" (`b_0 = 1`, "Advances in Financial Machine Learning", 2018, chapter 19, "The Tick
/// Rule"). A `NaN` price compares neither way and keeps `previous`.
///
/// It ESTIMATES what [`classify`] reads off a feed that carries `is_buyer_maker`. To fold a flagless
/// tape through a builder that routes by [`classify`], set each trade's flag from it first:
/// `is_buyer_maker = tick_rule(..) == TickAggressor::Sell`.
pub fn tick_rule(price: f64, previous_price: f64, previous: TickAggressor) -> TickAggressor {
    if price > previous_price {
        TickAggressor::Buy
    } else if price < previous_price {
        TickAggressor::Sell
    } else {
        previous
    }
}

#[path = "aggressor_tests.rs"]
#[cfg(test)]
mod aggressor_tests;
