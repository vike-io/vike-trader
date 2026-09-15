//! Buy/sell classification — the one load-bearing semantic of the whole crate.
//! `is_buyer_maker` is the Binance-aggTrades convention: the buyer was the resting
//! maker (a bid), so the aggressor was the SELLER. Ports no Python twin (new native).
use vike_model::TradeTick;

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

#[cfg(test)]
mod tests {
    use super::*;
    fn t(is_buyer_maker: bool) -> TradeTick {
        TradeTick {
            ts: 0,
            local_ts: 0,
            price: 100.0,
            size: 2.0,
            is_buyer_maker,
            symbol: String::new(),
        }
    }
    #[test]
    fn classify_maps_is_buyer_maker() {
        assert_eq!(classify(&t(false)), Side::Buy); // ask lifted → buy aggressor
        assert_eq!(classify(&t(true)), Side::Sell); // bid hit → sell aggressor
    }
    #[test]
    fn signed_splits_by_side() {
        assert_eq!(signed(&t(false)), (2.0, 0.0));
        assert_eq!(signed(&t(true)), (0.0, 2.0));
    }
}
