use super::*;

fn close(a: f64, b: f64) {
    assert!((a - b).abs() <= 1e-12 * b.abs().max(1.0), "{a} vs {b}");
}

/// Build an equity curve (starting at 100.0) whose per-bar simple returns are exactly `rets`.
fn curve_from_returns(rets: &[f64]) -> Vec<f64> {
    crate::test_support::equity_from_returns(100.0, rets)
}

/// The convention pin for the helper the three DSR/PSR call sites share: `sr_per_obs` must be
/// the PER-PERIOD Sharpe (never annualized) and `kurt` must be NON-EXCESS. Asserted against
/// the `metrics::` primitives directly — the two footguns consolidated in the doc comment.
#[test]
fn sharpe_moments_are_per_period_and_non_excess() {
    let rets: Vec<f64> = (0..48).map(|i| 0.002 + 0.01 * ((i % 4) as f64 - 1.5)).collect();
    let curve = curve_from_returns(&rets);
    let m = sharpe_moments(&curve);

    // per-period Sharpe == mean/std, NOT the annualized metrics::sharpe.
    assert_eq!(m.sr_per_obs, metrics::risk_return_ratio(&curve));
    assert_eq!(m.n_obs, 48);
    assert_eq!(m.skew, metrics::returns_skewness(&curve));
    // kurt is rebased to non-excess (normal == 3) exactly once.
    assert_eq!(m.kurt, metrics::returns_kurtosis(&curve) + 3.0);

    // The annualized value is a DIFFERENT quantity, larger by ~sqrt(252) — feeding it to
    // overfit:: is the bug this helper exists to prevent (it saturates DSR to ~1.0).
    let annualized = metrics::sharpe(&curve, 252.0);
    assert!((annualized / 252.0_f64.sqrt() - m.sr_per_obs).abs() < 1e-12);
    assert!(annualized.abs() > m.sr_per_obs.abs() * 10.0, "annualized must be far larger");
    assert!(
        probabilistic_sharpe_ratio(annualized, m.n_obs, 0.0, m.skew, m.kurt) > 0.999,
        "the annualized wiring is the saturating one"
    );
}

/// `n_obs` is unclamped by design: a degenerate curve falls through to
/// `probabilistic_sharpe_ratio`'s own `n < 2` guard -> an honest 0.0.
#[test]
fn sharpe_moments_leave_a_degenerate_curve_to_the_psr_guard() {
    for curve in [vec![], vec![100.0], vec![100.0, 101.0]] {
        let m = sharpe_moments(&curve);
        assert!(m.n_obs < 2, "curve {curve:?} -> n_obs {}", m.n_obs);
        assert_eq!(probabilistic_sharpe_ratio(m.sr_per_obs, m.n_obs, 0.0, m.skew, m.kurt), 0.0);
    }
}

/// `psr_of_curve` is the PSR of the curve's own per-observation moments, and stays `0.0` on a
/// degenerate curve.
#[test]
fn psr_of_curve_is_the_psr_of_its_moments() {
    let curve = curve_from_returns(
        &(0..48).map(|i| 0.002 + 0.01 * ((i % 4) as f64 - 1.5)).collect::<Vec<_>>(),
    );
    let m = sharpe_moments(&curve);
    assert_eq!(
        psr_of_curve(&curve),
        probabilistic_sharpe_ratio(m.sr_per_obs, m.n_obs, 0.0, m.skew, m.kurt)
    );
    assert_eq!(psr_of_curve(&[100.0]), 0.0);
}

#[test]
fn psr_degenerate_and_known() {
    assert_eq!(probabilistic_sharpe_ratio(1.0, 1, 0.0, 0.0, 3.0), 0.0); // n<2
    // sr=0, n>=2, sharpe_star=0 -> Φ(0) = 0.5
    close(probabilistic_sharpe_ratio(0.0, 100, 0.0, 0.0, 3.0), 0.5);
    // skew=0, kurt=3, sharpe_star=0: PSR = Φ(sr*sqrt(n-1) / sqrt(1 + (kurt-1)/4*sr^2)).
    // NOTE the denom's (kurt-1)/4*sr^2 term only vanishes at sr=0 (already covered above),
    // not merely at kurt=3 (non-excess-normal) — skew=0 alone only kills the -skew*sr term.
    let sr = 0.5_f64;
    let denominator = (1.0 + 0.5 * sr * sr).sqrt();
    let expected = normal_cdf_complementary_form(sr * (100.0 - 1.0f64).sqrt() / denominator);
    close(probabilistic_sharpe_ratio(0.5, 100, 0.0, 0.0, 3.0), expected);
}

#[test]
fn expected_max_sharpe_degenerate() {
    assert_eq!(expected_max_sharpe(0.1, 1), 0.0); // n_trials < 2
    assert_eq!(expected_max_sharpe(0.0, 5), 0.0); // var <= 0
    assert!(expected_max_sharpe(0.25, 10) > 0.0); // grows with trials
}

#[test]
fn deflated_sharpe_below_psr_when_many_trials() {
    // Same observed SR, but benchmarking against expected-max of 20 varied trials
    // deflates the significance below the raw PSR (sharpe_star > 0).
    let trials: Vec<f64> = (0..20).map(|i| 0.1 + 0.02 * i as f64).collect();
    let dsr = deflated_sharpe_ratio(0.8, &trials, 250, 0.0, 3.0);
    let psr = probabilistic_sharpe_ratio(0.8, 250, 0.0, 0.0, 3.0);
    assert!(dsr < psr, "dsr {dsr} should be < psr {psr}");
    assert!((0.0..=1.0).contains(&dsr));
}

#[test]
fn pbo_requires_even_splits_and_flags_nonfinite() {
    let m = vec![vec![0.1, 0.2], vec![0.1, 0.2]];
    assert!(pbo_cscv(&m, 2).is_finite() || pbo_cscv(&m, 2).is_nan());
    let bad = vec![vec![f64::NAN, 0.2], vec![0.1, 0.2]];
    assert!(pbo_cscv(&bad, 2).is_nan());
}

#[test]
#[should_panic(expected = "even")]
fn pbo_odd_splits_panics() {
    let m = vec![vec![0.1, 0.2]; 4];
    let _ = pbo_cscv(&m, 3);
}

#[test]
fn effective_n_collapses_correlated_trials() {
    // Two identical series -> effective N ~ 1; two anti-correlated -> ~2.
    let same = vec![vec![1.0, 2.0, 3.0, 4.0], vec![1.0, 2.0, 3.0, 4.0]];
    assert!((effective_n_trials(&same) - 1.0).abs() < 1e-9);
    let anti = vec![vec![1.0, 2.0, 3.0, 4.0], vec![4.0, 3.0, 2.0, 1.0]];
    assert!((effective_n_trials(&anti) - 2.0).abs() < 1e-9); // max(neg_corr,0)=0 -> N
    assert_eq!(effective_n_trials(&[]), 0.0);
}

#[test]
fn verdict_levels_and_nan_pbo() {
    // clean: low pbo, high dsr, consistent -> Low
    assert_eq!(overfit_verdict(0.1, 0.95, Some(0.8)).level, OverfitLevel::Low);
    // pbo>0.5 (2) + dsr<0.5 (2) -> High
    assert_eq!(overfit_verdict(0.6, 0.3, None).level, OverfitLevel::High);
    // NaN pbo is surfaced, not treated as "no overfit"
    let v = overfit_verdict(f64::NAN, 0.95, None);
    assert!(v.reasons.iter().any(|r| r.contains("not assessed")));
}

// --- selection audit (multiple-testing discipline) ---

/// Deterministic pseudo-random return series (no `rand` dep, no seeded-RNG drift):
/// a hash-mixed LCG per candidate so the trials are uncorrelated but reproducible.
fn synth_trial(seed: u64, n: usize, drift: f64) -> Vec<f64> {
    let mut s = seed.wrapping_mul(6_364_136_223_846_793_005).wrapping_add(1);
    (0..n)
        .map(|_| {
            s = s.wrapping_mul(6_364_136_223_846_793_005).wrapping_add(1_442_695_040_888_963_407);
            let u = ((s >> 11) as f64) / ((1u64 << 53) as f64); // [0,1)
            (u - 0.5) * 0.02 + drift
        })
        .collect()
}

fn sharpe_of(returns: &[f64]) -> f64 {
    let n = returns.len() as f64;
    let mean = returns.iter().sum::<f64>() / n;
    let var = returns.iter().map(|r| (r - mean) * (r - mean)).sum::<f64>() / n;
    if var <= 0.0 {
        return 0.0;
    }
    mean / var.sqrt()
}

/// THE TRAP, DEMONSTRATED — not merely documented.
///
/// Twenty candidates, of which exactly one carries a small drift so it wins the
/// selection with an unambiguously POSITIVE Sharpe (keeping both PSRs off the
/// 0.0 floor, so the comparison below is strict and deterministic rather than
/// dependent on which way a zero-drift sample happened to fall).
///
/// Auditing that winner ALONE (the survivors-only mistake) reports a materially
/// higher deflated Sharpe than auditing it against the full set it was drawn from
/// — because a single trial has zero trial-variance, so `expected_max_sharpe`
/// benchmarks it against 0 instead of against the best-of-20 a selection implies.
/// If this assertion ever flips, the gates have stopped accounting for selection.
#[test]
fn auditing_only_the_survivor_inflates_significance() {
    let n_obs = 250usize;
    let returns: Vec<Vec<f64>> =
        (0..20).map(|i| synth_trial(i as u64, n_obs, if i == 7 { 0.0012 } else { 0.0 })).collect();
    let sharpes: Vec<f64> = returns.iter().map(|r| sharpe_of(r)).collect();
    let best = sharpes
        .iter()
        .enumerate()
        .max_by(|a, b| a.1.partial_cmp(b.1).unwrap())
        .map(|(i, _)| i)
        .unwrap();

    let full = audit_selection(&sharpes, &returns, best, 42, n_obs, 0.0, 3.0, 8).unwrap();
    let survivor_only =
        audit_selection(&sharpes[best..=best], &returns[best..=best], 0, 42, n_obs, 0.0, 3.0, 8)
            .unwrap();

    assert_eq!(best, 7, "the drifted candidate must win the selection");
    assert!(full.selected_sharpe > 0.0, "winner Sharpe must be positive: {}", full.selected_sharpe);
    assert_eq!(full.n_candidates, 20);
    assert_eq!(survivor_only.n_candidates, 1);
    assert!(
        full.deflated_sharpe < survivor_only.deflated_sharpe,
        "auditing the full candidate set must NOT be more permissive than auditing the \
             survivor alone: full {} vs survivor-only {}",
        full.deflated_sharpe,
        survivor_only.deflated_sharpe
    );
}

/// A survivors-only audit must SAY so — the failure is silent otherwise.
#[test]
fn a_single_candidate_audit_warns_that_selection_is_unaccounted() {
    let r = vec![synth_trial(7, 120, 0.0005)];
    let s = vec![sharpe_of(&r[0])];
    let a = audit_selection(&s, &r, 0, 10, 120, 0.0, 3.0, 4).unwrap();
    assert_eq!(a.n_candidates, 1);
    assert!(
        a.verdict.reasons.iter().any(|m| m.contains("Only ONE candidate")),
        "expected an explicit single-candidate warning, got {:?}",
        a.verdict.reasons
    );
}

/// Trade count rides alongside Sharpe: this family starves before it loses money.
#[test]
fn trade_count_is_reported_with_the_verdict() {
    let returns: Vec<Vec<f64>> = (0..4).map(|i| synth_trial(i as u64, 100, 0.0)).collect();
    let sharpes: Vec<f64> = returns.iter().map(|r| sharpe_of(r)).collect();
    let a = audit_selection(&sharpes, &returns, 1, 3, 100, 0.0, 3.0, 4).unwrap();
    assert_eq!(a.selected_trades, 3, "a 3-trade winner must surface its trade count");
    assert!(a.effective_trials >= 1.0 && a.effective_trials <= 4.0);
}

/// An unauditable selection is not a passing one.
#[test]
fn inconsistent_inputs_are_none() {
    let r = vec![synth_trial(1, 50, 0.0)];
    let s = vec![0.5, 0.7]; // length mismatch
    assert!(audit_selection(&s, &r, 0, 1, 50, 0.0, 3.0, 4).is_none());
    assert!(audit_selection(&[], &[], 0, 1, 50, 0.0, 3.0, 4).is_none());
    let s1 = vec![0.5];
    assert!(
        audit_selection(&s1, &r, 5, 1, 50, 0.0, 3.0, 4).is_none(),
        "selected index out of range"
    );
}

/// **`audit_selection` hands `pbo_cscv` the T × N matrix that function documents.** One trial beats
/// the other at every observation, so the in-sample winner wins out of sample in every split: PBO
/// 0. The same series read the wrong way round (each trial a row of "observations") answer 1.0,
/// which is what the audit reported until 2026-11-09.
#[test]
fn the_selection_audit_reads_trials_as_columns() {
    let trials = [vec![0.010, 0.005, 0.004], vec![0.000, 0.002, 0.003]];
    let by_obs: Vec<Vec<f64>> = (0..3).map(|t| vec![trials[0][t], trials[1][t]]).collect();
    assert_eq!(pbo_cscv(&by_obs, 2), 0.0, "T × N: the dominant trial never overfits");
    assert_eq!(pbo_cscv(&trials, 2), 1.0, "the transposed reading answers the opposite");
    let audit = audit_selection(&[1.0, 0.5], &trials, 0, 3, 3, 0.0, 3.0, 2).unwrap();
    assert_eq!(audit.pbo, 0.0, "the audit reads trials as columns");
    let ragged = [vec![0.01], vec![0.0, 0.1]];
    assert!(audit_selection(&[1.0, 0.5], &ragged, 0, 1, 2, 0.0, 3.0, 2).is_none(), "not a matrix");
}

/// The minimum track record length by hand (Bailey & López de Prado 2012): a per-observation Sharpe of
/// 0.1 against 0 with normal returns needs `1 + (1 + 0.5·0.01)·(Φ⁻¹(0.95)/0.1)²` ≈ 272.9 observations at
/// 95%; asking for 80% power adds `Φ⁻¹(0.8)` to the numerator.
#[test]
fn the_minimum_track_record_length_by_hand() {
    let (z95, z80) = (1.6448536269514722, 0.8416212335729143);
    let at_half = minimum_track_record_length(0.1, 0.0, 0.0, 3.0, 0.95, 0.5);
    close(at_half, 1.0 + 1.005 * (z95 / 0.1) * (z95 / 0.1));
    assert!((at_half - 272.907).abs() < 1e-3, "{at_half}");
    let powered = minimum_track_record_length(0.1, 0.0, 0.0, 3.0, 0.95, 0.8);
    close(powered, 1.0 + 1.005 * ((z95 + z80) / 0.1) * ((z95 + z80) / 0.1));
}

/// At power one half it inverts the PSR: a record one observation shorter than the length falls short
/// of the confidence, a record of the length (rounded up) reaches it. Skewed, fat-tailed returns and a
/// non-zero benchmark included.
#[test]
fn the_minimum_track_record_length_inverts_the_probabilistic_sharpe_ratio() {
    for (sharpe, star, skew, kurt, confidence) in
        [(0.1, 0.0, 0.0, 3.0, 0.95), (0.08, 0.02, -0.7, 6.5, 0.9), (0.3, 0.1, 0.4, 4.0, 0.99)]
    {
        let n = minimum_track_record_length(sharpe, star, skew, kurt, confidence, 0.5);
        let psr = |n: f64| probabilistic_sharpe_ratio(sharpe, n as usize, star, skew, kurt);
        assert!(psr(n.ceil()) >= confidence, "n {n}: {} at its ceiling", psr(n.ceil()));
        assert!(psr(n.floor() - 1.0) < confidence, "n {n}: one shorter reaches it");
    }
}

/// More confidence, more power, fatter tails, a narrower gap: each needs a longer record. A Sharpe BELOW
/// the benchmark is told apart just as well (a record showing a forward test is worse); equal Sharpes
/// never part; a probability outside (0, 1) or a non-finite input is no question.
#[test]
fn the_minimum_track_record_length_grows_with_what_is_asked() {
    let n = |sharpe: f64, kurt: f64, confidence: f64, power: f64| {
        minimum_track_record_length(sharpe, 0.0, 0.0, kurt, confidence, power)
    };
    let base = n(0.1, 3.0, 0.95, 0.8);
    assert!(n(0.1, 3.0, 0.99, 0.8) > base);
    assert!(n(0.1, 3.0, 0.95, 0.9) > base);
    assert!(n(0.1, 9.0, 0.95, 0.8) > base);
    assert!(n(0.05, 3.0, 0.95, 0.8) > base);
    let below = minimum_track_record_length(0.0, 0.1, 0.0, 3.0, 0.95, 0.8);
    assert!(below.is_finite() && below > 1.0, "{below}");
    assert_eq!(minimum_track_record_length(0.1, 0.1, 0.0, 3.0, 0.95, 0.8), f64::INFINITY);
    assert!(minimum_track_record_length(0.1, 0.0, 0.0, 3.0, 1.0, 0.8).is_nan());
    assert!(minimum_track_record_length(0.1, 0.0, 0.0, 3.0, 0.95, 0.0).is_nan());
    assert!(minimum_track_record_length(f64::NAN, 0.0, 0.0, 3.0, 0.95, 0.8).is_nan());
}
