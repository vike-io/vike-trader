//! Option FEATURES: the numbers a model reads off one quote of a chain snapshot, written once so a
//! research study, a strategy file and the book's listings compute them the same way. NEW code, no
//! Python twin (the oracle stops at first-order greeks), so no parity fixture governs it; each
//! function is one line of the crate's existing maths:
//!
//! - [`moneyness`] `K / S` and [`log_moneyness`] `ln(K / S)` (`libm::log`, ADR 0032). Pass the
//!   forward for `S` to get the forward log-moneyness an SVI slice is written in (`crate::svi`).
//! - Years to expiry from millisecond instants: [`years_to_expiry_ms`](crate::years_to_expiry_ms),
//!   the ONE division by [`MS_PER_YEAR`](crate::MS_PER_YEAR) (the 365-day year the greeks and their
//!   theta count in).
//! - [`quote_iv`]: a quote's own implied volatility, else the one its mark implies
//!   ([`implied_vol`]); [`delta_at_quote`]: the Black–Scholes delta at that IV.
//! - [`atm_iv_by_expiry`]: the at-the-money IV of each expiry of one snapshot, by the SAME rule as
//!   [`atm_iv`](crate::atm_iv) (one point per strike, the point nearest the forward:
//!   [`smile_points`] and [`atm_iv_of_smile`]), for callers holding flat quote rows rather than an
//!   `OptionChain` (a stored chain's rows, `vike_data::ChainRow`, which this crate cannot name).
//!
//! Everything is pure `f64`, deterministic, and takes `r` as a plain parameter.

use std::collections::BTreeMap;

use vike_marketdata::OptionKind;
use vike_math::time_series::log_ratio;

use crate::pricing::greeks::{black_scholes_greeks, implied_vol, years_to_expiry_ms};
use crate::vol::{atm_iv_of_smile, smile_points};

/// Moneyness `K / S`: the strike over the underlying's price (or its forward).
pub fn moneyness(strike: f64, underlying_price: f64) -> f64 {
    strike / underlying_price
}

/// Log-moneyness `ln(K / S)`: negative in the money for a call, zero at the money. Pass the forward
/// for `underlying_price` to get the `k` an SVI slice is written in. `NaN` unless both prices are
/// positive.
pub fn log_moneyness(strike: f64, underlying_price: f64) -> f64 {
    // `libm::log` (inside `log_ratio`), never `f64::ln` (ADR 0032): a feature a model trains on must be the same bits on
    // the desktop and on the lane that trained it.
    log_ratio(strike, underlying_price)
}

/// A quote's implied volatility: its own when finite and positive, else the one its `mark` implies
/// at `underlying_price` and `years` ([`implied_vol`]); `None` when neither exists.
pub fn quote_iv(
    iv: Option<f64>,
    mark: Option<f64>,
    underlying_price: f64,
    strike: f64,
    years: f64,
    right: OptionKind,
    r: f64,
) -> Option<f64> {
    iv.filter(|iv| iv.is_finite() && *iv > 0.0)
        .or_else(|| implied_vol(mark?, underlying_price, strike, years, right, r))
}

/// A quote's Black–Scholes delta at its own IV ([`quote_iv`], same arguments): `None` when the quote
/// has no IV or the greeks cannot be computed (an expired quote, a mark outside the model's).
pub fn delta_at_quote(
    iv: Option<f64>,
    mark: Option<f64>,
    underlying_price: f64,
    strike: f64,
    years: f64,
    right: OptionKind,
    r: f64,
) -> Option<f64> {
    let iv = quote_iv(iv, mark, underlying_price, strike, years, right, r)?;
    black_scholes_greeks(underlying_price, strike, years, iv, right, r).map(|(delta, ..)| delta)
}

/// The at-the-money IV of each expiry of one snapshot taken at `now_ms`, keyed by expiry: the
/// `quotes` are `(expiry_ms, strike, iv)` (each side of a strike its own quote), each expiry's
/// smile folded by [`smile_points`] and read at the forward of `underlying_price` by
/// [`atm_iv_of_smile`] with its own years to expiry (clamped at 0, so pass only the live
/// expiries). With a price that is not positive every read is `None` and the map is empty.
pub fn atm_iv_by_expiry(
    quotes: &[(i64, f64, f64)],
    underlying_price: f64,
    now_ms: i64,
    r: f64,
) -> BTreeMap<i64, f64> {
    let mut by_expiry: BTreeMap<i64, Vec<(f64, f64)>> = BTreeMap::new();
    for &(expiry_ms, strike, iv) in quotes {
        by_expiry.entry(expiry_ms).or_default().push((strike, iv));
    }
    by_expiry
        .into_iter()
        .filter_map(|(expiry_ms, smile)| {
            let years = years_to_expiry_ms(expiry_ms, now_ms);
            atm_iv_of_smile(&smile_points(smile), underlying_price, years, r)
                .map(|iv| (expiry_ms, iv))
        })
        .collect()
}

#[cfg(test)]
mod tests;
