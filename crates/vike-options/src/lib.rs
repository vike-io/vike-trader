//! vike-options — the venue-free options pure core, extracted from vike-app on the
//! vike-indicators precedent (egui-free, I/O-free, CI-able; no internal-crate deps).
//! Faithful f64 port of the Python oracle `vike-trader-app data/options/`:
//!
//! - [`model`] — `data/options/model.py`: the normalized [`OptionChain`]/[`Expiry`]/
//!   [`StrikeRow`]/[`OptionQuote`] chain model, 08:00-UTC [`expiry_ms`], calendar-day
//!   [`make_expiry`] (0DTE label), symmetric [`limit_strikes`] windowing.
//! - [`greeks`] — `data/options/greeks.py`: [`black_scholes_price`],
//!   [`black_scholes_greeks`] (Δ/Γ/Θ-per-day/V-per-point), [`implied_vol`] (64-step
//!   bisection), [`years_to_expiry`], [`enrich_quote`]. erf via `libm`; `r` is a plain
//!   parameter (env reads stay in the consuming binary).
//! - [`columns`] — `data/options/columns.py`: the pure column model
//!   ([`columns::CHAIN_FIELDS`]/[`columns::GREEKS_FIELDS`], [`columns::cell_value`],
//!   [`columns::fmt`]) the chain grid renders from, unit-tested without a widget.
//!
//! Two ADDITIVE modules have no Python twin (the oracle stops at first-order greeks) and are
//! therefore NOT parity-gated — they are independent implementations of the standard
//! published formulas, verified by finite differences / hand-unrolled folds instead:
//!
//! - [`second_order`] — vanna/vomma/charm/veta/color ([`second_order_greeks`],
//!   [`SecondOrderGreeks`]) on the same d1/d2/erf internals and 365-day, no-dividend
//!   conventions as [`greeks`]; charm/veta/color per calendar day, passage-of-time sign.
//! - [`basket`] — multi-leg [`Basket`]/[`Leg`] analytics: piecewise-linear expiry payoff,
//!   exact [`Basket::break_evens`], [`Basket::max_profit`]/[`Basket::max_loss`] as a
//!   [`PayoffBound`] (`Unbounded` decided by the slope past the highest strike; the `S_T = 0`
//!   floor means the downside is never unbounded), signed [`Basket::net_greeks`] across BOTH
//!   [`greeks`] and [`second_order`], and lognormal [`Basket::probability_of_profit`] /
//!   [`Basket::expected_value`] (fixed-grid, deterministic) plus a
//!   [`Basket::payoff_curve`] sampler for charts.
//! - [`vol`] — realized-vol estimators ([`EwmaVol`] RiskMetrics λ=0.94, [`RollingVol`]
//!   sample stdev; both streaming `update(price)`/`value()` over log returns, std-only),
//!   365-day [`annualize`]/[`deannualize`], the [`iv_rv_spread`] read, and nearest-to-forward
//!   [`atm_iv`] over an [`OptionChain`].
//!
//! CONTRACT:
//! - Venue-specific fetch/parse (Deribit instrument names, book summaries) lives in
//!   `vike-deribit`'s `chain.rs` (`crates/bridges/deribit/src/chain.rs` — crate-reorg
//!   Phase 3, PR F), NOT here; order entry stays on the `ExecutionClient` path. Option
//!   identity stays in the instrument-name string — no strike/expiry/kind fields on
//!   vike-model's `OrderRequest`.
//! - Parity gates (`tests/oracle_parity.rs`, CPython-pinned): pure-arithmetic paths
//!   (DTE, windowing, scaling, non-theor columns, format strings) are exact;
//!   erf/exp/log-derived outputs gate at ≤1e-12 relative per `fixtures/README.md`.
//!   Never widen a tolerance; never reclassify a pure-arithmetic site to the relative tier.

pub mod basket;
pub mod columns;
pub mod greeks;
pub mod model;
pub mod second_order;
pub mod vol;

pub use basket::{Basket, EvGrid, Leg, LegMarket, NetGreeks, PayoffBound, TerminalDist};
pub use greeks::{
    black_scholes_greeks, black_scholes_price, enrich_quote, implied_vol, years_to_expiry,
};
pub use model::{
    expiry_ms, limit_strikes, make_expiry, AssetClass, Expiry, OptionChain, OptionKind,
    OptionQuote, StrikeRow,
};
pub use second_order::{charm, color, second_order_greeks, vanna, veta, vomma, SecondOrderGreeks};
pub use vol::{
    annualize, atm_iv, deannualize, iv_rv_spread, EwmaVol, RollingVol, DAYS_PER_YEAR,
    RISKMETRICS_LAMBDA,
};
