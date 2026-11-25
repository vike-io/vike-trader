//! Every pure formula that names no trading concept, whatever its number of users (decision 0294):
//! one home at the very bottom of the workspace graph (`layer = 2`), named at the module, never
//! re-exported at the crate root.
//!
//! A function belongs here when it is pure computation over numbers and names no order, position,
//! fill, fee, venue, instrument, account or strategy metric, however many crates call it, one
//! included. Domain maths (options pricing, strategy metrics and overfitting tests, arbitrage,
//! portfolio construction, the indicators and their registry, market making, the model crates) stays
//! in its domain crate and calls this one. **Before writing a formula anywhere in the workspace,
//! search this crate first**; moving a formula here deletes its copies in the same change.
//!
//! # The contract every module keeps
//!
//! 1. **Every function is pure**: no clock, file, environment or randomness of its own. A seeded
//!    generator ([`random::SplitMix64`]) is a value the caller holds.
//! 2. **Every transcendental is `libm`'s**, never `f64`'s platform methods
//!    (`docs/decisions/0032-transcendentals-come-from-the-libm-crate-not-the-platform.md`), so a
//!    value is the same bits on Windows and Linux.
//! 3. **Folds run in index order, with no `mul_add`, and the operand order and association of an
//!    expression are part of its contract**: results here are compared bit for bit (the parity
//!    fixtures, the indicator fingerprints, the per-crate platform probes), so a rewrite that is
//!    algebraically equal is still a change. Two summation rules exist on purpose: the compensated
//!    sum ([`summation::compensated_sum`]) and the plain left fold.
//! 4. **"No answer" is `None` or `NaN`**, as each function documents. A silent `0.0` is a CALLER's
//!    mapping, never this crate's: the few functions that moved here with a documented `0.0`
//!    ([`statistics::percentile`], [`statistics::pearson`],
//!    [`statistics::sample_skewness`], [`statistics::sample_excess_kurtosis`],
//!    [`regression::slope_t_value`], [`time_series::newey_west_t_statistic`]) say so in their own docs,
//!    because callers were written against it, and a new function adds none.
//! 5. **Named at the module**, nothing re-exported at the root: a caller writes
//!    `vike_math::statistics::mean_variance`, `vike_math::linear_algebra::least_squares`.
//! 6. **The plain-fold twin** ([`statistics::plain_mean`]) exists only because platform-probe rows
//!    pin a plain fold, and a probe row cannot be re-recorded without a run on both platforms; its doc
//!    names the rows. When those rows are re-recorded it folds into the compensated canonical
//!    (decision 0294's reopener), as the variance and Pearson twins did on 2026-11-18 (its amendment).
//!
//! It names no `vike-*` crate and cannot: nothing in the workspace declares a rank below 2. Its one
//! dependency is `libm`. It is PUBLISHED to crates.io (decision 0071's addendum of 2026-11-17),
//! because the published `vike-indicators` names it: a function here is a public promise, and its
//! module path is the API.

pub mod change_detection;
pub mod distributions;
pub mod garch;
pub mod hidden_markov;
pub mod hypothesis_testing;
pub mod linear_algebra;
pub mod minimize;
pub mod number_theory;
pub mod powers;
pub mod price_transforms;
pub mod random;
pub mod regression;
pub mod resampling;
pub mod rolling;
pub mod smoothing;
pub mod special_functions;
pub mod stationarity_tests;
pub mod statistics;
pub mod steps;
pub mod summation;
pub mod time_series;

#[cfg(test)]
mod test_support;
