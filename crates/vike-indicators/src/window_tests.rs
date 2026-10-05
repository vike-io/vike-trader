use super::*;

/// One group, as a slice `per_group` can take.
///
/// Spelled through a function rather than as a `[a..b]` literal because
/// `clippy::single_range_in_vec_init` reads that literal as a possible typo for `[a; b]` — an
/// array of `b` copies of `a` — and refuses it under `-D warnings`. Naming the intent once
/// beats an `#[allow]` at four call sites.
///
/// ⚠ It does NOT hide `clippy::reversed_empty_ranges`, which fires on the literal at the CALL
/// site regardless of what it is passed to. `an_inverted_group_is_refused` therefore still
/// carries its own allow, and must.
fn one(g: std::ops::Range<usize>) -> [std::ops::Range<usize>; 1] {
    [g]
}

/// THE bug this function exists to make unrepresentable, driven directly.
///
/// Two assets stacked in one column. Applied to the WHOLE slice, the kernel's window walks
/// straight across the boundary and the second asset's first rows average in the first
/// asset's tail — silently, with a plausible number where a warm-up NaN belongs.
#[test]
fn a_kernel_applied_per_group_cannot_reach_across_a_group_boundary() {
    // asset A: four 100s. asset B: four 1s. A 3-period mean.
    let x = [100.0, 100.0, 100.0, 100.0, 1.0, 1.0, 1.0, 1.0];
    let spec = WindowSpec::indicator(3);

    // WITHOUT grouping: index 4 is B's first row and index 5 its second, and both average in
    // A's 100s. This is the defect, asserted so the fix is measured against it rather than
    // assumed.
    let flat = rolling_mean(&x, spec);
    assert!(flat[4] > 60.0, "the ungrouped kernel should be contaminated, got {}", flat[4]);
    assert!(flat[5] > 30.0, "the ungrouped kernel should be contaminated, got {}", flat[5]);

    // WITH grouping: B's first two rows are warm-up NaN, and its third is a clean 1.0.
    let out = per_group(&x, &[0..4, 4..8], "close", |s| rolling_mean(s, spec)).unwrap();
    assert!(out[0].is_nan() && out[1].is_nan(), "A's own warm-up must survive");
    assert_eq!(out[2], 100.0);
    assert_eq!(out[3], 100.0);
    assert!(out[4].is_nan(), "B's first row must be warm-up NaN, got {}", out[4]);
    assert!(out[5].is_nan(), "B's second row must be warm-up NaN, got {}", out[5]);
    assert_eq!(out[6], 1.0, "B's first complete window is its own data alone");
    assert_eq!(out[7], 1.0);
}

/// A row no group covers stays NaN rather than silently taking a neighbour's value.
#[test]
fn rows_outside_every_group_stay_nan() {
    let x = [1.0, 2.0, 3.0, 4.0];
    let groups = one(0..2);
    let out = per_group(&x, &groups, "v", |s| s.to_vec()).unwrap();
    assert_eq!(out[0], 1.0);
    assert_eq!(out[1], 2.0);
    assert!(out[2].is_nan() && out[3].is_nan(), "uncovered rows must not be filled");
}

/// A kernel that returns the wrong length is a REFUSAL, never a silent truncation — the same
/// failure mode the grouping exists to prevent, one level up.
#[test]
fn a_kernel_that_returns_the_wrong_length_is_refused() {
    let x = [1.0, 2.0, 3.0, 4.0];
    let groups = one(0..4);
    let e = per_group(&x, &groups, "close", |_| vec![0.0; 2]).unwrap_err();
    match e {
        WindowError::KernelLength { src, expected, got } => {
            assert_eq!(src, "close");
            assert_eq!(expected, 4);
            assert_eq!(got, 2);
        }
        other => panic!("expected KernelLength, got {other:?}"),
    }
}

/// A group reaching past the end of the column is refused rather than panicking on the slice.
#[test]
fn a_group_out_of_bounds_is_refused_rather_than_panicking() {
    let x = [1.0, 2.0];
    let groups = one(0..5);
    let e = per_group(&x, &groups, "close", |s| s.to_vec()).unwrap_err();
    assert!(matches!(e, WindowError::GroupOutOfBounds { .. }), "got {e:?}");
}

/// An inverted range is refused too — `start > end` would otherwise panic inside the slice.
#[test]
fn an_inverted_group_is_refused() {
    let x = [1.0, 2.0, 3.0];

    // An inverted range IS what this test asserts against, so clippy's refusal of the literal
    // is exactly backwards here.
    #[allow(clippy::reversed_empty_ranges)]
    let groups = one(2..1);
    let e = per_group(&x, &groups, "close", |s| s.to_vec()).unwrap_err();
    assert!(matches!(e, WindowError::GroupOutOfBounds { .. }), "got {e:?}");
}

#[test]
fn indicator_preset_is_population_variance_over_the_inclusive_window() {
    // [1,2,3]: mean 2, population var = ((1)+(0)+(1))/3
    let v = rolling_var(&[1.0, 2.0, 3.0], WindowSpec::indicator(3));
    assert!(v[0].is_nan() && v[1].is_nan());
    assert_eq!(v[2], 2.0 / 3.0);
}

#[test]
fn point_in_time_preset_is_sample_variance_and_excludes_the_current_row() {
    // At i=3 the window is [1,2,3] (row 3 excluded); sample var = 2/2 = 1.
    let v = rolling_var(&[1.0, 2.0, 3.0, 99.0], WindowSpec::point_in_time(3));
    assert!(v[0].is_nan() && v[1].is_nan() && v[2].is_nan());
    assert_eq!(v[3], 1.0);
}

#[test]
fn std_is_the_sqrt_of_var_against_a_known_value() {
    // Same fixture as the population-variance test above: var = 2/3, so std = sqrt(2/3).
    // `rolling_std` is a named Task 2 deliverable with no caller yet — a dropped `sqrt` would
    // otherwise ship green.
    let s = rolling_std(&[1.0, 2.0, 3.0], WindowSpec::indicator(3));
    assert!(s[0].is_nan() && s[1].is_nan());
    assert!((s[2] - (2.0f64 / 3.0).sqrt()).abs() < 1e-15);
}

#[test]
fn a_flat_window_has_exactly_zero_variance_not_a_rounding_artifact() {
    // ⚠ This fixture passes with OR without the constant-window check, and for years it was
    // the only one: `100.0` is exact in binary, `100*4/4` is `100.0`, so the two-pass fold
    // cancels to zero by luck of the value. The sibling below is the one that can fail.
    let v = rolling_var(&[100.0; 8], WindowSpec::indicator(4));
    assert_eq!(v[7], 0.0, "two-pass fold must return exact zero on a flat window");
}

#[test]
fn a_flat_window_is_exactly_zero_even_when_its_naive_mean_is_not_the_value() {
    // The defect, minimised. 24 copies of 0.1 fold to a sum whose /24 is ONE ULP above 0.1,
    // so every deviation is ~1.4e-17 rather than 0 and the fold alone returns ~2e-34.
    // pandas (3.0.1, measured on the oracle's own interpreter):
    // `pd.Series([0.1]*24).rolling(24).std()` is bits 0000000000000000.
    let v = 0.1_f64;
    let x = [v; 24];
    assert_ne!(
        x.iter().sum::<f64>() / 24.0,
        v,
        "fixture is pointless unless the naive mean MISSES the repeated value"
    );
    let var = rolling_var(&x, WindowSpec::indicator(24));
    assert_eq!(
        var[23].to_bits(),
        0u64,
        "got {} — a window of identical values has variance exactly zero",
        var[23]
    );
}

#[test]
fn zscore_is_nan_when_the_window_is_flat() {
    let z = zscore(&[100.0; 8], WindowSpec::indicator(4), None);
    assert!(z[7].is_nan(), "zero sd must yield NaN, never an infinity");
}

#[test]
fn zscore_is_nan_on_a_flat_window_whose_naive_mean_misses_the_value() {
    // The live shape: a rate series holding one value for 24 hours. Before the
    // constant-window check this returned a FINITE number — measured against the oracle as an
    // order-1 value on one stretch of real data and ±1e14 on another, both against a pandas
    // NaN. A ±1e14 cell in a feature column that feeds ML training dominates every split.
    let z = zscore(&[0.1_f64; 24], WindowSpec::indicator(24), None);
    assert!(z[23].is_nan(), "got {} — a zero-variance window has no z-score", z[23]);
}

#[test]
fn a_flat_point_in_time_history_is_nan_even_when_the_current_row_differs() {
    // `funding_z_24h`'s own spec: the window EXCLUDES the current row, so the numerator here
    // is a real (0.5 - 0.1) sitting over a zero sd. The oracle divides by
    // `fstd.replace(0, np.nan)`, so the answer is NaN and NOT ±inf, and NOT the clip floor —
    // measured end-to-end through `features/price_context.py`'s expression, pandas 3.0.1.
    let mut x = vec![0.1_f64; 24];
    x.push(0.5);
    let z = zscore(&x, WindowSpec::point_in_time(24), Some(6.0));
    assert!(z[24].is_nan(), "got {} — a zero-sd history has no z-score at any numerator", z[24]);
}

#[test]
fn a_variance_that_is_small_but_real_still_yields_a_finite_z() {
    // ⚠ The anti-blindness gate. The constant-window check must be EXACT equality over the
    // inputs; a magnitude threshold on the output would swallow this window, whose values
    // differ by a single ULP and whose variance is therefore genuinely non-zero.
    //
    // ⚠ NEITHER the sd NOR the z is compared against pandas' number here, and that is a
    // MEASURED residual rather than a shrug. On this window, both measured on the oracle's
    // own interpreter:
    //
    //   sd — this kernel 2.790601e-17, pandas 3.0.1 (ddof = 1) 2.893719e-18: a factor of 9.6.
    //   z  — pandas -4.795831523312719; this kernel's differs in the same way and worse.
    //
    // It is arithmetic that predates the constant-window check and is untouched by it: the
    // mean here is the NAIVE fold `Σx / n`, which on 24 copies of 0.1 lands 2 ULP above the
    // true mean, while pandas' lands on it (measured: bits 3fb999999999999a against this
    // kernel's 3fb999999999999b). When the window's ENTIRE spread is
    // one ULP the mean's own rounding error is the LARGER quantity and it dominates the
    // deviations — 23 of them become 2 ULP instead of 1/24 of one. The arithmetic checks out
    // both ways: `sqrt((23·2² + 1²)/23)` is 2.01 ULP of 0.1 = 2.79e-17, and the exact-mean
    // `sqrt((23·(1/24)² + (23/24)²)/23)` is 0.204 ULP = 2.83e-18. The z is worse again because
    // its numerator is then a catastrophic cancellation. This is the regime immediately
    // OUTSIDE the constant case, it is UNCALIBRATED, and a caller who needs accuracy there
    // wants a compensated mean or Welford — not an epsilon, which would swallow the window
    // whole rather than merely rounding it.
    //
    // What this test gates is the property the fix must not break: the window is NOT
    // constant, so a number comes out — and it is a tiny one, which is what proves the
    // fixture sits in the near-flat regime rather than being a fat window in disguise.
    let v = 0.1_f64;
    let mut x = [v; 24];
    x[5] = f64::from_bits(v.to_bits() + 1); // nextafter(0.1, +inf) — one ULP
    let spec = WindowSpec { period: 24, lag: 0, min_periods: 24, ddof: 1 };
    let sd = rolling_std(&x, spec)[23];
    assert!(sd > 0.0 && sd.is_finite(), "got {sd} — a one-ULP spread is a REAL variance");
    assert!(
        (1e-20..1e-15).contains(&sd),
        "sd {sd} left the near-flat regime — this fixture no longer tests the boundary"
    );
    assert!(
        zscore(&x, spec, None)[23].is_finite(),
        "the constant-window check swallowed a real variance"
    );
}

#[test]
fn clip_bounds_a_positive_excursion() {
    // `point_in_time`'s window at i=19 is x[0..=18] — it EXCLUDES x[19] itself (lag=1). A
    // perfectly flat history there would hit the zero-sd -> NaN guard (see the test above),
    // which is not what this test means to exercise, so x[18] is the one nonzero row in that
    // history: enough for a real (small) sd, so the outlier at x[19] produces a huge but
    // FINITE z.
    let mut x = vec![0.0; 20];
    x[18] = 1.0; // the one nonzero row in an otherwise flat history
    x[19] = 1_000.0; // an enormous excursion against that near-flat history
    let unclipped = zscore(&x, WindowSpec::point_in_time(19), None);
    let clipped = zscore(&x, WindowSpec::point_in_time(19), Some(6.0));
    assert!(unclipped[19] > 6.0);
    assert_eq!(clipped[19], 6.0);
}

#[test]
fn clip_bounds_a_negative_excursion() {
    // Mirror of the positive case above: `Some(c)` clips to `[-c, c]`, not just `<= c`, and
    // nothing in the file exercised the lower bound — `z.max(-c).min(c)` would pass every
    // other test here even with no lower bound at all (e.g. a stray `z.min(c)`).
    let mut x = vec![0.0; 20];
    x[18] = 1.0; // the one nonzero row in an otherwise flat history
    x[19] = -1_000.0; // an enormous excursion the other way
    let unclipped = zscore(&x, WindowSpec::point_in_time(19), None);
    let clipped = zscore(&x, WindowSpec::point_in_time(19), Some(6.0));
    assert!(unclipped[19] < -6.0);
    assert_eq!(clipped[19], -6.0);
}

#[test]
fn clipping_in_range_values_leaves_their_relative_order_unchanged() {
    // A short oscillating series gives two different, well-inside-the-clip z-scores at two
    // complete windows; clip(6.0) must not touch either, so both their values AND their
    // relative order survive untouched.
    let x = [1.0, 2.0, 1.0, 3.0, 1.0, 4.0, 1.0, 5.0];
    let unclipped = zscore(&x, WindowSpec::indicator(4), None);
    let clipped = zscore(&x, WindowSpec::indicator(4), Some(6.0));
    assert!(unclipped[6] < unclipped[7], "fixture must produce two distinct in-range z-scores");
    assert_eq!(clipped[6], unclipped[6]);
    assert_eq!(clipped[7], unclipped[7]);
    assert!(clipped[6] < clipped[7]);
}

#[test]
fn a_nan_inside_the_window_propagates_to_nan() {
    let v = rolling_var(&[1.0, f64::NAN, 3.0], WindowSpec::indicator(3));
    assert!(v[2].is_nan());
}

#[test]
fn a_zero_period_is_not_a_window() {
    let v = rolling_var(&[1.0, 2.0], WindowSpec { period: 0, lag: 0, min_periods: 0, ddof: 0 });
    assert!(v.iter().all(|x| x.is_nan()));
}

#[test]
fn rank_pct_of_the_largest_value_in_its_window_is_one() {
    let r = rolling_rank_pct(&[1.0, 2.0, 3.0, 4.0], WindowSpec::indicator(4));
    assert_eq!(r[3], 1.0);
}

#[test]
fn rank_pct_of_the_smallest_value_is_one_over_n() {
    let r = rolling_rank_pct(&[4.0, 3.0, 2.0, 1.0], WindowSpec::indicator(4));
    assert_eq!(r[3], 0.25);
}

#[test]
fn ties_take_the_average_rank_like_pandas() {
    // window [1,2,2,4], current value 4 -> rank 4 -> 1.0
    let r = rolling_rank_pct(&[1.0, 2.0, 2.0, 4.0], WindowSpec::indicator(4));
    assert_eq!(r[3], 1.0);
    // window [2,2,4,2], current value 2: two below-or-equal ties + itself
    // less = 0, eq = 3 -> rank = 0 + (3+1)/2 = 2 -> 0.5
    let r2 = rolling_rank_pct(&[2.0, 2.0, 4.0, 2.0], WindowSpec::indicator(4));
    assert_eq!(r2[3], 0.5);
}

#[test]
fn rank_pct_with_lag_one_ranks_the_previous_row_not_the_current_one() {
    // At i=3, lag=1, period=3: the window is [1,2,3] and the ranked value is x[2] = 3.
    let r = rolling_rank_pct(&[1.0, 2.0, 3.0, 99.0], WindowSpec::point_in_time(3));
    assert_eq!(r[3], 1.0);
}

#[test]
fn median_of_an_odd_window_is_the_middle_value() {
    let m = rolling_median(&[3.0, 1.0, 2.0], WindowSpec::indicator(3));
    assert_eq!(m[2], 2.0);
}

#[test]
fn median_of_an_even_window_averages_the_two_middle_values() {
    let m = rolling_median(&[4.0, 1.0, 3.0, 2.0], WindowSpec::indicator(4));
    assert_eq!(m[3], 2.5);
}

#[test]
fn median_is_nan_before_the_window_is_complete() {
    let m = rolling_median(&[1.0, 2.0, 3.0], WindowSpec::indicator(3));
    assert!(m[0].is_nan() && m[1].is_nan());
}

#[test]
fn a_missing_current_value_stays_nan_under_a_clip() {
    // The window [1..=8] is complete and non-degenerate; row 8 is the missing observation.
    // `window_at` inspects rows `i - lag - period + 1 ..= i - lag`, so at `lag = 1` the
    // CURRENT row is never checked — which is the sparse-input shape exactly: one column's
    // current observation absent while the 24/72/168h history behind it is full.
    let mut x: Vec<f64> = (1..=8).map(|v| v as f64).collect();
    x.push(f64::NAN);
    let z = zscore(&x, WindowSpec::point_in_time(8), Some(6.0));
    assert!(z[8].is_nan(), "got {} — f64::max ignores NaN, so the clip invented a value", z[8]);
}

#[test]
fn an_unclipped_missing_value_was_already_nan_and_stays_nan() {
    let mut x: Vec<f64> = (1..=8).map(|v| v as f64).collect();
    x.push(f64::NAN);
    assert!(zscore(&x, WindowSpec::point_in_time(8), None)[8].is_nan());
}
