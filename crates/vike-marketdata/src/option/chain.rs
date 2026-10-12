//! The normalized options-chain snapshot types — moved here from `vike-options` (decision 0126);
//! ports `vike-trader-app data/options/model.py`.
//!
//! Providers translate their native payloads into these types; the UI only ever consumes
//! [`OptionChain`] / [`Expiry`] (bundled per underlying as [`UnderlyingChains`]), so it never knows
//! which feed produced them. Contract notes:
//! - Quote fields are `Option<f64>` (the Python dataclass's `float | None`); absent stays
//!   absent, never 0.0.
//! - `rows` are ascending by strike; expiries settle ~08:00 UTC (`vike-options`' `expiry_ms`).
//!
//! Only the TYPES moved. The calendar constructors (`expiry_ms`, `make_expiry`) and the strike
//! window (`limit_strikes`) stay in `vike-options`: they need `chrono`, which this crate does not
//! take, and their DTE arithmetic is a parity site that crate pins.
//!
//! Serde: every type derives it (the `vike-options` originals did not); the optional fields carry
//! `#[serde(default)]`, so a snapshot written before a field existed still deserialises.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use super::OptionKind;

/// One option quote, normalized — twin of `model.OptionQuote` (frozen dataclass).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct OptionQuote {
    pub strike: f64,
    pub kind: OptionKind,
    #[serde(default)]
    pub bid: Option<f64>,
    #[serde(default)]
    pub ask: Option<f64>,
    #[serde(default)]
    pub last: Option<f64>,
    #[serde(default)]
    pub mark: Option<f64>,
    /// Implied vol as a decimal (0.62 == 62%).
    #[serde(default)]
    pub iv: Option<f64>,
    #[serde(default)]
    pub open_interest: Option<f64>,
    #[serde(default)]
    pub volume: Option<f64>,
    #[serde(default)]
    pub delta: Option<f64>,
    #[serde(default)]
    pub gamma: Option<f64>,
    /// Per calendar day.
    #[serde(default)]
    pub theta: Option<f64>,
    /// Per 1pp change in IV (0.01 decimal).
    #[serde(default)]
    pub vega: Option<f64>,
    #[serde(default)]
    pub in_the_money: Option<bool>,
    /// Venue contract id (Deribit); `None` for equity feeds.
    #[serde(default)]
    pub instrument_name: Option<String>,
}

impl OptionQuote {
    /// A quote with every optional field absent — the Python dataclass's defaults.
    pub fn new(strike: f64, kind: OptionKind) -> Self {
        Self {
            strike,
            kind,
            bid: None,
            ask: None,
            last: None,
            mark: None,
            iv: None,
            open_interest: None,
            volume: None,
            delta: None,
            gamma: None,
            theta: None,
            vega: None,
            in_the_money: None,
            instrument_name: None,
        }
    }
}

/// One expiry with its display metadata — twin of `model.Expiry`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Expiry {
    /// ISO "YYYY-MM-DD".
    pub date: String,
    /// Days to expiry (>= 0).
    pub dte: i64,
    /// Display label, e.g. "02 Jul" / "0DTE".
    pub label: String,
}

/// Call + put at one strike — twin of `model.StrikeRow`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct StrikeRow {
    pub strike: f64,
    #[serde(default)]
    pub call: Option<OptionQuote>,
    #[serde(default)]
    pub put: Option<OptionQuote>,
}

/// The kind of thing a chain is written ON — the Python twin's
/// `Literal["crypto", "equity"]`.
///
/// ⚠ **This is NOT the workspace's asset-class taxonomy narrowed to two, and the name no longer
/// invites that reading.** It was spelled `AssetClass` — a third type of that name in this tree —
/// and that was wrong twice over. Every instrument an [`OptionChain`] holds is an option, so the
/// class of the INSTRUMENTS is a single value, `vike_model::AssetClass::Option`, one of the eleven
/// that type declares; this axis answers the other question, "an option on WHAT", and two answers
/// are all the providers here produce (deribit's crypto chains, yfinance's equity ones). Merging
/// the two would therefore be a category error, not a de-duplication — and it is structurally
/// unavailable anyway: this crate has NO internal-crate dependencies (its manifest states that
/// property), so the taxonomy is not nameable from here even in principle.
///
/// `Kind` rather than `Class` is deliberate: `Class` is the taxonomy's own word, and the confusion
/// this rename ends came from borrowing it for something that is not a taxonomy but a two-way
/// split. [`OptionKind`], this module's parent, already makes `Kind` the word for a small closed
/// enum — call/put is the kind of the option, crypto/equity the kind of the underlying it is
/// written on.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UnderlyingKind {
    Crypto,
    Equity,
}

/// One expiry's full chain snapshot — twin of `model.OptionChain`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct OptionChain {
    /// "BTC", "^VIX".
    pub underlying: String,
    /// The kind of [`OptionChain::underlying`] — never of the chain's own rows, which are all
    /// options. See [`UnderlyingKind`], which carries why that distinction earns its own name.
    pub underlying_kind: UnderlyingKind,
    #[serde(default)]
    pub underlying_price: Option<f64>,
    pub expiry: Expiry,
    /// Snapshot epoch ms (UTC).
    pub asof_ms: i64,
    /// "deribit" | "yfinance".
    pub source: String,
    /// Ascending by strike.
    pub rows: Vec<StrikeRow>,
}

/// One underlying's options snapshot: its nearest expiries for the date strip, each expiry's
/// chain keyed by its ISO date, and the front-expiry default selection. What a chain provider
/// hands the Options tool for one underlying, and the unit the streamed-row folds in
/// `vike-options` update in place; a feed and a server snapshot carry the same shape.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct UnderlyingChains {
    /// The nearest expiry's ISO date (the default selection).
    pub default_expiry: String,
    /// The nearest expiries, ordered, for the date strip.
    pub expiries: Vec<Expiry>,
    /// Expiry ISO date → that expiry's chain.
    pub chains: BTreeMap<String, OptionChain>,
}

#[path = "chain_tests.rs"]
#[cfg(test)]
mod chain_tests;
