//! vike-options — the venue-free options pure core, extracted from vike-app on the
//! vike-indicators precedent (egui-free, I/O-free, CI-able). It names two internal crates, both
//! vocabulary: `vike-model` (layer 10), whose `ContractOption` a basket [`BasketLeg`] is built from
//! ([`BasketLeg::from_contract`]), and the market-data vocabulary `vike-marketdata` (layer 5), which owns
//! the option VALUE types:
//! [`OptionKind`](vike_marketdata::OptionKind) and the chain snapshot
//! ([`OptionChain`](vike_marketdata::OptionChain)/[`OptionChainExpiry`](vike_marketdata::OptionChainExpiry)/
//! [`OptionChainStrike`](vike_marketdata::OptionChainStrike)/[`OptionQuote`](vike_marketdata::OptionQuote)/
//! [`OptionUnderlyingKind`](vike_marketdata::OptionUnderlyingKind)) are what a feed PRODUCES, so they moved
//! there (decision 0126) and consumers name them from `vike_marketdata`, never through this crate.
//! What stays here is the MATHS over those values and the constructors that need a calendar:
//! `chrono` and the bit-pinned DTE arithmetic do not belong at the bottom of the graph.
//! Faithful f64 port of the Python oracle `vike-trader-app data/options/` (the oracle is retired and
//! its fixtures frozen, `docs/decisions/0021-python-oracle-retired-vike-is-the-reference.md`):
//!
//! - [`chain::model`] — the helper half of `data/options/model.py`: 08:00-UTC [`expiry_ms`],
//!   calendar-day [`make_expiry`] (0DTE label), symmetric [`limit_strikes`] windowing by the one
//!   at-the-money rule [`atm_strike_window`] (the ticker focus set and the options grid cut by it too).
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
//!   [`OptionChainSet`](vike_marketdata::OptionChainSet) grid, greeks re-derived via
//!   [`enrich_quote`].
//! - [`chain::select`] — the selectors [`resolve_options_selection`], [`underlyings_by_listed_options`] and the
//!   front-expiry ATM focus-set [`front_expiry_focus_instruments`].
//!
//! The ADDITIVE modules below have no Python twin (the oracle stops at first-order greeks) and are
//! therefore NOT parity-gated — they are independent implementations of the standard
//! published formulas, verified by finite differences / hand-computed expectations instead:
//!
//! - [`pricing::second_order`] — vanna/vomma/charm/veta/color ([`second_order_greeks`],
//!   [`GreeksSecondOrder`]) on the same d1/d2/erf internals and 365-day, no-dividend
//!   conventions as [`pricing::greeks`]; charm/veta/color per calendar day, passage-of-time sign.
//! - [`basket`] — multi-leg [`Basket`]/[`BasketLeg`] analytics (a leg's right is the marketdata
//!   [`OptionKind`](vike_marketdata::OptionKind), and [`BasketLeg::from_contract`] takes strike and right
//!   from a `vike_model::ContractOption`): the one per-unit [`intrinsic`] value, piecewise-linear expiry payoff,
//!   exact [`Basket::break_evens`], [`Basket::max_profit`]/[`Basket::max_loss`] as a
//!   [`BasketPayoffBound`] (`Unbounded` decided by the slope past the highest strike; the `S_T = 0`
//!   floor means the downside is never unbounded), signed [`Basket::net_greeks`] across BOTH
//!   [`pricing::greeks`] and [`pricing::second_order`] (through [`position`]'s core), and lognormal
//!   [`Basket::probability_of_profit`] / [`Basket::expected_value`] (fixed-grid, deterministic) plus a
//!   [`Basket::payoff_curve`] sampler for charts.
//! - [`position`] — the ONE summing core of position greeks: [`PositionGreeks`] (per-unit greeks
//!   times the signed quantity, and their fold), [`PortfolioGreeks`] (a book's option sum and its
//!   delta per underlying) and its USD read [`PortfolioNetGreeks`] / [`UnderlyingDelta`]. The basket
//!   and the desktop's Greeks tool both call it.
//! - [`pricing::coin`] — a coin-settled (inverse) option's price and premium-adjusted greeks in
//!   coin ([`coin_settled_greeks`], [`CoinSettledGreeks`]).
//! - [`vol`] — the IV side of the IV↔RV read: nearest-to-forward [`atm_iv`] over an
//!   [`OptionChain`](vike_marketdata::OptionChain) (its two steps [`smile_points`] and
//!   [`atm_iv_of_smile`] for any quote list), the variance-linear smile interpolation [`smile_iv`],
//!   and the [`iv_rv_spread`] difference. The
//!   realized-vol estimators themselves are `vike-indicators`' realized-volatility indicator
//!   family (same layer, so neither crate names the other; the caller passes the realized vol in).
//! - [`features`] — what a model reads off a quote: [`moneyness`], [`log_moneyness`],
//!   [`years_to_expiry_ms`] (the one division by [`MS_PER_YEAR`]), [`quote_iv`], [`delta_at_quote`] and the at-the-money
//!   IV of each expiry [`atm_iv_by_expiry`].
//! - [`svi`] — Gatheral's raw SVI slice ([`SviParameters`]), its fit ([`fit_svi`], through
//!   `vike_math::minimize::nelder_mead`) and the butterfly and calendar arbitrage checks
//!   ([`butterfly_arbitrage`], [`calendar_arbitrage`]).
//!
//! CONTRACT:
//! - Venue-specific fetch/parse (Deribit instrument names, book summaries) lives in
//!   `vike-deribit`'s `chain.rs` (`crates/bridges/deribit/src/feed/options/chain.rs` — crate-reorg
//!   Phase 3, PR F), NOT here; order entry stays on the `ExecutionClient` path. An option's
//!   identity (right, strike, expiry) is `vike-model`'s typed `Contract` on the `Instrument`
//!   (decision 0126); `OrderRequest` still names the instrument by its symbol string.
//! - Parity gates (`tests/oracle_parity.rs`, CPython-pinned): pure-arithmetic paths
//!   (DTE, windowing, scaling, non-theor columns, format strings) are exact;
//!   erf/exp/log-derived outputs gate at ≤1e-12 relative per `fixtures/README.md`.
//!   Never widen a tolerance; never reclassify a pure-arithmetic site to the relative tier.

pub mod basket;
pub mod chain;
pub mod features;
pub mod position;
pub mod pricing;
pub mod svi;
// The deribit instrument and chain builders the fold tests share, here and in deribit's offline suite.
#[cfg(any(test, feature = "test-support"))]
pub mod test_support;
pub mod vol;

pub use basket::{
    Basket, BasketLeg, BasketLegMarket, BasketPayoffBound, intrinsic,
    prob::{BasketExpectedValueGrid, BasketTerminalDistribution},
};
pub use chain::fold::{apply_markprice_to_chains, apply_ticker_to_chains};
pub use chain::index::{OptionAddress, OptionIndex, index_refresh_due, premium_is_usd};
pub use chain::model::{atm_strike_window, expiry_ms, limit_strikes, make_expiry};
pub use chain::select::{
    front_expiry_focus_instruments, resolve_options_selection, underlyings_by_listed_options,
};
pub use features::{atm_iv_by_expiry, delta_at_quote, log_moneyness, moneyness, quote_iv};
pub use position::{PortfolioGreeks, PortfolioNetGreeks, PositionGreeks, UnderlyingDelta};
pub use pricing::coin::{CoinSettledGreeks, coin_settled_greeks};
pub use pricing::greeks::years_to_expiry_ms;
pub use pricing::greeks::{
    MS_PER_YEAR, black_scholes_greeks, black_scholes_price, enrich_quote, implied_vol,
    years_to_expiry,
};
pub use pricing::second_order::{
    GreeksSecondOrder, charm, color, second_order_greeks, vanna, veta, vomma,
};
pub use svi::{
    CalendarArbitrage, SviFit, SviParameters, SviSlice, butterfly_arbitrage, calendar_arbitrage,
    fit_svi,
};
pub use vol::{atm_iv, atm_iv_of_smile, iv_rv_spread, smile_iv, smile_points};
