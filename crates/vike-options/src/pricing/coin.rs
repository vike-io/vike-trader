//! Coin-settled (inverse) options: the price and greeks in COIN, for a book whose premium is paid
//! and whose P&L is counted in the underlying's coin (Deribit's BTC and ETH options: one contract
//! is one coin, the strike is in USD, the premium is quoted in coin). NEW code, no Python twin, no
//! parity fixture: hand-worked numbers and finite differences check it (`pricing/coin/tests.rs`).
//!
//! What the rest of the crate does with such an option, checked 2026-11-17: it prices EVERYTHING
//! in USD. [`black_scholes_price`] and [`black_scholes_greeks`] take the USD index price and the
//! USD strike, so their price is USD per coin of underlying and their delta is coins; the chain
//! folds turn a coin mark into USD by the chain's price (`crate::chain::fold`, decided by
//! [`premium_is_usd`](crate::premium_is_usd)), and the greeks a USD-margined holder hedges are those.
//! A COIN-margined holder needs the same option seen in coin, which nothing here gave before:
//!
//! With `V` the USD price, `C = V / S` is the coin price, and every greek is of `C`:
//!
//! - `price = V / S` (coin);
//! - `delta = S · ∂C/∂S = Δ − C`: the PREMIUM-ADJUSTED delta, in coins, the hedge a coin-margined
//!   book is sized by. Closed form at `q = 0`: `(K/S)·e^{−rt}·N(d2)` for a call,
//!   `−(K/S)·e^{−rt}·N(−d2)` for a put (the foreign-currency premium delta of FX options);
//! - `gamma = ∂delta/∂S = Γ − delta / S` (coins per 1.0 of price);
//! - `theta = Θ / S` (coin per calendar day) and `vega = ν / S` (coin per vol-point): time and
//!   volatility do not move `S`, so they only change unit.
//!
//! `S` is the underlying's USD price (Deribit prices off the expiry's forward with `r = 0`: pass
//! the forward and `r = 0.0` to match it). A linear (USD-settled) option needs none of this: its
//! premium is USD and [`black_scholes_greeks`] is already its answer.

use vike_marketdata::OptionKind;

use crate::pricing::greeks::{black_scholes_greeks, black_scholes_price};

/// A coin-settled option's price and greeks in coin (see the module doc for each formula).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct CoinSettledGreeks {
    /// The premium in coin: the USD price over the underlying's price.
    pub price: f64,
    /// The premium-adjusted delta, in coins: the Black–Scholes delta minus the coin price.
    pub delta: f64,
    /// The premium-adjusted delta's change per 1.0 of the underlying's price.
    pub gamma: f64,
    /// The coin price's change per calendar day.
    pub theta: f64,
    /// The coin price's change per vol-point (0.01 of volatility).
    pub vega: f64,
}

/// The price and greeks in coin of ONE coin-settled option on an underlying at USD price `s`, strike
/// `k` (USD), `t` years, volatility `sigma`, rate `r`. `None` on the inputs [`black_scholes_greeks`]
/// refuses (`s/k/t/sigma <= 0`).
pub fn coin_settled_greeks(
    s: f64,
    k: f64,
    t: f64,
    sigma: f64,
    kind: OptionKind,
    r: f64,
) -> Option<CoinSettledGreeks> {
    let price = black_scholes_price(s, k, t, sigma, kind, r)? / s;
    let (delta, gamma, theta, vega) = black_scholes_greeks(s, k, t, sigma, kind, r)?;
    let delta = delta - price;
    Some(CoinSettledGreeks {
        price,
        delta,
        gamma: gamma - delta / s,
        theta: theta / s,
        vega: vega / s,
    })
}

#[cfg(test)]
mod tests;
