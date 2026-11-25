use super::*;

/// `percentile` is INTERPOLATING (numpy's default `method="linear"`), and this pins it against
/// the NEAREST-RANK reading — `sorted[round((n - 1) * q)]` — that a second implementation in
/// `crates/vike-backtesthub/src/bin/cheap_np_depth.rs` had grown independently. The three
/// even-length assertions are values nearest-rank CANNOT produce — it can only ever return an
/// element of the input — so a "simplification" back to indexing reddens here rather than
/// silently moving an operator-facing number. The rest pin the cases where the two conventions
/// agree (the ends, an integer rank, and the two degenerate lengths), so the agreement is a
/// recorded claim rather than an assumption.
#[test]
fn percentile_interpolates_and_is_not_nearest_rank() {
    let v = [0.0, 1.0, 2.0, 3.0];
    assert_eq!(percentile(&v, 0.5), 1.5, "nearest-rank would read 2.0 here");
    assert_eq!(percentile(&v, 0.25), 0.75);
    assert_eq!(percentile(&v, 0.75), 2.25);
    // The two ends are exact, and `q == 1.0` is the `else` arm (there is no `lo + 1`).
    assert_eq!(percentile(&v, 0.0), 0.0);
    assert_eq!(percentile(&v, 1.0), 3.0);
    // An odd length AGREES with nearest-rank wherever `(n - 1) * q` lands on an integer.
    let odd = [0.0, 1.0, 2.0, 3.0, 4.0];
    assert_eq!(percentile(&odd, 0.5), 2.0);
    assert_eq!(percentile(&odd, 0.25), 1.0);
    // Degenerate inputs: the documented empty answer, and the no-special-case single element.
    assert_eq!(percentile(&[], 0.5), 0.0);
    for q in [0.0, 0.1, 0.5, 0.9, 1.0] {
        assert_eq!(percentile(&[7.0], q), 7.0, "n == 1 needs no special case");
    }
}

/// **The ONE literal pin of [`PERCENTILE_METHOD`], and it sits beside the algorithm it names.**
/// Every artifact-side test compares its stamp to the constant rather than to a second copy of
/// this string, so there is exactly one place a convention change has to be typed — and it is
/// this one, next to the body being changed.
///
/// It reddens on a RENAME as well as on a real change of convention, deliberately: a rename is
/// exactly as invisible to a reader holding two JSON files as a silent algorithm swap, and the
/// author who lands here is the author who has to decide which of the two they are doing.
///
/// The shape assertions are not decoration either — the value is grepped out of pasted JSON, so
/// it must stay one lowercase ASCII token with no whitespace.
#[test]
fn the_published_method_name_is_pinned_and_stays_greppable() {
    assert_eq!(
        PERCENTILE_METHOD, "numpy_linear",
        "an artifact stamped with a DIFFERENT name than an operator was told to look for is \
             the defect this constant exists to prevent — if `percentile`'s body really changed, \
             change this string AND say so where operators read (this crate's own doc, \
             `crates/vike-backtesthub/src/binutil.rs`'s `PERCENTILE_NOTE`, and \
             `crates/vike-backtest/CLAUDE.md`)"
    );
    assert!(!PERCENTILE_METHOD.is_empty());
    assert!(
        PERCENTILE_METHOD.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_'),
        "one lowercase ASCII token, greppable out of pasted JSON: {PERCENTILE_METHOD:?}"
    );
}

/// The mean and the sample variance of four values by hand, and the population divisor.
#[test]
fn mean_variance_by_hand() {
    let (average, variance) = mean_variance([1.0, 2.0, 3.0, 4.0].into_iter(), 1.0);
    assert_eq!(average, 2.5);
    assert_eq!(variance, 5.0 / 3.0);
    assert_eq!(mean_variance([1.0, 2.0, 3.0, 4.0].into_iter(), 0.0).1, 1.25);
    assert_eq!(mean([2.0, 4.0].into_iter()), 3.0);
    assert!(mean(std::iter::empty()).is_nan());
}

/// The report's squares ([`centred_sum_of_squares`]'s, the kurtosis' `(n - 1)²`, `vike-analytics`'
/// ulcer index and [`crate::regression::slope_t_value`]'s residuals) left `libm::pow(x, 2.0)` for
/// [`square`] on 2026-11-19 and moved no bit: the `libm` crate's `pow` answers `x * x` for `y == 2`
/// before every other branch but `1**y` (`1.0`, the same) and a NaN argument (a NaN either way).
/// Held over the specials, every scale and the subnormals, through random bit patterns.
#[test]
fn square_is_libm_pow_of_two_bit_for_bit() {
    let check = |x: f64| {
        let (by_pow, by_multiply) = (libm::pow(x, 2.0), square(x));
        if x.is_nan() {
            assert!(by_pow.is_nan() && by_multiply.is_nan(), "{x:?}");
        } else {
            assert_eq!(by_pow.to_bits(), by_multiply.to_bits(), "{x:e}");
        }
    };
    let overflow_edge = f64::MAX.sqrt();
    let subnormal_edge = f64::MIN_POSITIVE.sqrt();
    let specials = [
        0.0,
        1.0,
        2.0,
        0.5,
        f64::EPSILON,
        f64::MIN_POSITIVE,
        f64::from_bits(1),
        f64::from_bits(0x000f_ffff_ffff_ffff),
        f64::MAX,
        overflow_edge,
        f64::from_bits(overflow_edge.to_bits() + 1),
        subnormal_edge,
        f64::from_bits(subnormal_edge.to_bits() - 1),
        f64::INFINITY,
        f64::NAN,
    ];
    for x in specials {
        check(x);
        check(-x);
    }
    let mut generator = crate::random::SplitMix64::new(0x5eed);
    for _ in 0..1_000_000 {
        let bits = generator.next_u64();
        check(f64::from_bits(bits)); // any pattern: every scale, the infinities, NaN payloads
        check(f64::from_bits(bits & 0x800f_ffff_ffff_ffff)); // a subnormal of either sign
        check((generator.next_unit() - 0.5) * 0.2); // a return-sized value
    }
}

/// The downside variance by hand: below a target of 1, `[0, 1, 3, -1]` falls short by 1, 0, 0 and 2, so
/// the squares sum to 5, over 4 or over 3; a series never below its target has none.
#[test]
fn downside_variance_by_hand() {
    let xs = [0.0, 1.0, 3.0, -1.0];
    assert_eq!(downside_variance(&xs, 1.0, 0.0), 1.25);
    assert_eq!(downside_variance(&xs, 1.0, 1.0), 5.0 / 3.0);
    assert_eq!(downside_variance(&[2.0, 3.0], 1.0, 1.0), 0.0);
}

/// The covariance is symmetric to the bit, and its diagonal is the variance.
#[test]
fn covariance_is_symmetric_and_its_diagonal_is_the_variance() {
    let a = [0.1, -0.3, 0.7, 0.2, -0.05];
    let b = [1.5, 0.25, -0.75, 2.0, 0.125];
    assert_eq!(covariance(&a, &b, 1.0).to_bits(), covariance(&b, &a, 1.0).to_bits());
    assert_eq!(covariance(&a, &a, 1.0).to_bits(), mean_variance(a.into_iter(), 1.0).1.to_bits());
}

/// The plain twin is the plain left fold, verbatim.
#[test]
fn the_plain_twin_is_the_plain_fold() {
    let x = [0.1, 0.2, 0.3, 1e16, -1e16, 0.7];
    let mean = x.iter().sum::<f64>() / x.len() as f64;
    assert_eq!(plain_mean(x.iter().copied()).to_bits(), mean.to_bits());
}

/// Pearson by hand, its degenerate answers, and the closing formula it shares.
#[test]
fn pearson_by_hand_and_its_degenerate_answers() {
    assert_eq!(pearson(&[1.0, 2.0, 3.0], &[2.0, 4.0, 6.0]), 1.0);
    assert_eq!(pearson(&[1.0, 2.0, 3.0], &[3.0, 2.0, 1.0]), -1.0);
    assert_eq!(pearson(&[1.0], &[2.0]), 0.0);
    assert_eq!(pearson(&[1.0, 1.0], &[2.0, 3.0]), 0.0);
    assert_eq!(correlation_of_moments(1.0, 4.0, 1.0), 0.5);
}

/// Sorted 1, 2, 2, 3 rank 1, 2.5, 2.5, 4: doubled 2, 5, 5, 8.
#[test]
fn doubled_ranks_average_ties() {
    assert_eq!(doubled_average_ranks(&[2.0, 1.0, 3.0, 2.0]), vec![5, 2, 8, 5]);
}

/// The median averages the two middle values; the high median and the nearest rank answer an
/// observed one; each has no answer for an empty input.
#[test]
fn the_medians_and_the_nearest_rank_by_hand() {
    assert_eq!(median(vec![3.0, 1.0, 2.0]), Some(2.0));
    assert_eq!(median(vec![4.0, 1.0, 3.0, 2.0]), Some(2.5));
    assert_eq!(median(Vec::new()), None);
    assert_eq!(high_median_of_sorted(&[1.0, 2.0, 3.0, 4.0]), Some(3.0));
    assert_eq!(high_median_of_sorted(&[7_u64]), Some(7));
    assert_eq!(high_median_of_sorted::<i64>(&[]), None);
    let v = [0.0, 1.0, 2.0, 3.0];
    assert_eq!(nearest_rank_of_sorted(&v, 0.5), Some(2.0), "percentile would read 1.5");
    assert_eq!(nearest_rank_of_sorted(&v, 0.0), Some(0.0));
    assert_eq!(nearest_rank_of_sorted(&v, 1.0), Some(3.0));
    assert_eq!(nearest_rank_of_sorted(&v, 2.0), None);
    assert_eq!(nearest_rank_of_sorted::<f64>(&[], 0.5), None);
}

/// The z-score has no guard; the normalisation answers only for a finite positive total.
#[test]
fn the_z_score_and_the_unit_sum_by_hand() {
    assert_eq!(z_score(3.0, 1.0, 2.0), 1.0);
    assert!(z_score(1.0, 1.0, 0.0).is_nan());
    assert_eq!(normalised_to_unit_sum(&[1.0, 3.0]), Some(vec![0.25, 0.75]));
    let no_totals: [&[f64]; 5] = [&[], &[0.0, 0.0], &[1.0, -1.0], &[f64::INFINITY], &[f64::NAN]];
    for no_total in no_totals {
        assert_eq!(normalised_to_unit_sum(no_total), None, "{no_total:?}");
    }
    // The total is compensated: a plain fold would read 0 here and refuse.
    assert_eq!(normalised_to_unit_sum(&[1e16, 1.0, -1e16]), Some(vec![1e16, 1.0, -1e16]));
}

/// The correlation distance at its ends, past `1`, and for no correlation.
#[test]
fn the_correlation_distance_by_hand() {
    assert_eq!(correlation_distance(1.0), 0.0);
    assert_eq!(correlation_distance(-1.0), 1.0);
    assert_eq!(correlation_distance(0.0), 0.5_f64.sqrt());
    assert_eq!(correlation_distance(1.0 + f64::EPSILON), 0.0);
    assert!(correlation_distance(f64::NAN).is_nan());
}

/// Four bins of a quarter from zero: an edge belongs to the bin it opens, the top edge to the last
/// bin, and anything below the range or not a number to the first.
#[test]
fn the_equal_width_bin_by_hand() {
    let bin = |value| equal_width_bin(value, 0.0, 0.25, 4);
    assert_eq!([bin(0.0), bin(0.25), bin(0.5), bin(0.999), bin(1.0), bin(7.0)], [0, 1, 2, 3, 3, 3]);
    assert_eq!([bin(-1.0), bin(f64::NAN)], [0, 0]);
}
