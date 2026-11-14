//! `vike-market-making` — the market-maker MATHS: the pure pricing and risk functions the two market-making
//! strategy FILES price through, and the spread maker's presets. The makers themselves — the spread
//! maker (`strategies/market_making/spread_maker.rs`) and the cross-exchange maker
//! (`strategies/market_making/cross_exchange_maker.rs`) — are strategy files carrying their struct,
//! their `Strategy` impl, their saved state and their white-box tests (plan 2b); this crate holds no
//! strategy and owns no runtime. Its `vike-*` edges are vike-model and vike-marketdata.
//!
//! Each layer is a pure module that unit-tests without a runtime, and each has a neutral default
//! that reduces the maker to its plain behaviour bit-for-bit:
//! - [`skew`]: inventory-skew size shaping (`skew_multipliers`) and the fill-rate breaker's
//!   round-trip netting (`net_signed_fills`);
//! - [`BookView`], [`filter_own`], [`OwnBookFiltration`]: the normalized book, the
//!   [`QuoteStyle`](vike_model::QuoteStyle) placement (`BookView::priced`) and own-order filtration;
//! - [`avellaneda`]: the Avellaneda–Stoikov / Guéant pricing layer and its online estimators
//!   (`AsState`), with the κ/A fits and the Group-B fair-value and spread sources behind it;
//! - [`refresh`]: the order-refresh tolerance (the anti-churn gate); [`ladder`]: the per-side rungs;
//!   [`reward`]: the liquidity-rewards model and fold;
//! - [`alpha`], [`settlement`], [`underlying`]: the short-alpha drift, the near-resolution
//!   heuristics and the underlying-spot tracker;
//! - [`taker_flatten`] and [`xemm`]: the cross-exchange maker's impulse leg and its basis, guard,
//!   hedge and pricing maths.
//!
//! [`SpreadMakerPreset`] and [`rearm_fee_floor`] are what a mount hands the spread maker; its reader
//! (`from_params`) is the file's, and [`MakerParamError`] is the refusal that reader returns.

use vike_model::params::Params;

// The maths, split into cohesive modules: `skew` = inventory-skew sizing + fill-rate netting,
// `book` = the normalized book view + quote-style pricing + own-order filtration, `avellaneda` = the
// A-S pricing layer, `fits` = its κ/A estimators. The `pub` ones are what the market-making strategy
// FILES (`strategies/market_making/`) price through.
pub mod alpha;
pub mod avellaneda;
mod book;
mod fairvalue;
mod fits;
pub mod ladder;
// The pure correlated-inventory (multi-asset Guéant) skew core — `γ·(Σ·q)`. Written ahead of the
// live multi-symbol maker mount (a DEFERRED follow-up), so nothing on a live path calls it yet;
// `#[allow(dead_code)]` keeps the merge-gate clippy green until the mount wires it in.
// allow, not expect: dead in the lib build, used by the lib's own tests (measured 2026-10-10).
#[allow(dead_code, clippy::allow_attributes)]
mod multi_asset;
mod preset;
pub mod refresh;
pub mod settlement;
mod spread_source;
// The band→taker-flatten impulse leg (Guilbaud–Pham). Formerly a Group-C DEFERRED pure core under
// `#[allow(dead_code)]`; the cross-exchange maker file calls it as its hard-naked-band impulse, so
// it is `pub` and clippy gates it like any other module.
pub mod taker_flatten;
// The CROSS-EXCHANGE maker's MATHS (`xemm::{basis, guards, hedge, pricing}`). The maker itself is
// the strategy FILE `strategies/market_making/cross_exchange_maker.rs`, ⚠ MOUNTED BY NOTHING:
// `crates/vike-tradehub-core/src/xemm/live.rs`'s `build_live_xemm_core` has no caller and its paper
// twin is called only from tests (`docs/decisions/0033-xemm-is-built-but-mounted-by-nothing.md` is
// the verdict).
pub mod xemm;
// The liquidity-rewards MODEL + fold is a `pub mod` so its pure, unit-tested scoring functions
// (`spread_score`/`side_score`/`q_min`) are public API a Polymarket mount / analysis can compute
// expected reward with, and so are the maker's `clamp_into_band`/`moas_holds` fold helpers (the
// strategy file prices through them).
/// The cross-platform pin for this crate's quote math. `#[cfg(test)]`, so it compiles into no
/// shipped build — and it lives INSIDE `src/` rather than under `tests/` because every function it
/// covers is `pub(crate)` in a private module, which an integration test cannot reach. Its own
/// module doc carries the measurement and what it does and does not buy.
#[cfg(test)]
mod platform_probe;
pub mod reward;
pub mod skew;
pub mod underlying;

// The book view, its quote-style pricing and the own-order filtration bundle (the multi-order,
// race-aware alternative to the single-snapshot `filter_own`), public so a mount/runtime can
// drive REAL venue lifecycle events into an `OwnBookFiltration`. The LADDER it wraps is
// `vike_model::OrderBookOwn`: it left this crate so a layer below the strategies can maintain it.
pub use book::{BookView, OwnBookFiltration, filter_own};
// The spread maker's PRESETS (the two price domains a mount starts from) and the mount-time re-arm
// of its break-even floor: `preset.rs`.
pub use preset::{SpreadMakerPreset, rearm_fee_floor};

/// A `[strategy.params]` key whose value NAMES A VARIANT that does not exist — a profile author's
/// typo, returned by the spread maker's `from_params` (`strategies/market_making/spread_maker.rs`)
/// and by [`SpreadMakerPreset::named`] instead of being absorbed into the default.
///
/// ⚠ **This type exists because `Option` could not say which of two things happened.** Each enum
/// reader used to return `None` for BOTH "the key is absent" and "the key is present and
/// unrecognized", and `from_params` folded that `None` into `unwrap_or(default)` — so
/// `spread_model = "gueant_lehale"` (one `l` short of the accepted `gueant_lehalle`) silently ran
/// Avellaneda–Stoikov while the profile named GLFT, and any startup line reported the fallback as
/// if it had been chosen. A backtest that ran a different model than its profile names is a result
/// nobody can trust, and nothing said so. An ABSENT key still keeps its default — that is the
/// documented reader convention and is unchanged.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MakerParamError {
    /// The key is present and IS a string, but names no variant. Carries the operator's own value
    /// back so the message is greppable against the profile they wrote.
    Unrecognized {
        /// The `[strategy.params]` key, e.g. `"spread_model"`.
        key: &'static str,
        /// The value as written in the profile.
        value: String,
        /// Every accepted spelling of that key, `|`-separated (the match arms, verbatim).
        accepted: &'static str,
    },
    /// The key is present but is not a string at all (`spread_model = 3`) — the same silent
    /// fallback wearing a different shape, since `ParamValue::as_str` also answered `None` for it.
    NotAString {
        /// The `[strategy.params]` key.
        key: &'static str,
        /// The kind actually found, as
        /// [`ParamValue::type_str`](vike_model::params::ParamValue::type_str) spells it
        /// (`"integer"`, …).
        found: &'static str,
        /// Every accepted spelling of that key, `|`-separated.
        accepted: &'static str,
    },
}

impl std::fmt::Display for MakerParamError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            MakerParamError::Unrecognized { key, value, accepted } => write!(
                f,
                "strategy param {key} = {value:?} is not a recognized value (accepted: {accepted})"
            ),
            MakerParamError::NotAString { key, found, accepted } => write!(
                f,
                "strategy param {key} must be a string, got a TOML {found} \
                 (accepted: {accepted})"
            ),
        }
    }
}

impl std::error::Error for MakerParamError {}

/// The front half of the enum reader below (the spread maker file carries its own copy for its
/// readers): resolve `key` to its string, or say WHICH of the three states the table is in. `Ok(None)` is the ABSENT key (keep the default); `Err` is the
/// present-but-not-a-string case; `Ok(Some(s))` is the value AS WRITTEN — each reader lowercases
/// for its own match and keeps this copy, so a failure quotes the operator's own spelling back at
/// them rather than a normalized one. `accepted` is threaded through only so a failure can print it.
fn param_str(
    params: &Params,
    key: &'static str,
    accepted: &'static str,
) -> Result<Option<String>, MakerParamError> {
    match params.get(key) {
        None => Ok(None),
        Some(v) => match v.as_str() {
            Some(s) => Ok(Some(s.to_string())),
            None => Err(MakerParamError::NotAString { key, found: v.type_str(), accepted }),
        },
    }
}

impl SpreadMakerPreset {
    /// The preset `params` names in its `preset` key (case-insensitive): the bag the spread
    /// maker's `from_params` starts from. `Ok(None)` only for an ABSENT key (the bare defaults); a
    /// present-but-unrecognized value is a [`MakerParamError`]. The maker file's key-set test
    /// (`tests::from_params`) reads its `KEY` from THIS file, so keep the lookup spelled `KEY`.
    ///
    /// # Errors
    ///
    /// [`MakerParamError`] for a `preset` that is not a string or names no preset.
    pub fn named(params: &Params) -> Result<Option<Self>, MakerParamError> {
        const KEY: &str = "preset";
        const ACCEPTED: &str = "outcome_token | crypto";
        let Some(raw) = param_str(params, KEY, ACCEPTED)? else { return Ok(None) };
        match raw.to_ascii_lowercase().as_str() {
            "outcome_token" => Ok(Some(SpreadMakerPreset::OutcomeToken)),
            "crypto" => Ok(Some(SpreadMakerPreset::Crypto)),
            _ => Err(MakerParamError::Unrecognized { key: KEY, value: raw, accepted: ACCEPTED }),
        }
    }
}

#[cfg(test)]
mod tests;
