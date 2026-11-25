//! Position greeks — ONE summing core for every holder of option positions: a position's greeks are
//! the per-unit Black–Scholes greeks times its SIGNED quantity ([`PositionGreeks::of_option`]), a
//! net is their naive left fold in position order ([`PositionGreeks::add`]), and a book across
//! underlyings sums its delta per underlying in underlying units, then in USD at each underlying's
//! price ([`PortfolioGreeks`] → [`PortfolioNetGreeks`]). [`crate::Basket::net_greeks`] and the
//! desktop's Greeks tool (`vike-app-core`'s `orders::option_chain_position_greeks`, which keeps only the chain
//! and index lookups and the display) both call it, so a basket and a held book cannot disagree on
//! what a net greek is.
//!
//! Units follow the per-unit sources exactly: `delta` per 1.0 of underlying, `gamma` per 1.0²,
//! `theta` **per calendar day**, `vega` **per vol-point** (`black_scholes_greeks`' /100 scaling);
//! `vanna`/`vomma` are RAW per-1.0-sigma and `charm`/`veta`/`color` are per calendar day
//! (`second_order_greeks`' conventions). Nothing here rescales them.
//!
//! **Why a book's delta is summed per underlying, then in USD.** A delta is per 1.0 of ITS
//! underlying, so a 1-delta BTC option and a 1-delta ETH option are wildly different dollar
//! exposures and their deltas cannot be added. [`PortfolioGreeks::net_usd`] multiplies each
//! underlying's net delta by that underlying's price: on a coin-settled (inverse) book such as
//! Deribit's, where one contract is one coin and the premium is quoted in coin, the greeks are
//! priced from the USD index price and the USD strike, so a delta is in coins and `coins × price`
//! is its USD exposure. A LINEAR leg (a perpetual or a dated future) has delta only, and the holder
//! passes it in coins ([`PortfolioGreeks::add_delta_one`]: on Deribit the venue's own position delta,
//! because such a leg's size is USD notional, not coins). Vega and theta are already in the
//! premium's currency, priced from a USD price, so they need no conversion.
//!
//! Summation is a naive left fold in position order everywhere, the same order a caller adds in,
//! so a net is bit-identical to adding the per-position numbers by hand in that order. Pure `f64`,
//! deterministic, no I/O.

use std::collections::BTreeMap;

use vike_marketdata::OptionKind;

use crate::pricing::greeks::black_scholes_greeks;
use crate::pricing::second_order::second_order_greeks;

/// The signed, quantity-weighted greeks of one position, or the sum of several (see the module doc
/// for the units).
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct PositionGreeks {
    /// Σ quantity·delta.
    pub delta: f64,
    /// Σ quantity·gamma.
    pub gamma: f64,
    /// Σ quantity·theta (per calendar day).
    pub theta: f64,
    /// Σ quantity·vega (per vol-point).
    pub vega: f64,
    /// Σ quantity·vanna (raw, per 1.0 sigma).
    pub vanna: f64,
    /// Σ quantity·vomma (raw, per 1.0 sigma).
    pub vomma: f64,
    /// Σ quantity·charm (per calendar day).
    pub charm: f64,
    /// Σ quantity·veta (per calendar day).
    pub veta: f64,
    /// Σ quantity·color (per calendar day).
    pub color: f64,
}

impl PositionGreeks {
    /// The greeks of `quantity` units (signed: long > 0, short < 0) of one option, every per-unit
    /// greek ([`black_scholes_greeks`], [`second_order_greeks`]) times `quantity`. `None` when the
    /// greeks are not computable (the `spot/strike/years/iv <= 0` guard both share).
    pub fn of_option(
        quantity: f64,
        spot: f64,
        strike: f64,
        years: f64,
        iv: f64,
        right: OptionKind,
        r: f64,
    ) -> Option<Self> {
        let (delta, gamma, theta, vega) = black_scholes_greeks(spot, strike, years, iv, right, r)?;
        let second = second_order_greeks(spot, strike, years, iv, right, r)?;
        Some(Self {
            delta: quantity * delta,
            gamma: quantity * gamma,
            theta: quantity * theta,
            vega: quantity * vega,
            vanna: quantity * second.vanna,
            vomma: quantity * second.vomma,
            charm: quantity * second.charm,
            veta: quantity * second.veta,
            color: quantity * second.color,
        })
    }

    /// Adds `other` greek by greek: one step of the naive left fold every net here is.
    pub fn add(&mut self, other: &Self) {
        self.delta += other.delta;
        self.gamma += other.gamma;
        self.theta += other.theta;
        self.vega += other.vega;
        self.vanna += other.vanna;
        self.vomma += other.vomma;
        self.charm += other.charm;
        self.veta += other.veta;
        self.color += other.color;
    }
}

/// A book of positions across underlyings as it accumulates, position by position: the raw sum of
/// its OPTION positions' greeks, and each underlying's net delta in underlying (coin) units, option
/// and linear legs together. [`PortfolioGreeks::net_usd`] turns it into the USD read.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct PortfolioGreeks {
    /// Σ over the option positions added, naive per unit: meaningful within ONE underlying only (a
    /// delta is per 1.0 of its own underlying; see the module doc).
    pub options: PositionGreeks,
    /// Per underlying, its net delta in underlying units: every option position's delta plus every
    /// linear leg's coin delta, in the order they were added.
    pub delta_units: BTreeMap<String, f64>,
}

impl PortfolioGreeks {
    /// Adds an option position on `underlying` ([`PositionGreeks::of_option`]): its greeks join the
    /// option sum, its delta the underlying's.
    pub fn add_option(&mut self, underlying: &str, greeks: &PositionGreeks) {
        self.options.add(greeks);
        *self.delta_units.entry(underlying.to_string()).or_insert(0.0) += greeks.delta;
    }

    /// Adds a LINEAR leg (a perpetual or a dated future) on `underlying` by its delta in coins: delta
    /// only, so it moves the underlying's delta and none of the option sum.
    pub fn add_delta_one(&mut self, underlying: &str, coin_delta: f64) {
        *self.delta_units.entry(underlying.to_string()).or_insert(0.0) += coin_delta;
    }

    /// The USD read: each underlying's delta at `price_of(underlying)` (an underlying it answers
    /// `None` for is left out), their total, and the option sum's vega and theta (already in USD).
    /// `unpriced` is the holder's list of positions it could not price, carried so a reader of the
    /// net knows it is incomplete.
    pub fn net_usd(
        &self,
        price_of: impl Fn(&str) -> Option<f64>,
        unpriced: Vec<String>,
    ) -> PortfolioNetGreeks {
        let mut net = PortfolioNetGreeks {
            net_vega: self.options.vega,
            net_theta: self.options.theta,
            unpriced,
            ..Default::default()
        };
        for (underlying, &delta_units) in &self.delta_units {
            let Some(spot) = price_of(underlying) else { continue };
            let delta_usd = delta_units * spot;
            net.net_delta_usd += delta_usd;
            net.per_underlying
                .insert(underlying.clone(), UnderlyingDelta { delta_units, delta_usd, spot });
        }
        net
    }
}

/// Net delta for ONE underlying, in underlying units and in USD: the per-underlying half of
/// [`PortfolioNetGreeks`]. A delta-hedging executor sizes that underlying's hedge from
/// [`delta_units`](Self::delta_units) and compares books across underlyings by
/// [`delta_usd`](Self::delta_usd).
#[derive(Debug, Clone, PartialEq, Default)]
pub struct UnderlyingDelta {
    /// Net delta in UNDERLYING (coin) units: Σ over this underlying's positions of, for an option
    /// `quantity × delta`, and for a linear leg its coin delta.
    pub delta_units: f64,
    /// The same net delta in USD (`delta_units × spot`): comparable across underlyings, unlike the
    /// per-unit deltas.
    pub delta_usd: f64,
    /// The underlying's price the USD conversion used.
    pub spot: f64,
}

/// A book's net greeks in USD: the dimensionally correct aggregate across underlyings (the raw
/// [`PortfolioGreeks::options`] sum adds deltas of different underlyings, which means nothing).
///
/// Linear legs move [`net_delta_usd`](Self::net_delta_usd) and their underlying's
/// [`per_underlying`](Self::per_underlying) delta and nothing else (a linear leg has no vega or
/// theta). A position the holder could not price is listed in [`unpriced`](Self::unpriced), never
/// folded as a guess.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct PortfolioNetGreeks {
    /// Per-underlying net delta (units, USD, price), keyed by underlying.
    pub per_underlying: BTreeMap<String, UnderlyingDelta>,
    /// Cross-underlying USD delta: Σ of `per_underlying[*].delta_usd`, in underlying order.
    pub net_delta_usd: f64,
    /// Vega total (per vol-point, in the premium's currency, USD here), over the option positions.
    pub net_vega: f64,
    /// Theta total (per day, USD), over the option positions.
    pub net_theta: f64,
    /// The positions the holder could not price, as it names them, so the net reads as INCOMPLETE.
    pub unpriced: Vec<String>,
}

#[cfg(test)]
mod tests;
