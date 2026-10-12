//! Deflated Sharpe: within one batch, against prior ledger trials, and the per-period units pin.

use vike_analytics::{metrics, overfit};

use super::support::{
    CROSS, PPY, accepted_with_curve, cross_script, curve_from_returns, prior_accepted,
    run_with_ledger, seeded, submit,
};
use crate::agent::deflate::deflate_batch;
use crate::agent::{AgentResult, develop_strategies, develop_strategy};
use crate::client::{FakeClient, FakeTurn};
use crate::ledger::trial::TrialLedger;
// The module itself here (no local named `ledger` in this scope) — the tests drive the ledger
// files directly to set up prior state.
use crate::ledger::{self, LedgerPaths};

#[test]
fn develop_strategies_deflates_every_accepted_candidate() {
    let (_d, store) = seeded();
    // Three DIFFERENT sma-cross variants over the same seeded data -> different OOS Sharpes,
    // giving the trial set real variance (not a degenerate all-identical case).
    let variants = [(5u32, 20u32), (8, 25), (3, 15)];
    let mut turns = Vec::new();
    for (i, (fast, slow)) in variants.iter().enumerate() {
        turns.push(submit(&cross_script(*fast, *slow)));
        turns.push(FakeTurn::Text(format!("done{i}")));
    }
    let client = FakeClient::new(turns);

    let results =
        develop_strategies("x", "binance", "BTCUSDT", "1m", store.as_ref(), &client, 3, 2, 0.3);
    assert_eq!(results.len(), 3);

    let accepted: Vec<&AgentResult> = results.iter().filter(|r| r.accepted).collect();
    assert!(
        accepted.len() >= 2,
        "test setup must produce >=2 accepted trials for deflation to run at all"
    );

    let trial_sharpes: Vec<f64> = accepted.iter().map(|r| r.oos_sharpe).collect();
    assert!(
        trial_sharpes.iter().any(|&s| (s - trial_sharpes[0]).abs() > 1e-9),
        "test setup must give the trial set real variance, not all-identical sharpes"
    );

    let best = accepted.iter().max_by(|a, b| a.oos_sharpe.total_cmp(&b.oos_sharpe)).unwrap();
    assert!(best.deflated_sharpe.is_finite());
    assert!((0.0..=1.0).contains(&best.deflated_sharpe));
    assert_ne!(
        best.deflated_sharpe, 0.0,
        "deflated_sharpe must actually be computed, not left at the Default"
    );

    // overfit.rs's guarantee: benchmarking PSR against the trials' expected-max-Sharpe can only
    // match or REDUCE significance vs. a plain zero-benchmark PSR (deflated_sharpe_below_psr_
    // when_many_trials in overfit.rs). Recompute that same zero-benchmark PSR here and check
    // the direction holds for the winner. Both must use the PER-PERIOD Sharpe
    // (risk_return_ratio), same units DSR is computed in — feeding the annualized oos_sharpe
    // here would make raw_psr saturate to ~1.0 and the direction check vacuous.
    let observed_pp = metrics::risk_return_ratio(&best.oos_equity_curve);
    let n_obs = metrics::returns(&best.oos_equity_curve).len().max(2);
    let skew = metrics::returns_skewness(&best.oos_equity_curve);
    let kurt = 3.0 + metrics::returns_kurtosis(&best.oos_equity_curve);
    let raw_psr = overfit::probabilistic_sharpe_ratio(observed_pp, n_obs, 0.0, skew, kurt);
    assert!(
        best.deflated_sharpe <= raw_psr + 1e-9,
        "deflated {} should not exceed the plain zero-benchmark PSR {raw_psr}",
        best.deflated_sharpe
    );
}

/// MAGNITUDE pin (unit test on `deflate_batch` directly, overfit.rs's own test style): a fixed,
/// hand-reasonable equity curve whose deflated Sharpe is pinned to a known per-period value.
/// This is the regression guard the reviewer asked for — it FAILS if DSR is computed from the
/// annualized Sharpe (which saturates to ~1.0) instead of the per-period one.
#[test]
fn deflate_batch_pins_per_period_dsr_magnitude() {
    // Two deterministic candidates with different, modest per-period Sharpes (positive drift +
    // deterministic zig-zag noise). 48 returns each -> n_obs=48, enough for skew/kurt.
    let rets_a: Vec<f64> = (0..48).map(|i| 0.002 + 0.01 * ((i % 4) as f64 - 1.5)).collect();
    let rets_b: Vec<f64> = (0..48).map(|i| 0.001 + 0.008 * ((i % 3) as f64 - 1.0)).collect();
    let curve_a = curve_from_returns(&rets_a);
    let curve_b = curve_from_returns(&rets_b);

    // The correct per-period inputs the fixed implementation must feed overfit:: with.
    let pp_a = metrics::risk_return_ratio(&curve_a);
    let pp_b = metrics::risk_return_ratio(&curve_b);
    let n_obs = metrics::returns(&curve_a).len().max(2); // = 48
    let skew = metrics::returns_skewness(&curve_a);
    let kurt = 3.0 + metrics::returns_kurtosis(&curve_a);
    let expected = overfit::deflated_sharpe_ratio(pp_a, &[pp_a, pp_b], n_obs, skew, kurt);
    // What the OLD (buggy) wiring computed: annualized sr fed where per-period is required.
    let ann_a = metrics::sharpe(&curve_a, PPY);
    let ann_b = metrics::sharpe(&curve_b, PPY);
    let buggy = overfit::deflated_sharpe_ratio(ann_a, &[ann_a, ann_b], n_obs, skew, kurt);

    let mut results = vec![accepted_with_curve(curve_a), accepted_with_curve(curve_b)];
    deflate_batch(&mut results);

    // (1) The implementation must match the per-period computation exactly.
    assert!(
        (results[0].deflated_sharpe - expected).abs() < 1e-12,
        "deflated {} must equal the per-period DSR {expected}",
        results[0].deflated_sharpe
    );
    // (2) Pinned magnitude: this curve's per-period DSR sits well inside the unit interval and
    //     is NOT saturated. Value pinned from the fixed wiring; the buggy annualized wiring
    //     saturates to ~1.0, so this bound is what makes the test a real regression catcher.
    assert!(
        (results[0].deflated_sharpe - 0.874_192).abs() < 1e-5,
        "pinned per-period DSR magnitude drifted: {}",
        results[0].deflated_sharpe
    );
    // (3) The buggy annualized wiring would have saturated near 1.0 -> materially different, so
    //     assertions (1)/(2) genuinely distinguish the two unit conventions.
    assert!(
        buggy > 0.999 && (buggy - results[0].deflated_sharpe).abs() > 0.1,
        "annualized wiring should saturate (~1.0); got buggy={buggy}, actual={}",
        results[0].deflated_sharpe
    );
}

#[test]
fn develop_strategies_leaves_deflated_sharpe_zero_below_two_accepted() {
    let (_d, store) = seeded();
    // Only 1 candidate ever accepts (the other gives up) -> deflation is undefined, must stay
    // at the Default 0.0 rather than compute something from a single-element trial set.
    let client = FakeClient::new(vec![
        submit(CROSS),
        FakeTurn::Text("done0".into()),
        submit("fn on_bar( {"),
        submit("still broken ("),
        FakeTurn::Text("gave up".into()),
    ]);
    let results =
        develop_strategies("x", "binance", "BTCUSDT", "1m", store.as_ref(), &client, 2, 2, 0.3);
    assert_eq!(results.iter().filter(|r| r.accepted).count(), 1);
    for r in &results {
        assert_eq!(r.deflated_sharpe, 0.0);
    }
}

/// **The acceptance test for the whole trial-ledger item.** One accepted candidate — the
/// production shape, which before this could never reach the ≥2 trials deflation needs — plus
/// 4 prior accepted trials recorded on the same slice, must produce a real `deflated_sharpe`.
/// The bare (ledger-less) control run over the same data still reports `0.0`, which is exactly
/// the bug: the single most important overfitting control was inert on the production path.
#[test]
fn deflation_uses_prior_trials() {
    let (d, store) = seeded();
    let paths = LedgerPaths::under(d.path());
    // Four prior trials with REAL spread — a zero-variance trial set yields sr_star == 0 and
    // would make this test pass without the prior half being consulted at all.
    let prior = TrialLedger {
        trials: vec![
            prior_accepted(0, 0.02, 0.31),
            prior_accepted(1, 0.05, 0.79),
            prior_accepted(2, 0.03, 0.47),
            prior_accepted(3, 0.09, 1.42),
        ],
    };
    ledger::store::save_trials(&prior, &paths.trials);

    let client = FakeClient::new(vec![submit(CROSS), FakeTurn::Text("done".into())]);
    let r = run_with_ledger("make an sma cross", store.as_ref(), &client, &paths);
    assert!(r.accepted);
    assert!(r.deflated_sharpe.is_finite());
    assert!((0.0..=1.0).contains(&r.deflated_sharpe));
    assert_ne!(
        r.deflated_sharpe, 0.0,
        "a single accepted candidate + prior trials must deflate — this is the whole item"
    );

    // Exact: the trial set is {this candidate} ∪ {the 4 prior}, in that order.
    let m = overfit::sharpe_moments(&r.oos_equity_curve);
    let mut set = vec![m.sr_per_obs];
    set.extend(prior.trials.iter().map(|t| t.sr_per_obs));
    let expected = overfit::deflated_sharpe_ratio(m.sr_per_obs, &set, m.n_obs, m.skew, m.kurt);
    assert_eq!(r.deflated_sharpe, expected, "prior trials must join the trial set verbatim");

    // The run is appended (with its deflated value) — the NEXT session's prior set.
    let after = ledger::store::load_trials(&paths.trials);
    assert_eq!(after.trials.len(), 5, "the run's own trial is recorded");
    let last = after.trials.last().unwrap();
    assert!(last.accepted);
    assert_eq!(last.deflated_sharpe, expected);
    assert_eq!(last.sr_per_obs, m.sr_per_obs, "the deflation scalar survives curve pruning");
    assert_eq!(after.prior_sharpes_for("binance", "BTCUSDT", "1m").len(), 5);

    // Control — the pre-ledger path over the SAME data reports nothing.
    let bare_client = FakeClient::new(vec![submit(CROSS), FakeTurn::Text("done".into())]);
    let bare = develop_strategy(
        "make an sma cross",
        "binance",
        "BTCUSDT",
        "1m",
        store.as_ref(),
        &bare_client,
        2,
        0.3,
    );
    assert!(bare.accepted);
    assert_eq!(bare.deflated_sharpe, 0.0, "ledger-less is byte-identical to pre-ledger");
}

/// A prior trial on a DIFFERENT slice must not widen this slice's multiple-testing correction
/// — the trial set is per-`(venue, symbol, interval)`, not global.
#[test]
fn deflation_ignores_other_slices() {
    let (d, store) = seeded();
    let paths = LedgerPaths::under(d.path());
    let mut t = prior_accepted(0, 0.05, 0.79);
    t.symbol = "ETHUSDT".into();
    ledger::store::save_trials(&TrialLedger { trials: vec![t] }, &paths.trials);

    let client = FakeClient::new(vec![submit(CROSS), FakeTurn::Text("done".into())]);
    let r = run_with_ledger("x", store.as_ref(), &client, &paths);
    assert!(r.accepted);
    assert_eq!(r.deflated_sharpe, 0.0, "1 candidate + 0 same-slice priors -> still undefined");
}
