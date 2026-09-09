//! Scores over predictions. Pure functions, no model and no training involved.

use vike_model::py_sum;

/// How far from 0 and 1 a probability is clamped before its logarithm is taken.
///
/// Without it a confidently wrong prediction scores `+inf`, and every comparison against infinity
/// is meaningless: a search would rank two catastrophic candidates as equal. `1e-15` is the value
/// LightGBM's own metric and scikit-learn's `log_loss` both use.
pub const LOGLOSS_EPS: f64 = 1e-15;

/// Mean binary cross-entropy: `-mean(y ln p + (1 - y) ln(1 - p))`, lower is better.
///
/// Returns `NaN` for an empty input rather than `0.0` — zero is the BEST possible score, so an
/// empty validation fold would otherwise win a grid search outright.
///
/// A length mismatch scores over the shorter overlap rather than refusing: the signature is
/// infallible (`f64`, not `Result`), so there is no channel to refuse through, and truncating to
/// the aligned prefix is the least-surprising infallible answer — it never fabricates a label or a
/// probability for the unmatched tail, which padding would.
///
/// ⚠ The sum is [`vike_model::py_sum`], the workspace's Neumaier-compensated fold, and this is the
/// ONLY place in this crate that uses it. Adding hundreds of small logs in arrival order is exactly
/// the case compensation exists for, and the resulting score *would* decide which grid point wins.
/// (*Would* — see the caller paragraph at the bottom; this sentence used to be stated flatly, as if
/// naming a live search loop, and no such loop calls this function.) The TREE WALK in
/// [`crate::infer`] deliberately does the opposite — a plain fold — because there it must reproduce
/// LightGBM's own accumulation bit for bit. Two sums, two rules, both deliberate.
///
/// ⚠ **Both logarithms are `libm::log`, NOT `f64::ln`, and that substitution MOVED this function's
/// output on every input — on Linux as well as on Windows.** IEEE 754 requires `+`, `-`, `*`, `/`
/// and `sqrt` to be correctly rounded and requires NOTHING of `ln`, so an `f64` METHOD call reaches
/// whichever libm the platform happens to ship: MSVC's CRT on the Windows dev box, glibc on the CI box
/// and on every CI runner. The two disagree in the last bit across most of this function's domain
/// — `crates/vike-analytics/tests/libm_platform_probe.rs` measured its `ln/*` sweeps diverging on
/// 0.07%–10% of samples, glibc against the `libm` crate — and a log-loss is a number two grid
/// points are RANKED by, so a last-bit difference is a different winner at a tie. The `libm` crate
/// is the same source on both boxes.
/// `docs/decisions/0032-transcendentals-come-from-the-libm-crate-not-the-platform.md` is the
/// verdict; its Consequences section names this crate among the six not yet converted, and these
/// two sites — here and [`crate::infer`]'s `GbdtModel::predict_proba` — are that entry's whole
/// share. Nothing here is `sqrt`, which never needed converting.
///
/// ⚠ **Nothing in this workspace CALLS this function**, and that is worth saying plainly next to a
/// conversion that moved its numbers, because the paragraph above reads like a report from a live
/// search. `grep -rn binary_logloss --include=*.rs crates/` finds this definition and this module's
/// own tests, and nothing else. The one other hit in the tree is a SEPARATE function of the same
/// name in gitignored `user_data/research/studies/rust/cohort/search.rs` — different argument order
/// (`proba, y`), different epsilon (`1e-6`, not [`LOGLOSS_EPS`]), different empty-input answer
/// (`+inf`, not `NaN`), and its own naive fold rather than [`vike_model::py_sum`]. It does not call
/// this one. So this is LATENT PUBLIC-API SURFACE: no caller's fixtures move when the arithmetic
/// underneath it does, and the first caller will be measuring `libm`'s answer from its first run.
pub fn binary_logloss(labels: &[f32], probs: &[f64]) -> f64 {
    let n = labels.len().min(probs.len());
    if n == 0 {
        return f64::NAN;
    }
    let terms = (0..n).map(|i| {
        let p = probs[i].clamp(LOGLOSS_EPS, 1.0 - LOGLOSS_EPS);
        let y = f64::from(labels[i]);
        -(y * libm::log(p) + (1.0 - y) * libm::log(1.0 - p))
    });
    py_sum(terms) / n as f64
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_perfect_prediction_scores_essentially_zero() {
        let loss = binary_logloss(&[1.0, 0.0], &[1.0, 0.0]);
        assert!((0.0..1e-14).contains(&loss), "got {loss}");
    }

    #[test]
    fn an_uninformative_half_scores_ln_two() {
        let loss = binary_logloss(&[1.0, 0.0, 1.0], &[0.5, 0.5, 0.5]);
        assert!((loss - std::f64::consts::LN_2).abs() < 1e-12, "got {loss}");
    }

    #[test]
    fn a_confidently_wrong_prediction_is_large_but_finite() {
        // Without the clamp this is +inf, and every comparison against it is then meaningless:
        // a grid search would rank two catastrophic points as equal.
        let loss = binary_logloss(&[1.0], &[0.0]);
        assert!(loss.is_finite(), "got {loss}");
        assert!(loss > 30.0, "got {loss}");
    }

    #[test]
    fn a_lower_score_means_a_better_prediction() {
        let better = binary_logloss(&[1.0, 0.0], &[0.9, 0.1]);
        let worse = binary_logloss(&[1.0, 0.0], &[0.6, 0.4]);
        assert!(better < worse);
    }

    #[test]
    fn an_empty_input_has_no_score_rather_than_a_zero_one() {
        // Zero would be the BEST possible score, so an empty fold would win a grid search.
        assert!(binary_logloss(&[], &[]).is_nan());
    }

    #[test]
    fn mismatched_lengths_score_over_the_overlap_only() {
        assert_eq!(binary_logloss(&[1.0, 0.0], &[0.5]), binary_logloss(&[1.0], &[0.5]));
    }

    #[test]
    fn py_sum_recovers_what_a_naive_fold_would_silently_drop() {
        // All six tests above pass unchanged if `py_sum(terms)` in binary_logloss becomes
        // `terms.sum()` — every case there has 1-3 terms of comparable magnitude, where a
        // compensated and a naive fold agree bit for bit. This case is built to disagree: one term
        // big enough to swallow eight ~1e-15 terms individually — the ulp near 34.5388 is ≈7.1e-15,
        // more than double any one of them, so each addition alone rounds away completely — but not
        // their SUM, since eight together (~8e-15) exceed half a ulp. A naive left fold over these
        // terms, in this order, returns the big term's own value UNCHANGED — as if the eight small
        // terms had never been added at all. `py_sum`'s Neumaier compensation tracks the lost bits
        // and folds them back in as one final correction, landing one ulp higher.
        //
        // ⚠ THE TERM EXPRESSION BELOW IS A DUPLICATE of `binary_logloss`'s own, deliberately: the
        // last assertion in this test ties production's output to `py_sum`'s output over these
        // exact terms, which only means anything while the two copies compute the same thing. So
        // they convert TOGETHER — both call `libm::log` now — and the moment one of them says
        // `.ln()` again this test is pinning a different function from the one it is measuring.
        //
        // ⚠ THE TWO CONSTANTS BELOW WERE RE-MEASURED 2026-08-26, after the term moved from
        // `p.ln()` to `libm::log(p)`. They were NOT retyped from the pre-conversion pair: this
        // test's whole subject is a value near `1e-15`, and whether `libm::log` and the platform's
        // `ln` agree there is exactly the question ADR 0032 says has to be RUN rather than read.
        // The slots briefly held the undefined identifier `RECORD` — a compile error naming
        // itself, rather than a stale literal that looks measured — until the numbers came off a
        // real run.
        //
        // ⚠ **The measured pair is IDENTICAL to the pre-conversion one**, and that is a result
        // rather than a formality: `libm::log(1e-15)` returns the same bits as `f64::ln(1e-15)` on
        // this box, so the conversion moved nothing here. It cost one run to know that instead of
        // assuming it, and the assumption would have been right for the wrong reason — the pair
        // agreeing on THIS platform says nothing about the other, which is why the values are
        // pinned rather than trusted.
        //
        // To re-record after a future change: put `0.0` in each slot, run
        // `just t vike-ml py_sum_recovers_what_a_naive_fold_would_silently_drop`, and paste the
        // `left:` value each failed assertion prints. `f64`'s `Debug` is the shortest
        // round-tripping decimal, so the printed text re-parses to exactly those bits. The two
        // values must still differ by one ulp — that is what `assert_ne!` below is for, and it is
        // the property this test exists to prove; if a conversion ever collapses them, that is a
        // finding about the new function's value near `1e-15`, not a number to force.
        let mut labels = vec![1.0f32];
        let mut probs = vec![0.0f64]; // clamps to LOGLOSS_EPS: term = -ln(1e-15) ≈ 34.5388
        for _ in 0..8 {
            labels.push(0.0);
            probs.push(1e-15); // term = -ln(1 - 1e-15) ≈ 1e-15
        }
        let n = labels.len();
        let terms: Vec<f64> = (0..n)
            .map(|i| {
                let p = probs[i].clamp(LOGLOSS_EPS, 1.0 - LOGLOSS_EPS);
                let y = f64::from(labels[i]);
                -(y * libm::log(p) + (1.0 - y) * libm::log(1.0 - p))
            })
            .collect();

        let naive: f64 = terms.iter().copied().sum();
        assert_eq!(naive, 34.538776394910684, "a naive left fold drops all eight small terms");

        let compensated = py_sum(terms.iter().copied());
        assert_eq!(
            compensated, 34.53877639491069,
            "py_sum recovers the eight dropped terms as one final correction"
        );
        assert_ne!(
            compensated, naive,
            "if these ever compare equal, py_sum degenerated to a plain fold on this input"
        );

        // Ties binary_logloss's actual output to py_sum's output on these exact terms — a refactor
        // to `terms.sum()` inside binary_logloss would make this fail against `compensated`, even
        // though every OTHER test in this module would stay green.
        assert_eq!(binary_logloss(&labels, &probs), compensated / n as f64);
    }
}
