use super::*;

#[test]
fn the_two_sided_p_of_a_zero_tstat_is_one() {
    assert!((two_sided_normal_p_value(0.0) - 1.0).abs() < 1e-12);
}

#[test]
fn a_two_sigma_tstat_lands_near_the_familiar_five_percent() {
    let p = two_sided_normal_p_value(1.959_963_984_540_054);
    assert!((p - 0.05).abs() < 1e-6, "got {p}");
}

#[test]
fn the_p_value_is_symmetric_in_the_sign_of_the_tstat() {
    assert!((two_sided_normal_p_value(2.5) - two_sided_normal_p_value(-2.5)).abs() < 1e-15);
}

#[test]
fn an_empty_input_rejects_nothing() {
    assert!(benjamini_hochberg(&[], 0.10).is_empty());
}

#[test]
fn a_single_tiny_pvalue_is_rejected() {
    assert_eq!(benjamini_hochberg(&[0.001], 0.10), vec![true]);
}

#[test]
fn all_large_pvalues_are_rejected_by_nothing() {
    assert_eq!(benjamini_hochberg(&[0.9, 0.8, 0.7], 0.10), vec![false, false, false]);
}

#[test]
fn rejection_is_a_step_up_so_a_large_p_rides_in_on_a_small_one() {
    // Thresholds at q=0.10, n=3: 0.0333 / 0.0667 / 0.10.
    // Rank 2 (0.08) FAILS its own threshold; rank 3 (0.09) passes, so step-up rejects
    // all three. A step-down that stops at the first failure would return [true,false,false].
    assert_eq!(benjamini_hochberg(&[0.001, 0.08, 0.09], 0.10), vec![true, true, true]);
}

#[test]
fn the_mask_is_returned_in_the_callers_order_not_sorted_order() {
    // 0.9 must stay false in position 0 even though 0.001 in position 1 is rejected.
    assert_eq!(benjamini_hochberg(&[0.9, 0.001], 0.10), vec![false, true]);
}
