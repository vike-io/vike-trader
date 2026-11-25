//! The derivatives feed values: [`MarkPrice`], [`IndexPrice`], [`FundingRate`], [`OpenInterest`],
//! [`Liquidation`] and [`VolatilityIndex`] — what a venue PUBLISHES about a market beside its trades,
//! quotes and book — and [`FundingRatePoint`], one point of a funding-rate HISTORY.
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
    pub timestamp: i64,
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
    pub timestamp: i64,
    /// the venue's own index id
    pub name: String,
    /// The index level.
    pub price: f64,
}

/// One perpetual funding-rate observation.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FundingRate {
    /// epoch milliseconds (UTC)
    pub timestamp: i64,
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
    pub next_timestamp: Option<i64>,
    /// the venue's predicted rate for the next interval, same unit as `rate`
    #[serde(default)]
    pub predicted: Option<f64>,
    /// the price the venue charges funding on, when it is NOT the mark (Hyperliquid charges at its
    /// oracle price); `None` means the mark. Absent from the bytes while `None`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub price: Option<f64>,
}

/// One market funding-rate HISTORY point for a perp symbol: the interval timestamp and the rate
/// applied (decimal-string venue field decoded to `f64` — negative when shorts pay longs). Unlike the
/// live values above it carries no `symbol` (a series of them is keyed by its caller). `Copy` so a
/// pager can filter/collect points without cloning.
///
/// ⚠ **Its serde IS a wire**: the gatewayhub's funding fetch pages carry it as it is
/// (`crates/vike-gatewayhub-client/src/proto.rs`'s `FetchRows`), pinned by the committed
/// `fixtures/gatewayhub_wire/v1_responses.jsonl`. `premium` is always written (`null` when absent),
/// `mark` only when present, and a frame without either reads as `None`: a field renamed here is a
/// protocol break.
///
/// ⚠ **`timestamp_ms` is where the interval the rate pays for ENDS**, on every venue: a settlement venue's
/// funding time (binance, bybit, okx every 8h or the symbol's own 4h/1h; hyperliquid every hour), and
/// for deribit, whose funding accrues continuously, the end of the hour its `interest_1h` accrued over
/// (`crates/bridges/deribit/src/history.rs`'s `FundingRateSourceDeribit`). How a backtest applies each is
/// `crates/vike-backtest/src/run/dispatch/funding.rs`'s `FundingAccrual`.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct FundingRatePoint {
    /// Funding timestamp, ms since epoch (Binance `fundingTime` / Hyperliquid `time`).
    pub timestamp_ms: i64,
    /// The funding rate for this interval (`fundingRate`, a decimal STRING on the wire, `.parse`d).
    pub rate: f64,
    /// The venue's published funding PREMIUM for this interval, when it publishes one — signed, a
    /// fraction rather than a percentage, and NOT derivable from [`Self::rate`] (a venue folds an
    /// interest-rate term into the rate and then CLAMPS it, so the rate loses information the
    /// premium keeps).
    ///
    /// `None` for a venue whose funding-rate response carries no premium field: Binance's
    /// `/fapi/v1/fundingRate` rows are `{symbol, fundingTime, fundingRate, markPrice}` and have
    /// none, while Hyperliquid's `fundingHistory` rows are `{coin, fundingRate, premium, time}` and
    /// do. It is stored as `kind=perp_metrics` rather than on the funding [`Bar`](crate::Bar) — the
    /// funding rate's home — because `Bar` has no field for it and
    /// `vike_data::perp_metrics_log::PerpMetricRow` is where the argument for that split is made.
    #[serde(default)]
    pub premium: Option<f64>,
    /// The PRICE the venue publishes beside the rate, the one it values the payment at: binance's
    /// `markPrice` at the funding time, deribit's `index_price` at the end of the hour. `None` where the
    /// response carries none (bybit, okx and hyperliquid publish the rate alone) or the field will not
    /// parse to a positive number. Stored as the funding [`Bar`](crate::Bar)'s `close`
    /// (`crates/vike-backfill/src/funding_rate.rs`'s `point_to_bar`), where `0.0` meant and still means
    /// "no price". Absent on the wire when `None`, so a peer older than the field reads the frame
    /// unchanged and a newer one reads an older peer's frame as "no price".
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mark: Option<f64>,
}

/// One open-interest observation.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct OpenInterest {
    /// epoch milliseconds (UTC)
    pub timestamp: i64,
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
    pub timestamp: i64,
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
pub struct VolatilityIndex {
    /// epoch milliseconds (UTC)
    pub timestamp: i64,
    /// the venue's own index id (deribit's `"btc_usd"`)
    pub name: String,
    /// the index value as the venue publishes it (DVOL is annualised vol in PERCENT points)
    pub value: f64,
}

#[path = "feed_tests.rs"]
#[cfg(test)]
mod feed_tests;
