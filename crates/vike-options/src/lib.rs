//! vike-options — the venue-free options pure core, extracted from vike-app on the
//! vike-indicators precedent (egui-free, I/O-free, CI-able). It names two internal crates, both
//! vocabulary: `vike-model` (layer 10), whose `OptionSpec` a basket [`Leg`] is built from
//! ([`Leg::from_contract`]), and the market-data vocabulary `vike-marketdata` (layer 5), which owns
//! the option VALUE types:
//! [`OptionKind`](vike_marketdata::OptionKind) and the chain snapshot
//! ([`OptionChain`](vike_marketdata::OptionChain)/[`Expiry`](vike_marketdata::Expiry)/
//! [`StrikeRow`](vike_marketdata::StrikeRow)/[`OptionQuote`](vike_marketdata::OptionQuote)/
//! [`UnderlyingKind`](vike_marketdata::UnderlyingKind)) are what a feed PRODUCES, so they moved
//! there (decision 0126) and consumers name them from `vike_marketdata`, never through this crate.
//! What stays here is the MATHS over those values and the constructors that need a calendar:
//! `chrono` and the bit-pinned DTE arithmetic do not belong at the bottom of the graph.
//! Faithful f64 port of the Python oracle `vike-trader-app data/options/` (the oracle is retired and
//! its fixtures frozen, `docs/decisions/0021-python-oracle-retired-vike-is-the-reference.md`):
//!
//! - [`chain::model`] — the helper half of `data/options/model.py`: 08:00-UTC [`expiry_ms`],
//!   calendar-day [`make_expiry`] (0DTE label), symmetric [`limit_strikes`] windowing.
//! - [`pricing::greeks`] — `data/options/greeks.py`: [`black_scholes_price`],
//!   [`black_scholes_greeks`] (Δ/Γ/Θ-per-day/V-per-point), [`implied_vol`] (64-step
//!   bisection), [`years_to_expiry`], [`enrich_quote`]. erf via `libm`; `r` is a plain
//!   parameter (env reads stay in the consuming binary).
//! - [`chain::columns`] — `data/options/columns.py`: the pure column model
//!   ([`chain::columns::CHAIN_FIELDS`]/[`chain::columns::GREEKS_FIELDS`], [`chain::columns::cell_value`],
//!   [`chain::columns::fmt`]) the chain grid renders from, unit-tested without a widget.
//!
//! The option grid's DATA PATH — no Python twin, moved here from `vike-app-core`'s `tools/` so the
//! server folding the deribit feeds and the desktop tools run one implementation:
//!
//! - [`chain::index`] — the [`OptionIndex`]: an instrument NAME to the contract its payload
//!   defines ([`OptionIndex::from_instruments`], [`OptionIndex::address`] -> [`OptionAddress`],
//!   [`premium_is_usd`]), and the refetch cadence [`index_refresh_due`]. It fetches nothing; a name
//!   it lacks is skipped, never parsed.
//! - [`chain::fold`] — the streamed-row folds [`apply_markprice_to_chains`] and
//!   [`apply_ticker_to_chains`]: `vike_marketdata::OptionTicker` rows applied in place onto a
//!   [`UnderlyingChains`](vike_marketdata::UnderlyingChains) grid, greeks re-derived via
//!   [`enrich_quote`].
//! - [`chain::select`] — the selectors [`resolve_options_selection`], [`underlying_rank`] and the
//!   front-expiry ATM focus-set [`front_expiry_focus_instruments`].
//!
//! Three ADDITIVE modules have no Python twin (the oracle stops at first-order greeks) and are
//! therefore NOT parity-gated — they are independent implementations of the standard
//! published formulas, verified by finite differences / hand-computed expectations instead:
//!
//! - [`pricing::second_order`] — vanna/vomma/charm/veta/color ([`second_order_greeks`],
//!   [`SecondOrderGreeks`]) on the same d1/d2/erf internals and 365-day, no-dividend
//!   conventions as [`pricing::greeks`]; charm/veta/color per calendar day, passage-of-time sign.
//! - [`basket`] — multi-leg [`Basket`]/[`Leg`] analytics (a leg's right is the marketdata
//!   [`OptionKind`](vike_marketdata::OptionKind), and [`Leg::from_contract`] takes strike and right
//!   from a `vike_model::OptionSpec`): piecewise-linear expiry payoff,
//!   exact [`Basket::break_evens`], [`Basket::max_profit`]/[`Basket::max_loss`] as a
//!   [`PayoffBound`] (`Unbounded` decided by the slope past the highest strike; the `S_T = 0`
//!   floor means the downside is never unbounded), signed [`Basket::net_greeks`] across BOTH
//!   [`pricing::greeks`] and [`pricing::second_order`], and lognormal [`Basket::probability_of_profit`] /
//!   [`Basket::expected_value`] (fixed-grid, deterministic) plus a
//!   [`Basket::payoff_curve`] sampler for charts.
//! - [`vol`] — the IV side of the IV↔RV read: nearest-to-forward [`atm_iv`] over an
//!   [`OptionChain`](vike_marketdata::OptionChain) and the [`iv_rv_spread`] difference. The
//!   realized-vol estimators themselves are `vike-indicators`' realized-volatility indicator
//!   family (same layer, so neither crate names the other; the caller passes the realized vol in).
//!
//! CONTRACT:
//! - Venue-specific fetch/parse (Deribit instrument names, book summaries) lives in
//!   `vike-deribit`'s `chain.rs` (`crates/bridges/deribit/src/chain.rs` — crate-reorg
//!   Phase 3, PR F), NOT here; order entry stays on the `ExecutionClient` path. An option's
//!   identity (right, strike, expiry) is `vike-model`'s typed `Contract` on the `Instrument`
//!   (decision 0126); `OrderRequest` still names the instrument by its symbol string.
//! - Parity gates (`tests/oracle_parity.rs`, CPython-pinned): pure-arithmetic paths
//!   (DTE, windowing, scaling, non-theor columns, format strings) are exact;
//!   erf/exp/log-derived outputs gate at ≤1e-12 relative per `fixtures/README.md`.
//!   Never widen a tolerance; never reclassify a pure-arithmetic site to the relative tier.

pub mod basket;
pub mod chain;
pub mod pricing;
pub mod vol;

pub use basket::{
    Basket, Leg, LegMarket, NetGreeks, PayoffBound,
    prob::{EvGrid, TerminalDist},
};
pub use chain::fold::{apply_markprice_to_chains, apply_ticker_to_chains};
pub use chain::index::{OptionAddress, OptionIndex, index_refresh_due, premium_is_usd};
pub use chain::model::{expiry_ms, limit_strikes, make_expiry};
pub use chain::select::{
    front_expiry_focus_instruments, resolve_options_selection, underlying_rank,
};
pub use pricing::greeks::{
    black_scholes_greeks, black_scholes_price, enrich_quote, implied_vol, years_to_expiry,
};
pub use pricing::second_order::{
    SecondOrderGreeks, charm, color, second_order_greeks, vanna, veta, vomma,
};
pub use vol::{atm_iv, iv_rv_spread};
