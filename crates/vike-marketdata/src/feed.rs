//! The derivatives feed values: [`MarkPrice`], [`IndexPrice`], [`FundingRate`], [`OpenInterest`],
//! [`Liquidation`] and [`VolIndexTick`] — what a venue PUBLISHES about a market beside its trades,
//! quotes and book.
//!
//! The shapes are venue-free: whichever venue produced a row, a consumer reads the same fields.
//! They follow [`crate::Bar`]'s conventions: `ts` is epoch milliseconds (UTC) of the venue's
//! publish time, `symbol` is the fully-qualified `"SYMBOL.VENUE"` instrument id, and every
//! optional field carries `#[serde(default)]`, so a payload written before a field existed still
//! deserialises. An absent value stays `None`, never a fabricated `0.0`.
//
// Engineering notes (not rendered): VENUE-FREE by construction: a bridge decodes its own wire row
// and emits one of these (`docs/decisions/0126-every-instrument-class-is-a-typed-contract-in-vike-model-and-feed-values-live-in-vike-marketdata.md`).
// The `symbol` id is the one the engine dispatch uses, and `#[serde(default)]` is the additive-serde
// contract the journal and recorded streams ride on.

use serde::{Deserialize, Serialize};

/// One mark-price observation: the venue's fair price used for margin and unrealised PnL.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MarkPrice {
    /// epoch milliseconds (UTC)
    pub ts: i64,
    /// fully-qualified "SYMBOL.VENUE" id
    pub symbol: String,
    /// the mark price, in the instrument's quote (premium) unit
    pub mark: f64,
    /// the index (spot reference) price the venue published beside the mark, if it did
    #[serde(default)]
    pub index: Option<f64>,
}

/// One index-price observation: a venue's published reference index, which is not itself a
/// tradeable instrument (e.g. deribit's `"btc_usd"`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct IndexPrice {
    /// epoch milliseconds (UTC)
    pub ts: i64,
    /// the venue's own index id
    pub name: String,
    /// The index level.
    pub price: f64,
}

/// One perpetual funding-rate observation.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FundingRate {
    /// epoch milliseconds (UTC)
    pub ts: i64,
    /// fully-qualified "SYMBOL.VENUE" id
    pub symbol: String,
    /// the rate per funding interval, as a DECIMAL fraction (`0.0001` == 0.01%); positive means
    /// longs pay shorts
    pub rate: f64,
    /// the funding interval in milliseconds (8 h = 28 800 000), when the venue states it
    #[serde(default)]
    pub interval_ms: Option<u64>,
    /// epoch milliseconds (UTC) of the next funding settlement, when the venue states it
    #[serde(default)]
    pub next_ts: Option<i64>,
    /// the venue's predicted rate for the next interval, same unit as `rate`
    #[serde(default)]
    pub predicted: Option<f64>,
}

/// One open-interest observation.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct OpenInterest {
    /// epoch milliseconds (UTC)
    pub ts: i64,
    /// fully-qualified "SYMBOL.VENUE" id
    pub symbol: String,
    /// open interest in CONTRACTS (base units for a venue whose contract is one unit of the base)
    pub contracts: f64,
    /// the same open interest valued in the quote currency, when the venue publishes it
    #[serde(default)]
    pub notional: Option<f64>,
}

/// One forced (liquidation) trade a venue published.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Liquidation {
    /// epoch milliseconds (UTC)
    pub ts: i64,
    /// fully-qualified "SYMBOL.VENUE" id
    pub symbol: String,
    /// the side of the LIQUIDATING ORDER: `+1` buy (a short was closed out), `-1` sell (a long was
    /// closed out). A venue that reports the liquidated POSITION's side instead is flipped by
    /// whoever fills this struct, so the field means one thing whichever venue produced it.
    pub side: i32,
    /// Execution price of the forced trade.
    pub price: f64,
    /// executed quantity, in contracts
    pub qty: f64,
}

/// One volatility-index observation (a DVOL-style implied-volatility index).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct VolIndexTick {
    /// epoch milliseconds (UTC)
    pub ts: i64,
    /// the venue's own index id (deribit's `"btc_usd"`)
    pub name: String,
    /// the index value as the venue publishes it (DVOL is annualised vol in PERCENT points)
    pub value: f64,
}

#[path = "feed_tests.rs"]
#[cfg(test)]
mod feed_tests;
