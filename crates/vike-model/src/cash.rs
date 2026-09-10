//! RateBook — currency-conversion rates for valuing per-asset balances in one account
//! currency. Rust-native surface (NO Python twin): the oracle's Account keeps a single
//! collapsed scalar; this book exists so the additive per-asset ledger (accounting-upgrade
//! design, decision A2) can be valued. Semantics follow the LEAN twin:
//! `Common/Securities/CashBook.cs::Convert` (rate ratio formula; a dead/absent rate is the
//! caller's problem — here `None`, never a silent fallback) and
//! `SecurityCurrencyConversion::LinearSearch` (direct pair, inverted pair, then first 2-hop
//! path in registration order).
//!
//! Prices enter via `set(base, quote, px)` from whatever series the app already ingests
//! (mark ticks, bar closes of conversion pairs) — the book never subscribes to anything.

use indexmap::IndexMap;

/// Last-known prices of conversion pairs, keyed `(base, quote)` in registration order.
#[derive(Debug, Clone, Default)]
pub struct RateBook {
    rates: IndexMap<(String, String), f64>,
}

impl RateBook {
    pub fn new() -> Self {
        Self::default()
    }

    /// Record the latest price of one pair: 1 `base` = `px` `quote`. Non-positive prices
    /// are stored as-is but treated as a dead leg by every read (LEAN: a zero `GetLastData`
    /// price zeroes the whole conversion).
    pub fn set(&mut self, base: &str, quote: &str, px: f64) {
        self.rates.insert((base.to_string(), quote.to_string()), px);
    }

    /// 1-leg rate: direct pair, else inverted pair (`1/px`). `None` when neither pair is
    /// known or the stored price is not positive.
    pub fn rate(&self, base: &str, quote: &str) -> Option<f64> {
        if let Some(&px) = self.rates.get(&(base.to_string(), quote.to_string())) {
            return (px > 0.0).then_some(px);
        }
        if let Some(&px) = self.rates.get(&(quote.to_string(), base.to_string())) {
            return (px > 0.0).then_some(1.0 / px);
        }
        None
    }

    /// Conversion rate `src` -> `dst`: identity, 1-leg, then the FIRST 2-hop path in pair
    /// registration order (LEAN `LinearSearch`: existing/earlier securities win).
    pub fn rate_to(&self, src: &str, dst: &str) -> Option<f64> {
        if src == dst {
            return Some(1.0);
        }
        if let Some(r) = self.rate(src, dst) {
            return Some(r);
        }
        for (base, quote) in self.rates.keys() {
            let middle = if base == src {
                quote
            } else if quote == src {
                base
            } else {
                continue;
            };
            if let (Some(leg1), Some(leg2)) = (self.rate(src, middle), self.rate(middle, dst)) {
                return Some(leg1 * leg2);
            }
        }
        None
    }

    /// LEAN `CashBook.Convert`: `qty` of `src` expressed in `dst`. Zero quantity converts to
    /// zero without touching rates; an unpriced path is `None` — the caller decides whether
    /// that is an error, a skip, or a deferral.
    pub fn convert(&self, qty: f64, src: &str, dst: &str) -> Option<f64> {
        if qty == 0.0 {
            return Some(0.0);
        }
        self.rate_to(src, dst).map(|r| qty * r)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identity_is_one() {
        let rb = RateBook::new();
        assert_eq!(rb.rate_to("USDT", "USDT"), Some(1.0));
        assert_eq!(rb.convert(5.0, "USD", "USD"), Some(5.0));
    }

    #[test]
    fn direct_pair() {
        let mut rb = RateBook::new();
        rb.set("BTC", "USDT", 50_000.0);
        assert_eq!(rb.convert(2.0, "BTC", "USDT"), Some(100_000.0));
    }

    #[test]
    fn inverted_pair() {
        let mut rb = RateBook::new();
        rb.set("BTC", "USDT", 50_000.0);
        assert_eq!(rb.rate_to("USDT", "BTC"), Some(1.0 / 50_000.0));
    }

    #[test]
    fn two_hop_path() {
        // EUR -> USD via EURGBP then GBPUSD (LEAN LinearSearch 2-hop shape)
        let mut rb = RateBook::new();
        rb.set("EUR", "GBP", 0.85);
        rb.set("GBP", "USD", 1.25);
        assert_eq!(rb.rate_to("EUR", "USD"), Some(0.85 * 1.25));
        // inverted legs work too: USD -> EUR
        let r = rb.rate_to("USD", "EUR").unwrap();
        assert!((r - 1.0 / (0.85 * 1.25)).abs() < 1e-15);
    }

    #[test]
    fn first_registered_middle_wins() {
        let mut rb = RateBook::new();
        rb.set("EUR", "GBP", 0.85); // registered first -> its path wins
        rb.set("GBP", "USD", 1.25);
        rb.set("EUR", "JPY", 160.0);
        rb.set("JPY", "USD", 0.007);
        assert_eq!(rb.rate_to("EUR", "USD"), Some(0.85 * 1.25));
    }

    #[test]
    fn dead_leg_is_none() {
        let mut rb = RateBook::new();
        rb.set("BTC", "USDT", 0.0); // dead: no data yet
        assert_eq!(rb.rate_to("BTC", "USDT"), None);
        assert_eq!(rb.convert(1.0, "BTC", "USDT"), None);
    }

    #[test]
    fn unknown_pair_is_none() {
        let rb = RateBook::new();
        assert_eq!(rb.convert(1.0, "SOL", "USDT"), None);
    }

    #[test]
    fn zero_qty_never_touches_rates() {
        let rb = RateBook::new(); // empty book
        assert_eq!(rb.convert(0.0, "SOL", "USDT"), Some(0.0));
    }
}
