//! Parameter-stability scoring for optimization results — port of `analysis/stability.py`.
//!
//! WealthLab's "Parameter Stability" graph shows how performance varies across the parameter
//! neighbourhood — a broad plateau is robust; a lonely spike is fragile. [`parameter_stability`]
//! quantifies that into a single `[0, 1]` score and [`stability_label`] gives a three-way label.
//!
//! Given N trial scores sorted best-first:
//! 1. Select the top `top_frac` fraction of trials (at least 1).
//! 2. Compute the plateau ratio: `mean(top scores) / best_score` (sign-handled for negative
//!    scores by negating so "least negative" reads as "best").
//! 3. Clamp to `[0, 1]`.
//!
//! Only a plain score list is needed — no actual optimizer/trial-generation machinery.

/// Python's `round()`: round-half-to-even ("banker's rounding"), NOT Rust's `f64::round()`
/// (round-half-away-from-zero). `k = round(n * top_frac)` must match exactly, or the trial
/// count entering the "top" neighbourhood silently diverges from the oracle.
fn round_half_even(x: f64) -> f64 {
    let floor = x.floor();
    let diff = x - floor;
    if diff < 0.5 {
        floor
    } else if diff > 0.5 {
        floor + 1.0
    } else if (floor as i64) % 2 == 0 {
        floor
    } else {
        floor + 1.0
    }
}

/// Return a robustness score in `[0, 1]` for the optimum's neighbourhood.
///
/// `scores`: trial scores (any order — sorted internally, descending).
/// `top_frac`: fraction of trials (sorted by score, descending) to treat as the neighbourhood
/// of the optimum. `1.0` = flat plateau (all top scores ~= best), `0.0` = isolated spike.
/// Returns `1.0` for 0 or 1 trials (no evidence of spikiness).
pub fn parameter_stability(scores: &[f64], top_frac: f64) -> f64 {
    let n = scores.len();
    if n <= 1 {
        return 1.0;
    }

    let mut sorted = scores.to_vec();
    sorted.sort_by(|a, b| b.partial_cmp(a).unwrap());
    let best = sorted[0];

    let k = (round_half_even(n as f64 * top_frac) as usize).max(1);
    let top_scores = &sorted[..k.min(n)];
    let mean_top = top_scores.iter().sum::<f64>() / top_scores.len() as f64;

    let ratio = if best == 0.0 {
        return if mean_top == 0.0 { 1.0 } else { 0.0 };
    } else if best > 0.0 {
        mean_top / best
    } else {
        let neg_best = -best;
        let neg_mean = -mean_top;
        if neg_mean != 0.0 { neg_best / neg_mean } else { 1.0 }
    };

    ratio.clamp(0.0, 1.0)
}

/// Classify a stability score into a plain-language label.
///
/// `>= 0.8` -> `"plateau"` (broad, robust optimum); `>= 0.5` -> `"ridge"` (moderate
/// sensitivity); else `"spike"` (fragile / overfit optimum).
pub fn stability_label(stability: f64) -> &'static str {
    if stability >= 0.8 {
        "plateau"
    } else if stability >= 0.5 {
        "ridge"
    } else {
        "spike"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stability_empty_returns_one() {
        assert_eq!(parameter_stability(&[], 0.25), 1.0);
    }

    #[test]
    fn stability_single_trial_returns_one() {
        assert_eq!(parameter_stability(&[5.0], 0.25), 1.0);
    }

    #[test]
    fn stability_two_equal_scores_returns_one() {
        assert!((parameter_stability(&[3.0, 3.0], 0.25) - 1.0).abs() < 1e-9);
    }

    #[test]
    fn plateau_high_stability() {
        let s = parameter_stability(&[10.0, 10.0, 9.9, 9.8, 9.7, 1.0], 0.25);
        assert!(s >= 0.9, "expected plateau, got {s:.3}");
    }

    #[test]
    fn spike_low_stability() {
        // top_frac=0.5 over 8 trials -> top 4: [10, 1, 1, 1]; mean=3.25; ratio=0.325 < 0.5
        let s = parameter_stability(&[10.0, 1.0, 1.0, 1.0, 1.0, 1.0, 1.0, 1.0], 0.5);
        assert!(s < 0.5, "expected spike, got {s:.3}");
    }

    #[test]
    fn spike_lower_than_plateau() {
        let plateau = parameter_stability(&[10.0, 9.9, 9.8, 9.7, 9.6, 9.5], 0.25);
        let spike = parameter_stability(&[10.0, 1.0, 1.0, 1.0, 1.0, 1.0], 0.25);
        assert!(spike < plateau);
    }

    #[test]
    fn stability_in_unit_interval() {
        for scores in [
            &[10.0, 1.0, 0.5][..],
            &[-1.0, -2.0, -3.0][..],
            &[0.0, 0.0, 0.0][..],
            &[5.0, 5.0, 5.0, 5.0][..],
        ] {
            let s = parameter_stability(scores, 0.25);
            assert!((0.0..=1.0).contains(&s), "out of range: {s}");
        }
    }

    #[test]
    fn negative_plateau() {
        let s = parameter_stability(&[-1.0, -1.1, -1.2, -1.3, -10.0, -20.0], 0.25);
        assert!(s >= 0.8, "expected negative plateau to be stable, got {s:.3}");
    }

    #[test]
    fn negative_spike() {
        let s = parameter_stability(&[-1.0, -9.0, -9.0, -9.0, -9.0, -9.0, -9.0, -9.0], 0.25);
        assert!(s < 0.5, "expected negative spike to be unstable, got {s:.3}");
    }

    #[test]
    fn zero_best_no_panic() {
        assert!((parameter_stability(&[0.0, 0.0, 0.0], 0.25) - 1.0).abs() < 1e-9);
    }

    #[test]
    fn round_half_even_matches_python() {
        assert_eq!(round_half_even(1.5), 2.0);
        assert_eq!(round_half_even(2.5), 2.0); // ties-to-even, NOT away-from-zero
        assert_eq!(round_half_even(0.75), 1.0);
        assert_eq!(round_half_even(4.0), 4.0);
    }

    #[test]
    fn label_plateau_ridge_spike() {
        assert_eq!(stability_label(1.0), "plateau");
        assert_eq!(stability_label(0.8), "plateau");
        assert_eq!(stability_label(0.79), "ridge");
        assert_eq!(stability_label(0.5), "ridge");
        assert_eq!(stability_label(0.49), "spike");
        assert_eq!(stability_label(0.0), "spike");
    }

    #[test]
    fn label_matches_stability_for_plateau_scenario() {
        let s = parameter_stability(&[10.0, 9.9, 9.8, 9.7, 9.6, 9.5], 0.25);
        assert_eq!(stability_label(s), "plateau");
    }

    #[test]
    fn label_matches_stability_for_spike_scenario() {
        let s = parameter_stability(&[10.0, 1.0, 1.0, 1.0, 1.0, 1.0, 1.0, 1.0], 0.5);
        assert_eq!(stability_label(s), "spike");
    }
}
