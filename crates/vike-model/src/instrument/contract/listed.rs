//! The defining data of the cash and over-the-counter classes of [`super::Contract`]: equity and
//! etf ([`EquitySpec`]), spot fx ([`FxSpec`]), cfd ([`CfdSpec`] with its sub-kind, product and
//! expiry) and the prediction outcome ([`PredictionSpec`]).
//!
//! Every field a venue may not publish is an `Option`, and `None` means UNKNOWN — never a default
//! value standing in for one (the parent module's rules).

use serde::{Deserialize, Serialize};

/// What defines a cash equity or an ETF.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct EquitySpec {
    /// The listing exchange as the venue names it (alpaca's `exchange`, IBKR's `primaryExch`).
    pub exchange: Option<String>,
    /// The currency the share is quoted and settled in.
    pub currency: Option<String>,
}

/// What defines a spot FX pair beyond its two currencies (which are the `Instrument`'s
/// `base`/`quote`). ctrader and oanda publish all four; most venues none.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct FxSpec {
    /// One pip in price units (`0.0001` for EURUSD, `0.01` for USDJPY).
    pub pip_size: Option<f64>,
    /// Units of the base currency in one standard lot.
    pub lot_size: Option<f64>,
    /// Overnight financing for a held LONG, as the venue publishes it.
    pub swap_long: Option<f64>,
    /// Overnight financing for a held SHORT, as the venue publishes it.
    pub swap_short: Option<f64>,
}

/// The kind of thing a CFD tracks — what oanda, ig and fxcm used to fold into one undifferentiated
/// `Cfd`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CfdUnderlying {
    Index,
    Metal,
    Commodity,
    Share,
    Crypto,
    Bond,
    Fx,
    /// The venue names a kind none of the above covers.
    Other,
}

/// The legal wrapper of a CFD-style product (IG lists both).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CfdProduct {
    Cfd,
    SpreadBet,
}

/// How a CFD expires.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CfdExpiry {
    /// No expiry; held positions roll (IG's `"-"`).
    Rolling,
    /// A daily-funded bet (IG's `"DFB"`).
    Dfb,
    /// A dated contract expiring at this instant, epoch ms UTC.
    Date(i64),
    /// The venue publishes no expiry for this row.
    Unknown,
}

/// What defines a CFD.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct CfdSpec {
    /// What kind of thing it tracks.
    pub underlying_kind: CfdUnderlying,
    /// The tracked instrument as the venue names it (an opaque reference, not parsed here).
    pub underlying: Option<String>,
    /// CFD or spread bet.
    pub product: CfdProduct,
    /// How it expires.
    pub expiry: CfdExpiry,
    /// The venue's margin rate as a fraction of notional (`0.05` = 5 %).
    pub margin_rate: Option<f64>,
    /// Overnight financing for a held LONG, as the venue publishes it.
    pub swap_long: Option<f64>,
    /// Overnight financing for a held SHORT, as the venue publishes it.
    pub swap_short: Option<f64>,
}

/// What defines one outcome token of a prediction market (polymarket).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PredictionSpec {
    /// The market's condition id.
    pub condition_id: String,
    /// This token's outcome label (`"Yes"`, `"No"`, a candidate's name).
    pub outcome: String,
    /// The outcome token's id on the venue.
    pub token_id: String,
    /// When the market is scheduled to resolve, epoch ms UTC.
    pub resolution_ms: Option<i64>,
    /// Whether the market is a neg-risk market (it selects the signing domain on polymarket).
    /// ⚠ `None` is UNKNOWN, NOT `false`: a consumer that needs the answer must refuse a `None`
    /// rather than read it as a plain market.
    pub neg_risk: Option<bool>,
}
