//! [`OptionTicker`]: the per-option LIVE row — one contract's quote, mark, implied vols and
//! greeks at one instant, as an option venue's ticker channel publishes them.
//!
//! Its field set covers what deribit's `ticker.<instrument>.<interval>` channel sends (best
//! bid/ask with sizes, last, mark, `mark_iv`/`bid_iv`/`ask_iv`, the five greeks, open interest,
//! 24 h volume, index and underlying price) and what its `markprice.options.<index>` channel sends
//! (mark and implied vol), so a bridge fills one shape from either.
//!
//! UNITS are fixed here so a consumer never asks which venue a row came from:
//! - implied vols are DECIMAL fractions (`0.62` == 62%); a venue that publishes percent (deribit's
//!   ticker `mark_iv`) is divided by 100 in its bridge;
//! - prices (`bid`, `ask`, `last`, `mark`) are in the contract's PREMIUM unit — coin units on an
//!   inverse venue such as deribit, the quote currency on a linear one; scaling to USD is the
//!   consumer's, by the underlying price it trusts;
//! - greeks are per contract as the venue publishes them: `theta` per calendar day, `vega` and
//!   `rho` per one-percentage-point change in vol / rate. Their CURRENCY is the venue's as well and
//!   need not be the premium unit: deribit's `vega`/`theta`/`rho` are USD while its premiums are
//!   coin.
//!
//! Every value field is `Option` with `#[serde(default)]`: a ticker frame routinely omits a side
//! of the book or the greeks, and an absent value stays `None`, never a fabricated `0.0`.

use serde::{Deserialize, Serialize};

/// One option contract's live ticker row.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct OptionTicker {
    /// epoch milliseconds (UTC) of the venue's publish
    pub ts: i64,
    /// the option contract's id exactly as the feed that produced it names it: the venue's own name
    /// (e.g. `BTC-31JUL26-67000-C`, what the deribit option streams send and what `vike-options`'
    /// `OptionIndex` keys on), or a fully-qualified `SYMBOL.VENUE` id where the engine attached one
    pub symbol: String,
    /// best bid, premium unit
    #[serde(default)]
    pub bid: Option<f64>,
    /// best ask, premium unit
    #[serde(default)]
    pub ask: Option<f64>,
    /// size at the best bid, contracts
    #[serde(default)]
    pub bid_size: Option<f64>,
    /// size at the best ask, contracts
    #[serde(default)]
    pub ask_size: Option<f64>,
    /// last traded price, premium unit
    #[serde(default)]
    pub last: Option<f64>,
    /// the venue's mark price, premium unit
    #[serde(default)]
    pub mark: Option<f64>,
    /// implied vol at the mark, decimal fraction
    #[serde(default)]
    pub mark_iv: Option<f64>,
    /// implied vol at the best bid, decimal fraction
    #[serde(default)]
    pub bid_iv: Option<f64>,
    /// implied vol at the best ask, decimal fraction
    #[serde(default)]
    pub ask_iv: Option<f64>,
    #[serde(default)]
    pub delta: Option<f64>,
    #[serde(default)]
    pub gamma: Option<f64>,
    /// per one-percentage-point change in implied vol
    #[serde(default)]
    pub vega: Option<f64>,
    /// per calendar day
    #[serde(default)]
    pub theta: Option<f64>,
    /// per one-percentage-point change in the interest rate
    #[serde(default)]
    pub rho: Option<f64>,
    /// open interest, contracts
    #[serde(default)]
    pub open_interest: Option<f64>,
    /// trailing 24 h volume, contracts
    #[serde(default)]
    pub volume: Option<f64>,
    /// the price of the instrument the option is written on (a future's price, for a venue that
    /// prices options off the same-expiry future)
    #[serde(default)]
    pub underlying_price: Option<f64>,
    /// the venue's spot reference index at publish
    #[serde(default)]
    pub index_price: Option<f64>,
}

impl OptionTicker {
    /// A row with every value field absent — a bridge sets what its frame carried.
    pub fn new(ts: i64, symbol: impl Into<String>) -> Self {
        Self {
            ts,
            symbol: symbol.into(),
            bid: None,
            ask: None,
            bid_size: None,
            ask_size: None,
            last: None,
            mark: None,
            mark_iv: None,
            bid_iv: None,
            ask_iv: None,
            delta: None,
            gamma: None,
            vega: None,
            theta: None,
            rho: None,
            open_interest: None,
            volume: None,
            underlying_price: None,
            index_price: None,
        }
    }
}

#[path = "ticker_tests.rs"]
#[cfg(test)]
mod ticker_tests;
