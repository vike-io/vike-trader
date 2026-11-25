use super::*;

#[test]
fn a_series_shorter_than_two_has_no_tstat() {
    assert_eq!(newey_west_t_statistic(&[], 4), 0.0);
    assert_eq!(newey_west_t_statistic(&[1.0], 4), 0.0);
}

#[test]
fn a_zero_lag_reduces_to_the_ordinary_t_of_the_mean() {
    // lag 0 means no autocovariance terms: V = gamma_0 (population variance).
    let x: [f64; 4] = [1.0, 2.0, 3.0, 4.0];
    let n = x.len() as f64;
    let mean = 2.5;
    let var = x.iter().map(|v| (v - mean) * (v - mean)).sum::<f64>() / n;
    let expected = mean / (var / n).sqrt();
    assert!((newey_west_t_statistic(&x, 0) - expected).abs() < 1e-12);
}

#[test]
fn a_flat_series_has_no_variance_and_therefore_no_tstat() {
    assert_eq!(newey_west_t_statistic(&[3.0; 10], 4), 0.0);
}

#[test]
fn nans_are_dropped_not_propagated() {
    let clean = newey_west_t_statistic(&[1.0, 2.0, 3.0, 4.0], 2);
    let dirty = newey_west_t_statistic(&[1.0, f64::NAN, 2.0, 3.0, 4.0], 2);
    assert!((clean - dirty).abs() < 1e-12);
}

#[test]
fn the_bartlett_weight_and_the_factor_of_two_are_both_pinned() {
    // x = [1,2,3,4], lag 1: gamma0 = 1.25, gamma1 = 0.3125, weight = 1 - 1/2 = 0.5,
    // V = 1.25 + 2*0.5*0.3125 = 1.5625, t = 2.5 / sqrt(1.5625/4) = 4.0 exactly.
    let x: [f64; 4] = [1.0, 2.0, 3.0, 4.0];
    assert!((newey_west_t_statistic(&x, 1) - 4.0).abs() < 1e-12);
}

#[test]
fn the_long_run_variance_is_the_kpss_arithmetic() {
    // The KPSS denominator as `vike-indicators`' stationarity row wrote it before it was shared, inline:
    // the weights by the UNCAPPED bandwidth, each lag's term added to the running total.
    let kpss = |e: &[f64], lags: usize| {
        let t = e.len() as f64;
        let mut squares = 0.0;
        for v in e {
            squares += v * v;
        }
        let mut long_run = squares / t;
        for s in 1..=lags.min(e.len() - 1) {
            let mut covariance = 0.0;
            for i in s..e.len() {
                covariance += e[i] * e[i - s];
            }
            long_run += 2.0 * (1.0 - s as f64 / (lags as f64 + 1.0)) * covariance / t;
        }
        long_run
    };
    let x: Vec<f64> = (0..257).map(|i| ((i * 7919) % 101) as f64 * 0.013 - 0.6).collect();
    let mean = x.iter().sum::<f64>() / x.len() as f64;
    let centred: Vec<f64> = x.iter().map(|v| v - mean).collect();
    for lags in [0, 1, 5, 40, 256, 400] {
        assert_eq!(
            bartlett_long_run_variance(&centred, lags).to_bits(),
            kpss(&centred, lags).to_bits(),
            "lags {lags}"
        );
    }
    assert!(bartlett_long_run_variance(&[], 3).is_nan());
}

/// Re-derived with decision 0294 (REVIEW-LAWS H1): Newey–West's `V` IS the shared Bartlett long-run
/// variance, bit for bit, at every bandwidth, one at or past the series length included (where the
/// old copy capped the bandwidth before weighting and so answered differently).
#[test]
fn newey_west_is_the_mean_over_the_shared_long_run_deviation() {
    let x: Vec<f64> = (0..257).map(|i| ((i * 7919) % 101) as f64 * 0.013 - 0.6).collect();
    let n = x.len() as f64;
    let mean = x.iter().sum::<f64>() / n;
    let centred: Vec<f64> = x.iter().map(|v| v - mean).collect();
    for lag in [0, 1, 5, 40, 300] {
        let want = mean / (bartlett_long_run_variance(&centred, lag) / n).sqrt();
        assert_eq!(newey_west_t_statistic(&x, lag).to_bits(), want.to_bits(), "lag {lag}");
    }
}

#[test]
fn the_autocorrelation_of_a_small_series_is_the_hand_computed_one() {
    // x = [1, 2, 3, 4]: centred [-1.5, -0.5, 0.5, 1.5], gamma_0 = 5/4; gamma_1 = 1.25/4,
    // gamma_2 = -1.5/4, gamma_3 = -2.25/4, so r = 0.25, -0.3, -0.45; lag 4 has no pair.
    let r = autocorrelation(&[1.0, 2.0, 3.0, 4.0], 4);
    for (got, want) in r[..3].iter().zip([0.25, -0.3, -0.45]) {
        assert!((got - want).abs() < 1e-15, "{r:?}");
    }
    assert!(r[3].is_nan(), "{r:?}");
}

#[test]
fn an_alternating_series_is_anticorrelated_at_odd_lags() {
    let x: Vec<f64> = (0..100).map(|i| if i % 2 == 0 { 1.0 } else { -1.0 }).collect();
    let r = autocorrelation(&x, 2);
    assert!((r[0] + 0.99).abs() < 1e-12 && (r[1] - 0.98).abs() < 1e-12, "{r:?}");
}

#[test]
fn a_flat_or_empty_series_has_no_autocorrelation() {
    assert!(autocorrelation(&[2.0; 8], 3).iter().all(|r| r.is_nan()));
    assert_eq!(autocorrelation(&[], 2).len(), 2);
    assert!(autocorrelation(&[], 2).iter().all(|r| r.is_nan()));
    assert!(autocorrelation(&[1.0, 2.0], 0).is_empty());
}

/// Two bins, half in each: one bit of the most there is. Three to one: the binary entropy of a
/// quarter, `0.8112781244591328`. A window of one return is 0, a non-finite one `NaN`.
#[test]
fn shannon_entropy_by_hand() {
    let mut counts = vec![0; 2];
    let ln2 = libm::log(2.0);
    assert!((shannon_entropy_of_bins(&[0.0, 1.0, 2.0, 3.0], &mut counts, ln2) - 1.0).abs() < 1e-15);
    assert!(
        (shannon_entropy_of_bins(&[0.0, 0.0, 0.0, 1.0], &mut counts, ln2)
            - 0.811_278_124_459_132_8)
            .abs()
            < 1e-15
    );
    assert_eq!(shannon_entropy_of_bins(&[0.25; 6], &mut counts, ln2), 0.0);
    assert!(shannon_entropy_of_bins(&[0.0, f64::INFINITY, 1.0], &mut counts, ln2).is_nan());
    assert!(shannon_entropy_of_bins(&[0.0, 1.0], &mut [0], 0.0).is_nan());
}

/// Kaspar and Schuster's worked example, `0·001·10·100·1000·101`, and the two regular extremes.
#[test]
fn lempel_ziv_by_hand() {
    let bits = |s: &str| s.bytes().map(|b| f64::from(b - b'0')).collect::<Vec<f64>>();
    assert_eq!(lempel_ziv_complexity(&bits("0001101001000101")), 6);
    assert_eq!(lempel_ziv_complexity(&bits("0000000000")), 2);
    assert_eq!(lempel_ziv_complexity(&bits("0101010101")), 3);
    assert_eq!(lempel_ziv_complexity(&bits("1")), 1);
}

fn near(got: f64, want: f64) -> bool {
    (got - want).abs() <= 1e-14 * want.abs().max(1.0)
}

/// `y` is `x` one step later at the one move both share: `ρ(1) = corr([0,1,0,0], [0,1,0,0]) = 1`;
/// `ρ(0) = -0.6 / sqrt(3.2 · 0.8) = -0.375`; `ρ(-1) = corr([1,0,0,2], [0,0,1,0]) = -0.75 /
/// sqrt(2.75 · 0.75)`, so the ratio is `1 / (0.5625 / 2.0625) = 11 / 3`.
#[test]
fn a_lead_lag_by_hand() {
    let (x, y) = ([0.0, 1.0, 0.0, 0.0, 2.0], [0.0, 0.0, 1.0, 0.0, 0.0]);
    let pair = lead_lag(&x, &y, 1).unwrap();
    let back = -0.75 / 2.0625f64.sqrt();
    assert!(
        near(pair.correlations[0], back)
            && near(pair.correlations[1], -0.375)
            && near(pair.correlations[2], 1.0),
        "{pair:?}"
    );
    assert_eq!((pair.peak_lag, pair.peak_correlation), (1, pair.correlations[2]));
    assert!(near(pair.lead_lag_ratio, 11.0 / 3.0), "{pair:?}");
    // Swapping the two mirrors every lag bit for bit.
    let swapped = lead_lag(&y, &x, 1).unwrap();
    let mirrored: Vec<f64> = pair.correlations.iter().rev().copied().collect();
    assert_eq!(swapped.correlations, mirrored);
    assert_eq!(swapped.peak_lag, -1);
    assert!(near(swapped.lead_lag_ratio, 3.0 / 11.0), "{swapped:?}");
}

#[test]
fn no_lead_lag_without_an_answer() {
    let x = [0.0, 1.0, 0.0, 2.0];
    assert!(lead_lag(&x, &x[..3], 1).is_none(), "lengths differ");
    assert!(lead_lag(&x, &x, 0).is_none(), "no lag");
    assert!(lead_lag(&x, &x, 3).is_none(), "one pair left");
    assert!(lead_lag(&x, &[0.0, f64::NAN, 0.0, 1.0], 1).is_none(), "not finite");
    // A series that never moves correlates zero at every lag: neither leads.
    let flat = lead_lag(&x, &[1.0; 4], 1).unwrap();
    assert_eq!((flat.correlations, flat.peak_lag, flat.lead_lag_ratio), (vec![0.0; 3], 0, 1.0));
}

/// **The guard is `peak > 0`, nothing else**: a zero, negative-zero, negative or `NaN` peak has no
/// fraction (a `peak != 0.0` guard would measure the negative one), and a positive peak answers
/// `(peak - value) / peak` exactly: zero at the peak, a negative fraction above it.
#[test]
fn a_drawdown_is_measured_only_from_a_positive_peak() {
    for peak in [0.0, -0.0, -100.0, f64::NEG_INFINITY, f64::NAN] {
        assert_eq!(drawdown_fraction(peak, -150.0), None, "peak {peak}");
    }
    assert_eq!(drawdown_fraction(1_000.0, 800.0), Some(0.2));
    assert_eq!(drawdown_fraction(1_000.0, 1_000.0), Some(0.0), "at the peak");
    assert_eq!(drawdown_fraction(100.0, 110.0), Some(-0.1), "above the peak: not clamped");
    assert_eq!(drawdown_fraction(1_000.0, 0.0), Some(1.0), "everything lost");
    assert!(drawdown_fraction(1_000.0, f64::NAN).is_some_and(f64::is_nan), "a NaN value is NaN");
}

/// **The operand order is the contract**: the answer is `(peak - value) / peak`'s bits, and the
/// algebraically equal `1 - value / peak` parts from it on this grid, which is why a reader that spelled
/// it that way moves its numbers when it moves here.
#[test]
fn a_drawdown_keeps_its_operand_order_bit_for_bit() {
    let mut other_spelling_differs = 0;
    for peak in 1..=200 {
        for value in 0..=peak {
            let (peak, value) = (f64::from(peak) * 1.37, f64::from(value) * 0.91);
            let got = drawdown_fraction(peak, value).expect("a positive peak");
            assert_eq!(got.to_bits(), ((peak - value) / peak).to_bits(), "{peak} {value}");
            other_spelling_differs += usize::from(got.to_bits() != (1.0 - value / peak).to_bits());
        }
    }
    assert!(other_spelling_differs > 0, "the grid must show the two spellings parting");
}
