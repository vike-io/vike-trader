//! Arbitrage and relative value, the maths the book's chapters 37 and 38 build on: test that a combination of
//! prices is stationary, screen many assets for pairs, hold a spread's signals and the rows a model of its reversion
//! learns from, account for the spot–perpetual basis and the funding carry, and lock the arbitrage of a prediction
//! market's complete outcome set. Pure maths, with no clock, file, randomness or environment read, so a research study
//! FILE and a live strategy FILE compute the same numbers (both templates name this crate:
//! `docs/decisions/0089-the-user-code-fence-is-held-by-name-not-by-rank.md`'s addendum of 2026-11-17).
//!
//! - [`cointegration`] — the Engle–Granger test (a regression, then a unit-root test of its residual) and the
//!   Johansen trace and maximal-eigenvalue tests (a vector error-correction model's canonical correlations), with
//!   the published critical values of [`cointegration::critical_values`].
//! - [`pair_discovery`] — [`pair_discovery::cluster_returns`] groups assets by their returns' correlation
//!   (`vike-portfolio`'s clusterings), [`pair_discovery::cointegrated_pairs`] tests the pairs inside each group (or
//!   inside groups the CALLER chose), and [`pair_discovery::factor_residuals`] takes the factors the CALLER computed
//!   out of each asset's returns (Avellaneda and Lee) and leaves a spread of the asset against them.
//! - [`spread`] — [`spread::signals`], a spread with its z-score, half-life and deviation (`vike-indicators`' own),
//!   the feature rows and labels of a model of its reversion, and the gross profit of trading on a prediction.
//! - [`basis_and_funding`] — the spot–perpetual basis, the funding a position is paid and a decision can know, and
//!   the delta-neutral carry's gross profit split into funding and basis.
//! - [`prediction_market`] — the complete-outcome-set arbitrage of a prediction market (buy-all-YES, buy-all-NO,
//!   convert or resolve, the p(1−p) fee) on live books and on cached marks, venue-neutral: a venue passes its fee
//!   schedule and says whether it converts a full NO set at once. A venue bridge is a connection and holds none of it.
//!
//! **Composition, not repetition.** The regressions and the eigen-decomposition are
//! `vike_math::linear_algebra`'s (decision 0141) and the unit-root statistic is `vike_math::stationarity_tests::augmented_dickey_fuller`;
//! the spread z-score, half-life and rolling deviation are `vike_indicators`' (`series::spread_zscore`,
//! `series::half_life`, `vike_math::rolling::rolling_std`), called once, in [`spread::signals`]; the clusterings are
//! `vike_portfolio`'s (`hierarchical`, `kmeans`); the outcome-set lock reads `vike_marketdata::L2Book` and nets
//! `vike_model::FeeSchedule` and `ContractValue::taker_price`. Nothing here fits a regression, factors a matrix,
//! computes a unit-root test, a z-score or a clustering of its own. Those edges (layers 3, 5, 10, 15 and 15) put the
//! crate at layer 20 (a `domain`: it composes two `leaf` crates), which every crate that names it, the research
//! study host and the venue bridges, is above; both user-file templates carry all of them, so BOTH name this crate, a
//! research study's and a strategy file's, and a live pairs, funding-carry or outcome-set strategy calls the maths its
//! study measured.
//!
//! **What stays with its owner.** `vike-indicators` owns the registered pairs rows and the Kalman hedge ratio
//! (`series::kalman_beta`: one call, which a caller makes directly); the cross-exchange maker's pricing is
//! `vike_market_making::xemm`'s; a venue's catalog of outcome sets and its on-chain convert are the venue's
//! (Polymarket's `NegRiskSet` and `settlement::split_merge`, which call into [`prediction_market`]). The principal
//! components are `vike_analytics`' (`vike_portfolio`'s once that module moves): [`pair_discovery::factor_residuals`]
//! takes the factors' RETURNS, so this crate names neither.
//!
//! **Conventions.** A price is passed as its LOG (a spread of log prices is a return, and the hedge ratio is an
//! elasticity). A matrix holds one row per bar and one column per series. An answer that does not exist is
//! `None` (a shape that does not fit, a non-finite input, a singular regression), and inside a vector of
//! per-bar values a `NaN` is "not defined here" (a window not yet full), never a number made up. Every fold is a
//! naive left fold in index order and the only transcendental is `libm`'s, so a result is the same bits on every
//! box.
//!
//! **What this is not.** Cointegration found in a sample is evidence about THAT sample. A pair that passes
//! a screen has passed one test among many, and the critical values are asymptotic; the check is a spread
//! that kept reverting on bars the screen never saw, which is what the study does with a time split. And a
//! spread that reverts on paper must still beat what two legs cost to trade: the profits here are gross, and the
//! costs are a backtest's, the one cost type (`vike_model::Costs`) a study hands to the engine it runs.

pub mod basis_and_funding;
pub mod cointegration;
pub mod pair_discovery;
pub mod prediction_market;
pub mod spread;
