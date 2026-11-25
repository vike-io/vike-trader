//! The scoring rules and the reliability diagram by hand, the Brier decomposition's identity, and
//! the two fits checked against their definitions (Platt's likelihood equations, the pool adjacent
//! violators steps).

use super::*;
use vike_ml_prediction::Predictor;
use vike_model::test_support::corpus::lcg;

/// `rows` forecasts uniform in `[0, 1)`, each event happening with probability `truth(p)`.
fn draw(seed: u64, rows: usize, truth: impl Fn(f64) -> f64) -> (Vec<f64>, Vec<bool>) {
    let mut s = seed;
    let forecasts: Vec<f64> = (0..rows).map(|_| lcg(&mut s)).collect();
    let outcomes = forecasts.iter().map(|&p| lcg(&mut s) < truth(p)).collect();
    (forecasts, outcomes)
}

fn close(a: f64, b: f64, tolerance: f64) -> bool {
    (a - b).abs() <= tolerance
}

#[test]
fn brier_and_log_loss_by_hand() {
    let (forecasts, outcomes) = ([0.9, 0.2, 0.6], [true, false, false]);
    // (0.1² + 0.2² + 0.6²) / 3
    assert!(close(brier_score(&forecasts, &outcomes).unwrap(), 0.41 / 3.0, 1e-15));
    let want = -(libm::log(0.9) + libm::log(0.8) + libm::log(0.4)) / 3.0;
    assert!(close(log_loss(&forecasts, &outcomes).unwrap(), want, 1e-15));
    // A certain forecast that is wrong: the log loss is infinite, the Brier score is 1.
    assert_eq!(log_loss(&[0.0], &[true]), Ok(f64::INFINITY));
    assert_eq!(log_loss(&[1.0], &[true]), Ok(0.0));
    assert_eq!(brier_score(&[0.0], &[true]), Ok(1.0));
}

#[test]
fn the_inputs_are_refused_by_name() {
    assert!(brier_score(&[0.5], &[true, false]).unwrap_err().contains("one outcome per forecast"));
    assert!(log_loss(&[], &[]).is_err());
    for p in [-0.1, 1.5, f64::NAN] {
        assert!(brier_score(&[0.5, p], &[true, false]).unwrap_err().contains("forecast 1"), "{p}");
    }
    assert!(reliability_bins(&[0.5], &[true], 0).is_err());
    assert!(fit_isotonic(&[], &[]).is_err());
}

#[test]
fn reliability_bins_and_the_calibration_error_by_hand() {
    let forecasts = [0.25, 0.25, 0.75, 0.75, 1.0, 0.05];
    let outcomes = [false, true, true, true, true, false];
    let bins = reliability_bins(&forecasts, &outcomes, 4).unwrap();
    let rows: Vec<(f64, f64, usize)> = bins.iter().map(|b| (b.lower, b.upper, b.count)).collect();
    assert_eq!(rows, [(0.0, 0.25, 1), (0.25, 0.5, 2), (0.5, 0.75, 0), (0.75, 1.0, 3)]);
    assert_eq!((bins[1].mean_forecast, bins[1].observed_frequency), (0.25, 0.5));
    assert!(bins[2].mean_forecast.is_nan() && bins[2].observed_frequency.is_nan());
    assert!(close(bins[3].mean_forecast, 2.5 / 3.0, 1e-15));
    assert_eq!(bins[3].observed_frequency, 1.0);
    // (1·|0 − 0.05| + 2·|0.5 − 0.25| + 3·|1 − 5/6|) / 6
    let want = (0.05 + 0.5 + 0.5) / 6.0;
    assert!(close(expected_calibration_error(&bins), want, 1e-15));
}

#[test]
fn the_brier_decomposition_by_hand_and_its_identity_on_any_forecasts() {
    // One forecast per bin: the within-bin terms vanish and Murphy's three terms are the score.
    let forecasts = [0.25, 0.25, 0.75, 0.75];
    let outcomes = [false, true, true, true];
    let d = brier_decomposition(&forecasts, &outcomes, 2).unwrap();
    assert_eq!((d.reliability, d.resolution, d.uncertainty), (0.0625, 0.0625, 0.1875));
    assert_eq!((d.within_bin_variance, d.within_bin_covariance), (0.0, 0.0));
    assert_eq!(brier_score(&forecasts, &outcomes), Ok(0.1875));

    // Any forecasts: the five terms add up to the score.
    let (forecasts, outcomes) = draw(7, 2_000, |p| p * p);
    for bin_count in [1, 5, 10, 37] {
        let d = brier_decomposition(&forecasts, &outcomes, bin_count).unwrap();
        let sum = d.reliability - d.resolution + d.uncertainty + d.within_bin_variance
            - 2.0 * d.within_bin_covariance;
        let score = brier_score(&forecasts, &outcomes).unwrap();
        assert!(close(sum, score, 1e-12), "{bin_count} bins: {sum} against {score}");
    }
}

#[test]
fn platt_solves_its_likelihood_equations_and_recovers_a_known_distortion() {
    // The truth is a Platt map of the forecast: slope 0.6, intercept 0.3 in log-odds.
    let truth = Calibration::Platt { slope: 0.6, intercept: 0.3 };
    let (forecasts, outcomes) = draw(11, 20_000, |p| truth.apply(p).unwrap());
    let fit = fit_platt(&forecasts, &outcomes, 0.0).unwrap();
    let Calibration::Platt { slope, intercept } = fit else { panic!("{fit:?}") };
    assert!(close(slope, 0.6, 0.05) && close(intercept, 0.3, 0.05), "{slope} {intercept}");
    // The maximum-likelihood equations: Σ (o − p') = 0 and Σ (o − p') · log_odds(p) = 0.
    let (mut level, mut tilt) = (0.0, 0.0);
    for (&p, &o) in forecasts.iter().zip(&outcomes) {
        let residual = f64::from(u8::from(o)) - fit.apply(p).unwrap();
        level += residual;
        tilt += residual * log_odds(p);
    }
    let n = forecasts.len() as f64;
    assert!(level.abs() / n < 1e-9 && tilt.abs() / n < 1e-9, "{level} {tilt}");
}

#[test]
fn platt_refuses_one_outcome_and_separable_rows_without_a_penalty() {
    assert!(fit_platt(&[0.2, 0.8], &[true, true], 0.0).is_err());
    let (forecasts, outcomes) = ([0.2, 0.3, 0.7, 0.8], [false, false, true, true]);
    assert!(fit_platt(&forecasts, &outcomes, 0.0).unwrap_err().contains("penalty"));
    let fit = fit_platt(&forecasts, &outcomes, 0.1).unwrap();
    let Calibration::Platt { slope, .. } = fit else { panic!("{fit:?}") };
    assert!(slope > 0.0, "{slope}");
}

#[test]
fn isotonic_pools_adjacent_violators_by_hand() {
    // 0.2 → 1 then 0.3 → 0 violate and pool to one half; ties pool before anything else.
    let fit = fit_isotonic(&[0.4, 0.2, 0.1, 0.3], &[true, true, false, false]).unwrap();
    assert_eq!(
        fit,
        Calibration::Isotonic { starts: vec![0.1, 0.2, 0.4], values: vec![0.0, 0.5, 1.0] }
    );
    let fit = fit_isotonic(&[0.3, 0.5, 0.5], &[true, true, false]).unwrap();
    assert_eq!(fit, Calibration::Isotonic { starts: vec![0.3], values: vec![2.0 / 3.0] });
    // One outcome only: one step at its rate.
    let fit = fit_isotonic(&[0.9, 0.1], &[false, false]).unwrap();
    assert_eq!(fit, Calibration::Isotonic { starts: vec![0.1], values: vec![0.0] });
}

#[test]
fn isotonic_is_monotone_and_beats_the_raw_forecasts_on_its_own_rows() {
    let (forecasts, outcomes) = draw(5, 3_000, |p| 0.2 + 0.6 * p * p);
    let fit = fit_isotonic(&forecasts, &outcomes).unwrap();
    fit.check().unwrap();
    let calibrated: Vec<f64> = forecasts.iter().map(|&p| fit.predict_one(&[p]).unwrap()).collect();
    let raw = brier_score(&forecasts, &outcomes).unwrap();
    let after = brier_score(&calibrated, &outcomes).unwrap();
    // The identity map is non-decreasing, so the best such map scores at most what it scores.
    assert!(after <= raw, "{after} > {raw}");
    // The order of the rows does not change the fit.
    let reversed = |v: &[f64]| v.iter().rev().copied().collect::<Vec<_>>();
    let outcomes_reversed: Vec<bool> = outcomes.iter().rev().copied().collect();
    assert_eq!(fit_isotonic(&reversed(&forecasts), &outcomes_reversed).unwrap(), fit);
}
