use super::*;
use crate::random::SplitMix64;
use crate::test_support::normal;

#[test]
fn the_start_is_the_stationary_law() {
    // leave 0.5 and 0.25: 0.5 / 0.75
    assert_eq!(stationary([0.5, 0.75]), 0.5 / 0.75);
    assert_eq!(stationary([1.0, 1.0]), 0.5, "neither state is ever left");
    assert_eq!(stationary([1.0, 0.5]), 0.0, "state 0 is never left");
}

/// Two states that emit alike tell the filter nothing: the belief is the chain's prediction alone.
#[test]
fn alike_states_leave_the_prediction() {
    let model = TwoStateGaussian::new([0.0, 0.0], [0.5, 0.5], [0.75, 0.5]);
    let step = model.step(0.25, 0.3);
    // q1 = 0.75 * 0.25 + 0.25 * 0.5 = 0.3125, both densities scale to 1.
    assert_eq!(step.scaled_density, [1.0, 1.0]);
    assert_eq!(step.total, 1.0);
    assert_eq!(step.filtered, 0.3125);
}

/// Hamilton's update against the two Gaussian densities written out.
#[test]
fn the_update_weighs_the_prediction_by_the_densities() {
    let (mean, sigma, stay) = ([0.0, 0.01], [0.01, 0.03], [0.98, 0.95]);
    let model = TwoStateGaussian::new(mean, sigma, stay);
    let (p, r) = (0.4, 0.02);
    let density = |i: usize| {
        let z = (r - mean[i]) / sigma[i];
        libm::exp(-0.5 * z * z) / sigma[i]
    };
    let q1 = (1.0 - p) * (1.0 - stay[0]) + p * stay[1];
    let want = q1 * density(1) / ((1.0 - q1) * density(0) + q1 * density(1));
    let got = model.step(p, r).filtered;
    assert!((got - want).abs() < 1e-12, "{got} vs {want}");
}

/// A belief that is certain stays certain when the return rules nothing in.
#[test]
fn a_certainty_is_kept() {
    let model = TwoStateGaussian::new([0.0, 0.0], [0.000001, 1.0], [0.5, 1.0]);
    // Far in state 0's tail, so state 1's weight is the larger; but the belief is already in state 1.
    let step = model.step(1.0, 0.5);
    assert_eq!(step.filtered, 1.0);
    // Now the other way: state 0 ruled out, state 1's density underflows against state 0's scale.
    let model = TwoStateGaussian::new([0.0, 5.0], [1.0, 0.000001], [0.5, 1.0]);
    let step = model.step(1.0, 0.0);
    assert_eq!(step.total, 0.0);
    assert_eq!(step.filtered, 1.0, "no weight left: the prediction stands");
}

const TRUTH: HmmParameters =
    HmmParameters { mean: [0.0005, -0.001], sigma: [0.01, 0.03], stay: [0.98, 0.95] };

/// `n` returns of the model and the states that drew them, the chain started at its stationary law.
fn simulate(truth: HmmParameters, n: usize, seed: u64) -> (Vec<f64>, Vec<usize>) {
    let mut rng = SplitMix64::new(seed);
    let mut state = usize::from(rng.next_unit() < truth.stationary());
    let mut returns = Vec::with_capacity(n);
    let mut states = Vec::with_capacity(n);
    for _ in 0..n {
        if rng.next_unit() >= truth.stay[state] {
            state = 1 - state;
        }
        states.push(state);
        returns.push(truth.mean[state] + truth.sigma[state] * normal(&mut rng));
    }
    (returns, states)
}

/// 20,000 simulated returns give back the model (about 5,700 of them from state 1), its likelihood
/// is at least the truth's, and Viterbi decodes most of the true path.
#[test]
fn recovers_a_simulated_regime_model_and_its_path() {
    let (returns, states) = simulate(TRUTH, 20_000, 3);
    let fitted = fit(&returns).expect("a long finite series fits");
    let p = fitted.parameters;
    assert!(fitted.converged, "{fitted:?}");
    for i in 0..2 {
        assert!((p.sigma[i] / TRUTH.sigma[i] - 1.0).abs() < 0.1, "sigma {i}: {fitted:?}");
        assert!((p.stay[i] - TRUTH.stay[i]).abs() < 0.02, "stay {i}: {fitted:?}");
    }
    assert!((p.mean[0] - TRUTH.mean[0]).abs() < 0.001, "{fitted:?}");
    assert!((p.mean[1] - TRUTH.mean[1]).abs() < 0.002, "{fitted:?}");
    let at_truth = TRUTH.log_likelihood(&returns);
    assert!(fitted.log_likelihood >= at_truth - 1e-9 * at_truth.abs(), "{fitted:?} vs {at_truth}");
    let path = viterbi(&p, &returns).expect("finite returns decode");
    let agree = path.iter().zip(&states).filter(|(a, b)| a == b).count();
    assert!(
        agree as f64 >= 0.9 * states.len() as f64,
        "Viterbi agrees on {agree} of {}",
        states.len()
    );
    assert_eq!(fit(&returns), Some(fitted), "the fit is deterministic");
}

/// Started with the states the wrong way round, the fit still names the calm state 0.
#[test]
fn the_calm_state_is_state_zero() {
    let (returns, _) = simulate(TRUTH, 5_000, 8);
    let swapped = HmmParameters { mean: [0.0; 2], sigma: [0.04, 0.005], stay: [0.9, 0.9] };
    let fitted = fit_from(&returns, swapped).expect("fits");
    assert!(fitted.parameters.sigma[0] < fitted.parameters.sigma[1], "{fitted:?}");
    let ll = fitted.parameters.log_likelihood(&returns);
    assert_eq!(ll.to_bits(), fitted.log_likelihood.to_bits());
}

/// Viterbi by hand on two returns: a tiny one then a huge one, with sticky states, reads calm then
/// turbulent.
#[test]
fn viterbi_reads_a_calm_bar_then_a_turbulent_one() {
    let model = HmmParameters { mean: [0.0, 0.0], sigma: [0.01, 0.1], stay: [0.9, 0.9] };
    assert_eq!(viterbi(&model, &[0.001, 0.3]), Some(vec![0, 1]));
    assert_eq!(viterbi(&model, &[f64::NAN]), None);
    assert_eq!(viterbi(&model, &[]), Some(vec![]));
}

#[test]
fn refuses_what_cannot_be_fitted() {
    assert_eq!(fit(&[0.01]), None, "one return");
    assert_eq!(fit(&[0.01, f64::NAN, 0.02]), None, "a missing return");
}
