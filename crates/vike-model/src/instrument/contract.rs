//! [`Contract`] — WHAT an instrument is, as typed data: the sum type of every instrument class this
//! workspace lists, each variant carrying the fields that DEFINE its class.
//!
//! `docs/decisions/0126-every-instrument-class-is-a-typed-contract-in-vike-model-and-feed-values-live-in-vike-marketdata.md`
//! is the verdict. Before it, a class was a bare [`AssetClass`] tag and everything the class MEANT
//! (an option's strike and expiry, a perp's linear-or-inverse margining, a future's expiry) was
//! re-derived from symbol strings, differently at every consumer. Here it is carried as data from
//! the one place it is known for free: the venue payload the catalog parsed.
//!
//! # The contract this module keeps
//!
//! - **One variant per class, with its defining data.** An `Fx` cannot carry a strike and a `Perp`
//!   cannot carry an expiry: the shape refuses what the class does not have.
//! - **Unknown is `None`, never a default.** A field the venue does not publish is an `Option`, and
//!   `None` is a different claim from any value — [`PredictionSpec::neg_risk`] is the sharpest case
//!   (`None` = unknown, NOT `false`). A parser fills what the payload says and leaves the rest absent.
//! - **[`Margining`] is read from a payload field** (`contractType`, `ctType`, `instrument_type`,
//!   `marginAsset`), never inferred from a host, a category list or a symbol suffix.
//! - **[`AssetClass`] is the PROJECTION, not a second vocabulary**: [`Contract::class`] is
//!   exhaustive, and the stored word, the picker tab and the docs page all key on its answer.
//! - Times are epoch milliseconds UTC (`i64`); no calendar type enters this crate.
//!
//! The spec structs live in two private children and are re-exported here and at the crate root:
//! `contract/listed.rs` (the cash and over-the-counter classes: equity, fx, cfd, prediction outcome)
//! and `contract/derivatives.rs` (perp, future, option, combo).

mod derivatives;
mod listed;

pub use derivatives::{
    ComboContract, ComboKind, ContractLeg, Delivery, ExerciseStyle, FutureFamily, FutureSpec,
    OptionSpec, PerpSpec, Tenor,
};
pub use listed::{
    CfdExpiry, CfdProduct, CfdSpec, CfdUnderlying, EquitySpec, FxSpec, PredictionSpec,
};

use crate::instrument::asset_class::AssetClass;
use serde::{Deserialize, Serialize};

/// How a derivative's PnL and margin relate to its quote: the product fact a router and the
/// economics both need, typed so nobody re-derives it from a venue word.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Margining {
    /// Margined and settled in the QUOTE (or a stable) asset: PnL is `qty · Δprice`.
    Linear,
    /// Margined and settled in the BASE asset ("coin-margined", deribit's `reversed`): PnL is
    /// `qty · (1/entry − 1/exit)` in base units.
    Inverse,
    /// Settled in a third asset at a fixed conversion: neither the base nor the quote.
    Quanto,
}

/// What an instrument IS, with the data that defines it. See the module doc for the rules every
/// variant keeps.
///
/// Serialized externally tagged with `snake_case` tags (`"index"`, `{"perp": {…}}`); `f64` fields
/// inside rule out `Eq`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Contract {
    /// A cash equity share.
    Equity(EquitySpec),
    /// An exchange-traded fund. A venue that cannot tell an ETF from a share lists an
    /// [`Contract::Equity`]; this variant is a refinement only a venue that says so may claim.
    Etf(EquitySpec),
    /// An index level (a reference value; a CFD or future may point at one).
    Index,
    /// A bond.
    Bond,
    /// A fund that is not exchange-traded (mutual fund).
    Fund,
    /// A warrant.
    Warrant,
    /// A spot commodity (IBKR `CMDTY`, a physical metal). A commodity CFD is
    /// [`Contract::Cfd`] with [`CfdUnderlying::Commodity`]/[`CfdUnderlying::Metal`].
    Commodity,
    /// A crypto spot pair; `base`/`quote` on the `Instrument` are its two assets.
    CryptoSpot,
    /// A spot FX pair.
    Fx(FxSpec),
    /// A contract for difference (or a spread bet) on some underlying.
    Cfd(CfdSpec),
    /// A perpetual swap. ⚠ A perp has NO expiry (deribit's `32503708800000` sentinel is dropped at
    /// the parser).
    Perp(PerpSpec),
    /// A dated future.
    Future(FutureSpec),
    /// A listed option.
    Option(OptionSpec),
    /// A venue-listed multi-leg instrument (a future spread or an option combo). An order on it is
    /// an ordinary order on its symbol.
    Combo(ComboContract),
    /// One outcome token of a prediction market.
    PredictionOutcome(PredictionSpec),
}

impl Contract {
    /// The [`AssetClass`] this contract projects to — the stored word, the picker tab and the docs
    /// page key on it. Exhaustive: a new variant cannot exist without its author choosing a class.
    ///
    /// [`Contract::Perp`] is [`AssetClass::CryptoPerp`] (the taxonomy has no other perp word);
    /// [`Contract::Future`] splits on [`FutureSpec::family`].
    #[must_use]
    pub fn class(&self) -> AssetClass {
        match self {
            Contract::Equity(_) => AssetClass::Equity,
            Contract::Etf(_) => AssetClass::Etf,
            Contract::Index => AssetClass::Index,
            Contract::Bond => AssetClass::Bond,
            Contract::Fund => AssetClass::Fund,
            Contract::Warrant => AssetClass::Warrant,
            Contract::Commodity => AssetClass::Commodity,
            Contract::CryptoSpot => AssetClass::CryptoSpot,
            Contract::Fx(_) => AssetClass::Fx,
            Contract::Cfd(_) => AssetClass::Cfd,
            Contract::Perp(_) => AssetClass::CryptoPerp,
            Contract::Future(f) => match f.family {
                FutureFamily::Crypto => AssetClass::CryptoFuture,
                FutureFamily::Traditional => AssetClass::Future,
            },
            Contract::Option(_) => AssetClass::Option,
            Contract::Combo(_) => AssetClass::Combo,
            Contract::PredictionOutcome(_) => AssetClass::PredictionMarket,
        }
    }

    /// The asset PnL and margin settle in — bybit's `settleCoin`, deribit's
    /// `settlement_currency`, okx's `settleCcy`. `Some` only for the derivative classes that carry
    /// one ([`Contract::Perp`], [`Contract::Future`], [`Contract::Option`], and a
    /// [`Contract::Combo`] whose venue published it); `None` for every cash class.
    #[must_use]
    pub fn settle_asset(&self) -> Option<&str> {
        match self {
            Contract::Perp(p) => Some(p.settle.as_str()),
            Contract::Future(f) => Some(f.settle.as_str()),
            Contract::Option(o) => Some(o.settle.as_str()),
            Contract::Combo(c) => c.settle.as_deref(),
            _ => None,
        }
    }

    /// The derivative's [`Margining`]; `None` for a class that has none.
    #[must_use]
    pub fn margining(&self) -> Option<Margining> {
        match self {
            Contract::Perp(p) => Some(p.margining),
            Contract::Future(f) => Some(f.margining),
            Contract::Option(o) => Some(o.margining),
            _ => None,
        }
    }

    /// When the contract expires, epoch ms UTC: a future's or an option's expiry, a combo's when
    /// published, a dated CFD's [`CfdExpiry::Date`]. `None` for everything that does not expire —
    /// a perp included — and for a combo or CFD whose expiry is unknown.
    #[must_use]
    pub fn expiry_ms(&self) -> Option<i64> {
        match self {
            Contract::Future(f) => Some(f.expiry_ms),
            Contract::Option(o) => Some(o.expiry_ms),
            Contract::Combo(c) => c.expiry_ms,
            Contract::Cfd(CfdSpec { expiry: CfdExpiry::Date(ms), .. }) => Some(*ms),
            _ => None,
        }
    }
}

#[path = "contract_tests.rs"]
#[cfg(test)]
mod contract_tests;
