use super::*;
use crate::random::SplitMix64;
use crate::test_support::normal;

/// Bollerslev's recurrence by hand, in binary fractions so every product and sum is exact.
#[test]
fn the_step_is_the_recurrence() {
    // 0.25 + 0.5 * 0.5^2 + 0.25 * 2 = 0.875
    assert_eq!(next_variance(0.25, 0.5, 0.25, 0.5, 2.0), 0.875);
    assert_eq!(next_variance(0.0, 0.0, 0.0, 3.0, 7.0), 0.0);
}

/// The start is the unconditional variance for a stationary model and `r[1]^2` otherwise.
#[test]
fn the_start_is_the_unconditional_variance_or_the_first_square() {
    // 0.125 / (1 - 0.75) = 0.5
    assert_eq!(starting_variance(0.125, 0.25, 0.5, 9.0), 0.5);
    // alpha + beta = 1: integrated, no unconditional variance.
    assert_eq!(starting_variance(0.0, 0.25, 0.75, 0.5), 0.25);
    assert_eq!(starting_variance(0.0, 0.75, 0.75, -2.0), 4.0);
}

/// `n` returns of the model, through [`next_variance`], started at the unconditional variance.
fn simulate(truth: GarchParameters, n: usize, seed: u64) -> Vec<f64> {
    let mut rng = SplitMix64::new(seed);
    let GarchParameters { omega, alpha, beta } = truth;
    let mut variance = omega / (1.0 - alpha - beta);
    (0..n)
        .map(|_| {
            let r = variance.sqrt() * normal(&mut rng);
            variance = next_variance(omega, alpha, beta, r, variance);
            r
        })
        .collect()
}

const TRUTH: GarchParameters = GarchParameters { omega: 0.000001, alpha: 0.1, beta: 0.85 };

/// 20,000 simulated returns give back the model's parameters (standard errors there are about
/// 0.007 for alpha and 0.012 for beta), and the fit's likelihood is at least the truth's.
#[test]
fn recovers_the_parameters_of_a_simulated_series() {
    let returns = simulate(TRUTH, 20_000, 11);
    let fitted = fit(&returns).expect("a long finite series fits");
    let p = fitted.parameters;
    assert!((p.alpha - TRUTH.alpha).abs() < 0.03, "{fitted:?}");
    assert!((p.beta - TRUTH.beta).abs() < 0.05, "{fitted:?}");
    let unconditional = p.omega / (1.0 - p.alpha - p.beta);
    assert!((unconditional / 0.00002 - 1.0).abs() < 0.15, "{fitted:?}: {unconditional}");
    assert!(p.alpha + p.beta < 1.0 && p.alpha >= 0.0 && p.beta >= 0.0 && p.omega > 0.0);
    let at_truth = TRUTH.log_likelihood(&returns);
    assert!(fitted.log_likelihood >= at_truth - 1e-9 * at_truth.abs(), "{fitted:?} vs {at_truth}");
    assert_eq!(fit(&returns), Some(fitted), "the fit is deterministic");
}

#[test]
fn refuses_what_cannot_be_fitted() {
    assert_eq!(fit(&[0.01; 9]), None, "too short");
    assert_eq!(fit(&[0.0; 50]), None, "no variation at all");
    let mut gap = vec![0.01; 50];
    gap[7] = f64::NAN;
    assert_eq!(fit(&gap), None, "a missing return");
}

/// The forecasts start at the unconditional variance, or at `r[1]^2` for an integrated model.
#[test]
fn variances_start_where_the_row_starts() {
    let returns = [0.01, -0.02, 0.005];
    let v = TRUTH.variances(&returns);
    assert_eq!(v.len(), 4);
    assert_eq!(v[0], 0.000001 / (1.0 - (0.1 + 0.85)));
    // `(1 - 0.94) + 0.94` is exactly 1 (Sterbenz), so the model has no unconditional variance.
    let integrated = GarchParameters { omega: 0.0, alpha: 1.0 - 0.94, beta: 0.94 };
    assert_eq!(integrated.variances(&returns)[0], 0.01 * 0.01);
    assert!(
        GarchParameters { omega: 0.0, alpha: 0.0, beta: 0.0 }.log_likelihood(&returns)
            == f64::NEG_INFINITY
    );
}
