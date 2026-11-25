//! Hand-computed cases for `change_detection`: every expected number below is worked from the published
//! formula in the comment beside it, never captured from a run.

use super::*;

fn close_enough(a: f64, b: f64) -> bool {
    (a - b).abs() <= 1e-12 * b.abs().max(1.0)
}

#[test]
fn population_stability_index_moves_with_the_mass_between_reference_quantile_bins() {
    // Reference [1, 2, 3, 4], 2 bins: the one edge is the median 2.5, shares (1/2, 1/2).
    // Current [1, 1, 1, 4]: shares (3/4, 1/4).
    // PSI = (3/4 - 1/2)·ln(3/2) + (1/4 - 1/2)·ln(1/2) = (ln 3/2 + ln 2) / 4 = ln(3) / 4.
    let psi = population_stability_index(&[1.0, 2.0, 3.0, 4.0], &[4.0, 1.0, 1.0, 1.0], 2);
    assert!(close_enough(psi, libm::log(3.0) / 4.0), "{psi}");
    // The same shares give exactly zero.
    assert_eq!(population_stability_index(&[1.0, 2.0, 3.0, 4.0], &[2.0, 1.0, 3.0, 4.0], 2), 0.0);
}

#[test]
fn population_stability_index_floors_an_empty_bin_and_merges_tied_edges() {
    // Current [5, 6] fills only the upper bin: shares (1e-4 floored, 1) against (1/2, 1/2).
    let floor = POPULATION_STABILITY_EMPTY_BIN_SHARE;
    let psi = population_stability_index(&[1.0, 2.0, 3.0, 4.0], &[5.0, 6.0], 2);
    let expected = (floor - 0.5) * libm::log(floor / 0.5) + 0.5 * libm::log(2.0);
    assert!(close_enough(psi, expected), "{psi} != {expected}");

    // A constant reference: all nine decile edges are 3, merged into one, so two bins remain:
    // (-inf, 3] holds the whole reference and (3, inf) none of it (floored).
    // Current [3, 3, 4, 4]: shares (1/2, 1/2).
    let psi = population_stability_index(&[3.0; 4], &[3.0, 3.0, 4.0, 4.0], 10);
    let expected = (0.5 - 1.0) * libm::log(0.5) + (0.5 - floor) * libm::log(0.5 / floor);
    assert!(close_enough(psi, expected), "{psi} != {expected}");
}

#[test]
fn only_finite_values_are_compared_and_an_empty_sample_answers_nan() {
    let clean = population_stability_index(&[1.0, 2.0, 3.0, 4.0], &[1.0, 1.0, 1.0, 4.0], 2);
    let dirty = population_stability_index(
        &[1.0, f64::NAN, 2.0, 3.0, f64::INFINITY, 4.0],
        &[1.0, 1.0, f64::NEG_INFINITY, 1.0, 4.0],
        2,
    );
    assert_eq!(clean, dirty);
    assert!(population_stability_index(&[], &[1.0], 4).is_nan());
    assert!(population_stability_index(&[1.0], &[f64::NAN], 4).is_nan());
    let ks = kolmogorov_smirnov_two_sample(&[f64::NAN], &[1.0]);
    assert!(ks.statistic.is_nan() && ks.p_value.is_nan());
}

#[test]
#[should_panic(expected = "at least one bin")]
fn population_stability_index_refuses_zero_bins() {
    population_stability_index(&[1.0], &[1.0], 0);
}

#[test]
fn kolmogorov_smirnov_statistic_is_the_largest_cdf_gap_with_ties_stepped_together() {
    // F_a - F_b at 1, 2, 3, 4: 1/4 - 0, 2/4 - 0, 3/4 - 1/4, 1 - 2/4 -> D = 1/2.
    let ks = kolmogorov_smirnov_two_sample(&[1.0, 2.0, 3.0, 4.0], &[6.0, 5.0, 4.0, 3.0]);
    assert_eq!(ks.statistic, 0.5);
    // Ties: at 1 both CDFs step together, 2/3 - 1/3; at 2 both reach 1. D = 1/3, never the 2/3 a
    // one-sample-at-a-time walk would read.
    let ks = kolmogorov_smirnov_two_sample(&[1.0, 1.0, 2.0], &[1.0, 2.0, 2.0]);
    assert!(close_enough(ks.statistic, 1.0 / 3.0), "{}", ks.statistic);
}

#[test]
fn kolmogorov_smirnov_p_value_is_one_for_identical_samples_and_vanishes_for_disjoint_ones() {
    let same = kolmogorov_smirnov_two_sample(&[1.0, 2.0, 3.0], &[3.0, 2.0, 1.0]);
    assert_eq!((same.statistic, same.p_value), (0.0, 1.0));
    // 50 against 50 disjoint: D = 1, nₑ = 25, λ = 5 + 0.12 + 0.022, Q ≈ 2·e^{-52.9} ≈ 2e-23.
    let low: Vec<f64> = (0..50).map(f64::from).collect();
    let high: Vec<f64> = (100..150).map(f64::from).collect();
    let apart = kolmogorov_smirnov_two_sample(&low, &high);
    assert_eq!(apart.statistic, 1.0);
    assert!(apart.p_value > 0.0 && apart.p_value < 1e-20, "{}", apart.p_value);
}

#[test]
fn the_kolmogorov_tail_matches_its_series_and_the_published_critical_values() {
    // Q(1) = 2(e^-2 - e^-8 + e^-18 - e^-32 + …); the fifth term is below 1e-21.
    let series = 2.0 * (libm::exp(-2.0) - libm::exp(-8.0) + libm::exp(-18.0) - libm::exp(-32.0));
    assert!(close_enough(kolmogorov_survival(1.0), series));
    // The tabulated asymptotic critical values: λ = 1.2238, 1.3581, 1.6276 at 10%, 5%, 1%.
    for (lambda, alpha) in [(1.2238, 0.10), (1.3581, 0.05), (1.6276, 0.01)] {
        let q = kolmogorov_survival(lambda);
        assert!((q - alpha).abs() < 1e-4, "Q({lambda}) = {q}, table {alpha}");
    }
    assert_eq!(kolmogorov_survival(0.0), 1.0);
}

#[test]
fn page_hinkley_alarms_on_a_rise_and_restarts() {
    // Errors 0, 0, 0, 0 then 1s; tolerance 0, threshold 1.5. x̄ and m after each 1:
    // t=5: x̄ = 1/5, m = 4/5.   t=6: x̄ = 1/3, m = 4/5 + 2/3 ≈ 1.467 (not > 1.5).
    // t=7: x̄ = 3/7, m ≈ 1.467 + 4/7 ≈ 2.038 > 1.5 (M = 0) -> alarm at position 6, restart;
    // position 7 is the first error of a new run: m = 0.
    let errors = [0.0, 0.0, 0.0, 0.0, 1.0, 1.0, 1.0, 1.0];
    assert_eq!(page_hinkley_alarms(&errors, 0.0, 1.5), vec![6]);
    // Tolerance 0.5: m falls to -2 over the zeros (M = -2), then rises by 0.3, 1/6, 1/14 and 0:
    // m - M peaks near 0.54, below the threshold.
    assert!(page_hinkley_alarms(&errors, 0.5, 1.5).is_empty());
    // A NaN error is skipped: it neither moves the detector nor counts toward the mean.
    let with_gap = [0.0, 0.0, f64::NAN, 0.0, 0.0, 1.0, 1.0, 1.0, 1.0];
    assert_eq!(page_hinkley_alarms(&with_gap, 0.0, 1.5), vec![7]);
    assert!(page_hinkley_alarms(&[], 0.0, 1.0).is_empty());
}

#[test]
fn cusum_alarms_on_a_rise_above_reference_plus_allowance() {
    // Steps x - 0.1 - 0.2: -0.2, -0.3, -0.1, 0.7, 0.7, 0.7, -0.3.
    // S = max(0, S + step): 0, 0, 0, 0.7, 1.4 > 1 -> alarm at 4, restart; 0.7, 0.4.
    let errors = [0.1, 0.0, 0.2, 1.0, 1.0, 1.0, 0.0];
    assert_eq!(cusum_alarms(&errors, 0.1, 0.2, 1.0), vec![4]);
    assert!(cusum_alarms(&[], 0.0, 0.0, 1.0).is_empty());
}

#[test]
fn cusum_alarms_leave_out_the_filters_lower_side() {
    // Steps -1, -1, -1, 1, 1 (threshold 1.5). `symmetric_cusum`'s lower sum crosses at step 1
    // (-2 < -1.5); the upper one is 0, 0, 0, 1, 2 > 1.5 at step 4. Only the rise is an alarm.
    let errors = [-1.0, -1.0, -1.0, 1.0, 1.0];
    assert_eq!(cusum_alarms(&errors, 0.0, 0.0, 1.5), vec![4]);
    // The FALL is the same call on the negated errors: S over 1, 1, 1, -1, -1 crosses at step 1.
    let negated: Vec<f64> = errors.iter().map(|e| -e).collect();
    assert_eq!(cusum_alarms(&negated, 0.0, 0.0, 1.5), vec![1]);
}

#[test]
fn cusum_fires_on_strict_crossings_and_resets_only_the_side_that_fired() {
    // Δ = [1, 1, 0.5, -1.5, -0.5, -0.1, 2.6], h = 1.5.
    // i=2: S⁺ = 2 > 1.5 -> event, S⁺ = 0.  i=4: S⁻ = -1.5 is NOT < -1.5.
    // i=5: S⁻ = -2.0 -> event.  i=7: S⁺ = 2.6 -> event.
    let series = [0.0, 1.0, 2.0, 2.5, 1.0, 0.5, 0.4, 3.0];
    assert_eq!(cusum_events_of_series(&series, 1.5), vec![2, 5, 7]);
    assert!(cusum_events_of_series(&[1.0], 0.1).is_empty());
    assert!(cusum_events_of_series(&[], 0.1).is_empty());
}
