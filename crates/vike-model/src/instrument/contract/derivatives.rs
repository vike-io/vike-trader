//! The defining data of the derivative classes of [`super::Contract`]: the perpetual
//! ([`PerpSpec`]), the dated future ([`FutureSpec`]), the listed option ([`OptionSpec`]) and the
//! venue-listed combo ([`ComboContract`]).
//!
//! Each carries its [`Margining`] and its settlement asset as typed fields read from the payload;
//! `value_ccy` is the currency one contract's VALUE is denominated in (okx's `ctValCcy`, a COIN-M
//! contract's `USD`), which is what makes a contract multiplier mean anything. Times are epoch ms
//! UTC.

use super::Margining;
use serde::{Deserialize, Serialize};
use vike_marketdata::OptionKind;

/// What defines a perpetual swap. ⚠ No expiry field, by construction.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PerpSpec {
    pub margining: Margining,
    /// The asset PnL and margin settle in.
    pub settle: String,
    /// The currency one contract's value is denominated in, where the venue says.
    pub value_ccy: Option<String>,
    /// How often funding is exchanged, in ms (bybit's `fundingInterval`).
    pub funding_interval_ms: Option<u64>,
    /// When a perp that is being delisted settles, epoch ms UTC (binance `PERPETUAL_DELIVERING`).
    /// `None` for an ordinary perp. This is a delisting instant, not an expiry.
    pub settling_ms: Option<i64>,
}

/// The calendar cycle a dated future belongs to (deribit's `settlement_period`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Tenor {
    Day,
    Week,
    Month,
    Quarter,
    Other,
}

/// How a future settles at expiry.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Delivery {
    Cash,
    Physical,
    Unknown,
}

/// Which family of futures a dated future belongs to — the axis [`super::Contract::class`] splits
/// a future on, because the stored taxonomy keeps two words for it: a crypto-venue future
/// (`AssetClass::CryptoFuture`, 24/7, under the picker's Perps tab) and an exchange-listed
/// traditional one (`AssetClass::Future`: CME, Eurex).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FutureFamily {
    Crypto,
    Traditional,
}

/// What defines a dated future.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct FutureSpec {
    pub margining: Margining,
    /// The asset PnL and margin settle in.
    pub settle: String,
    /// The currency one contract's value is denominated in, where the venue says.
    pub value_ccy: Option<String>,
    /// The expiry instant, epoch ms UTC.
    pub expiry_ms: i64,
    /// The calendar cycle, where the venue publishes one.
    pub tenor: Option<Tenor>,
    pub delivery: Delivery,
    /// Crypto-venue or traditional exchange future; decides the projected `AssetClass`.
    pub family: FutureFamily,
}

/// Exercise style of an option.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExerciseStyle {
    European,
    American,
    Unknown,
}

/// What defines a listed option — read from the payload's own fields, never re-parsed from the
/// instrument's name.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct OptionSpec {
    /// Call or put.
    pub right: OptionKind,
    /// The strike, in the quote currency of the underlying.
    pub strike: f64,
    /// The expiry instant, epoch ms UTC.
    pub expiry_ms: i64,
    pub style: ExerciseStyle,
    pub margining: Margining,
    /// The asset PnL and premium settle in.
    pub settle: String,
    /// The currency one contract's value is denominated in, where the venue says.
    pub value_ccy: Option<String>,
    /// The underlying as the venue names it (an index, a future), where it says.
    pub underlying: Option<String>,
}

/// What a combo is made of.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ComboKind {
    /// Legs are futures (a calendar spread).
    FutureSpread,
    /// Legs are options (a spread, straddle, …).
    OptionCombo,
}

/// One leg of a combo: the leg instrument's venue symbol and its SIGNED ratio (`+` buys the leg
/// when the combo is bought, `−` sells it).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ContractLeg {
    pub symbol: String,
    pub ratio: i32,
}

/// What defines a venue-listed combo (deribit combos, an IBKR `BAG`).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ComboContract {
    pub kind: ComboKind,
    pub legs: Vec<ContractLeg>,
    /// The combo's own expiry, epoch ms UTC, where the venue publishes one.
    pub expiry_ms: Option<i64>,
    /// The settlement asset, where the venue publishes one.
    pub settle: Option<String>,
}
