//! Buy/sell classification of a trade — which side the AGGRESSOR was. It was born in
//! `vike-orderflow` (where it is the load-bearing semantic of every builder), moved down to
//! `vike-model`, and lives here beside the [`TradeTick`] it reads, so the vocabulary sits below
//! every builder that routes by it. `is_buyer_maker` is the Binance-aggTrades convention: the
//! buyer was the resting maker (a bid), so the aggressor was the SELLER. Ports no Python twin
//! (new native).
use crate::TradeTick;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Side {
    Buy,
    Sell,
}

pub fn classify(t: &TradeTick) -> Side {
    if t.is_buyer_maker { Side::Sell } else { Side::Buy }
}

/// (buy_add, sell_add) for the full `size` of `t`, routed to the aggressor side.
pub fn signed(t: &TradeTick) -> (f64, f64) {
    match classify(t) {
        Side::Buy => (t.size, 0.0),
        Side::Sell => (0.0, t.size),
    }
}

#[path = "aggressor_tests.rs"]
#[cfg(test)]
mod aggressor_tests;
