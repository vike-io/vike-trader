//! One bar's price transforms: the average, median and typical prices, the weighted close, and the
//! Heikin-Ashi candle law. The one copy the indicators (`vike-indicators`' price rows, Heikin-Ashi
//! core and every kernel that reads one of these prices) and the chart (its Heikin-Ashi style and its
//! `hl2`/`hlc3`/`ohlc4` study sources) call (decision 0294).
//!
//! Only `+`, `/`, `f64::max` and `f64::min`, in the operand order each doc states: the callers compare
//! these bit for bit (this crate's module doc, rule 3). A `NaN` price gives `NaN` through the sums,
//! while `max`/`min` take the other argument, so [`heikin_ashi_range`] skips a `NaN` side.

/// `(open + high + low + close) / 4`: the average price (`ohlc4`), and a Heikin-Ashi candle's close.
#[inline(always)]
pub fn average_price(open: f64, high: f64, low: f64, close: f64) -> f64 {
    (open + high + low + close) / 4.0
}

/// `(high + low) / 2`: the median price (`hl2`).
#[inline(always)]
pub fn median_price(high: f64, low: f64) -> f64 {
    (high + low) / 2.0
}

/// `(high + low + close) / 3`: the typical price (`hlc3`). A division, never a multiplication by
/// `1/3`, which differs in the last bit.
#[inline(always)]
pub fn typical_price(high: f64, low: f64, close: f64) -> f64 {
    (high + low + close) / 3.0
}

/// `(high + low + 2·close) / 4`: the weighted close.
#[inline(always)]
pub fn weighted_close(high: f64, low: f64, close: f64) -> f64 {
    (high + low + 2.0 * close) / 4.0
}

/// A Heikin-Ashi candle's open: `(previous open + previous close) / 2` of the previous CANDLE, or
/// `(open + close) / 2` of the bar itself for the first candle (`previous` is `None`). A rounding
/// caller passes its rounded open and close as `previous`.
#[inline(always)]
pub fn heikin_ashi_open(previous: Option<(f64, f64)>, open: f64, close: f64) -> f64 {
    match previous {
        Some((previous_open, previous_close)) => (previous_open + previous_close) / 2.0,
        None => (open + close) / 2.0,
    }
}

/// A Heikin-Ashi candle's `(high, low)`: the bar's `high` and `low` widened to take in the candle's
/// `open` and `close`, as `high.max(open).max(close)` and `low.min(open).min(close)`.
#[inline(always)]
pub fn heikin_ashi_range(high: f64, low: f64, open: f64, close: f64) -> (f64, f64) {
    (high.max(open).max(close), low.min(open).min(close))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_prices_of_one_bar() {
        assert_eq!(average_price(10.0, 14.0, 8.0, 12.0), 11.0);
        assert_eq!(median_price(14.0, 8.0), 11.0);
        assert_eq!(typical_price(14.0, 8.0, 12.0).to_bits(), (34.0f64 / 3.0).to_bits());
        assert_eq!(weighted_close(14.0, 8.0, 12.0), 11.5);
        assert!(median_price(f64::NAN, 8.0).is_nan());
    }

    /// The first candle opens on its own bar, a later one on the previous candle; the range takes in
    /// the open and close, and a `NaN` side is skipped by `max`/`min`.
    #[test]
    fn the_heikin_ashi_law() {
        assert_eq!(heikin_ashi_open(None, 10.0, 12.0), 11.0);
        assert_eq!(heikin_ashi_open(Some((11.0, 13.0)), 99.0, 99.0), 12.0);
        assert_eq!(heikin_ashi_range(13.0, 11.0, 12.0, 11.75), (13.0, 11.0));
        assert_eq!(heikin_ashi_range(12.0, 11.5, 12.5, 11.0), (12.5, 11.0));
        assert_eq!(heikin_ashi_range(f64::NAN, f64::NAN, 12.0, 11.0), (12.0, 11.0));
    }
}
