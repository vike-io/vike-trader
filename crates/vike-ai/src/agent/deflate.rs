//! Cross-session deflation: each accepted candidate's Sharpe against this call's and the ledger's trials.

use vike_analytics::overfit;

use super::AgentResult;

/// Deflate every ACCEPTED candidate's `oos_sharpe` in place against the shared trial set — the
/// search that was ACTUALLY performed against this slice: every accepted candidate in THIS call,
/// plus `prior_sharpes`, the per-period Sharpes of accepted trials recorded on the same slice in
/// earlier calls/sessions ([`crate::ledger::trial::TrialLedger::prior_sharpes_for`]).
///
/// **Why the prior half matters.** Bailey & López de Prado's correction is a multiple-testing
/// correction: `sr_star` is the expected MAXIMUM Sharpe across `N` trials, so it is only honest if
/// `N` counts every trial run against that slice. Before the ledger, `N` was "candidates that
/// happened to share one process", which for the production path (`develop_strategy_with_ledger`,
/// one candidate) was 1 — deflation undefined, `deflated_sharpe` permanently `0.0`. A slice you have
/// hammered with 200 attempts across 40 sessions deserves a far higher bar than a fresh one, and
/// only the ledger knows that.
///
/// Trial set: per-period Sharpes only. Per-candidate skew/kurtosis/observation-count are derived
/// from THAT candidate's own `oos_equity_curve`; a prior trial contributes ONLY its scalar
/// `sr_per_obs`, which is exactly why [`crate::ledger::trial::Trial`] persists that scalar and can
/// therefore prune the curve without shrinking the correction.
///
/// ORDER: this call's candidates first (in slice order), then `prior_sharpes` in ledger append
/// order. `sample_variance` folds f64s in that order, so it is fixed rather than incidental — and
/// an empty `prior_sharpes` reproduces the pre-ledger fold bit-for-bit.
///
/// UNITS: every statistical input comes from `overfit::sharpe_moments`, the shared derivation
/// helper that owns both load-bearing conventions (per-period — NOT annualized — Sharpe, and
/// non-excess kurtosis) and documents why each is a footgun; the annualized-Sharpe one shipped as
/// a real bug here once, pinned below by `deflate_batch_pins_per_period_dsr_magnitude`. Read that
/// helper's doc before touching this. The displayed `oos_sharpe` field stays annualized
/// (`metrics::sharpe` at `vike_analytics::report::periods_per_year_for_interval` of the slice's own
/// interval — it used to be a hardcoded 252 at every interval, which understated an intraday
/// Copilot Sharpe by `sqrt(24)` on 1h and `sqrt(1440)` on 1m) — a different quantity, for display
/// only, that must never reach `overfit::`. Using `sharpe_moments` for BOTH the observed value AND
/// every trial keeps the
/// observed value, the trial set, and the derived `sr_star` internally consistent.
///
/// Fewer than 2 trials in TOTAL -> deflation is undefined (no variance across trials to estimate
/// the expected-max-Sharpe benchmark from): every result's `deflated_sharpe` is left at its
/// `Default` `0.0` rather than computed, NaN, or panicking.
pub fn deflate_with_prior(results: &mut [AgentResult], prior_sharpes: &[f64]) {
    let mut trial_sharpes: Vec<f64> = results
        .iter()
        .filter(|r| r.accepted)
        .map(|r| overfit::sharpe_moments(&r.oos_equity_curve).sr_per_obs)
        .collect();
    // Prior rows are already finiteness-filtered by `prior_sharpes_for`; this call's candidates
    // are not, but a non-finite one here would equally have poisoned the pre-ledger fold, so the
    // behavior is deliberately left unchanged.
    trial_sharpes.extend_from_slice(prior_sharpes);
    if trial_sharpes.len() < 2 {
        return;
    }
    for r in results.iter_mut().filter(|r| r.accepted) {
        let m = overfit::sharpe_moments(&r.oos_equity_curve);
        r.deflated_sharpe =
            overfit::deflated_sharpe_ratio(m.sr_per_obs, &trial_sharpes, m.n_obs, m.skew, m.kurt);
    }
}

/// [`deflate_with_prior`] with no prior trials — the pre-ledger, within-one-batch deflation, kept
/// as a named entry point because that is the contract every existing test pins.
#[cfg(test)]
pub(super) fn deflate_batch(results: &mut [AgentResult]) {
    deflate_with_prior(results, &[]);
}
