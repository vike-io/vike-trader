//! Fixtures for the Avellaneda–Stoikov maker's tests.

use crate::{AsParams, HorizonMode, KappaMode, VarianceMode};

/// A deterministic A-S config, so every price the maker quotes is exact: `PureBernoulli` variance
/// (V = p(1−p), no σ warm-up), a constant horizon (no resolution blackout), a FIXED κ, `q_scale = 1`
/// (the position IS q_norm) and a one-tick standoff. Everything else is [`AsParams::default`]. The
/// tick grid is not here: the maker carries it (`with_quote_style`).
///
/// Shared by `vike-mm`'s pricing tests and `vike-sim`'s mounted backtest checks, each of which keeps
/// its own pinned quote numbers: a change here moves both crates' expectations at once.
pub fn as_test_params(gamma: f64) -> AsParams {
    AsParams {
        gamma,
        horizon_mode: HorizonMode::ConstantTau,
        variance_mode: VarianceMode::PureBernoulli,
        kappa_mode: KappaMode::Fixed,
        kappa_default: 50.0,
        q_scale: 1.0,
        min_standoff_ticks: 1.0,
        resolution_ts: None,
        resolution_blackout_ms: 0,
        ..AsParams::default()
    }
}
