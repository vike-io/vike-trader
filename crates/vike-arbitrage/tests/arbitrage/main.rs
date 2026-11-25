//! `vike-arbitrage`, through its public API only: the cointegration tests and their critical values, the pair
//! screen (clusters of returns and the pairs inside them) and the factor residuals, the spread's signals with the rows
//! of a model of reversion, the basis and funding carry, and the prediction-market lock. One test binary, one file
//! per module under test. Every series here is built from a fixed seed with plain arithmetic (a 64-bit LCG, no
//! `rand`, one copy in `common`), so a run is the same on every box.
//!
//! The research studies `pairs_reversion` and `funding_carry` call the crate end to end, directly and jailed:
//! `crates/vike-user-research/tests/pipeline.rs` and `crates/vike-strategy-sandbox/tests/study_child.rs`.

mod basis_and_funding;
mod cointegration;
mod common;
mod critical_values;
mod pair_discovery;
mod prediction_market;
mod spread;
