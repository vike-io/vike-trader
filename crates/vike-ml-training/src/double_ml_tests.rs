//! Double machine learning against hand-worked scores, against simulated data with a KNOWN effect
//! (the interval's coverage over many seeds), and on a confounded case where a naive regression is
//! biased and the estimate is not.

use super::*;
use crate::ensemble::AsAnyModel;
use crate::gaussian_process::{GaussianProcessLearner, GaussianProcessParams};
use crate::linear::{LinearLearner, LinearSpec, fit_linear};
use crate::search::DEFAULT_POINT;
use vike_ml_prediction::PredictionKind;
use vike_ml_prediction::any_model::AnyModel;
use vike_model::test_support::corpus::lcg;

/// A nuisance double that predicts the mean of its training labels, so every out-of-fold value in
/// the hand-worked cases is a number written in the test.
struct MeanLearner;

struct Constant(f64);

impl Predictor for Constant {
    fn kind(&self) -> PredictionKind {
        PredictionKind::Value
    }
    fn predict_into(&self, _row: &[f64], out: &mut [f64]) -> Result<(), String> {
        out.fill(self.0);
        Ok(())
    }
}

impl Learner for MeanLearner {
    type Model = Constant;
    fn fit(&self, d: &TrainData<'_>, _p: &GridPoint, _seed: u64) -> Result<Constant, String> {
        Ok(Constant(d.y.iter().map(|&v| f64::from(v)).sum::<f64>() / d.n_rows as f64))
    }
}

fn cross(folds: &[(Vec<usize>, Vec<usize>)]) -> CrossFitting<'_> {
    CrossFitting { folds, point: &DEFAULT_POINT, seed: 0 }
}

/// Fold `k` tests the rows `i % count == k` and trains on the rest.
fn interleaved(rows: usize, count: usize) -> Vec<(Vec<usize>, Vec<usize>)> {
    (0..count)
        .map(|k| {
            let (train, test): (Vec<usize>, Vec<usize>) = (0..rows).partition(|i| i % count != k);
            (train, test)
        })
        .collect()
}

/// A standard-normal draw: the quantile of a uniform one.
fn normal(s: &mut u64) -> f64 {
    normal_quantile(lcg(s).max(f64::MIN_POSITIVE))
}

fn ols() -> LinearLearner {
    LinearLearner { spec: LinearSpec::OrdinaryLeastSquares }
}

#[test]
fn the_partially_linear_estimate_is_the_hand_worked_one() {
    let treatment = [1.0, 2.0, 4.0, 3.0];
    let outcome = [1.0, 3.0, 2.0, 6.0];
    let confounders = [0.0; 4]; // one column the mean double never reads
    let folds = vec![(vec![2, 3], vec![0, 1]), (vec![0, 1], vec![2, 3])];
    let data = DoubleMlData {
        confounders: &confounders,
        column_count: 1,
        treatment: &treatment,
        outcome: &outcome,
    };
    let e = partially_linear(&data, &MeanLearner, &MeanLearner, &cross(&folds)).unwrap();

    // Fold 0 predicts m̂ = 3.5 and ℓ̂ = 4 for rows 0 and 1, fold 1 m̂ = 1.5 and ℓ̂ = 2 for rows 2
    // and 3: D̃ = (−2.5, −1.5, 2.5, 1.5), Ỹ = (−3, −1, 0, 4), Σ D̃·Ỹ = 15, Σ D̃² = 17.
    let theta = 15.0 / 17.0;
    assert_eq!(e.effect, theta);
    assert_eq!(e.rows, 4);
    let treatment_residual = [-2.5, -1.5, 2.5, 1.5];
    let outcome_residual = [-3.0, -1.0, 0.0, 4.0];
    let mut mean_square_score = 0.0;
    for i in 0..4 {
        let score = (outcome_residual[i] - theta * treatment_residual[i]) * treatment_residual[i];
        mean_square_score += score * score / 4.0;
    }
    let derivative = 17.0 / 4.0; // mean(D̃²)
    let standard_error = (mean_square_score / (derivative * derivative) / 4.0).sqrt();
    assert!((e.standard_error - standard_error).abs() < 1e-15, "{e:?} vs {standard_error}");
    assert!((e.standard_error - 0.418_874_5).abs() < 1e-6, "{e:?}");

    let (low, high) = e.confidence_interval(0.95).unwrap();
    assert!((high - theta - 1.959963984540054 * standard_error).abs() < 1e-12);
    assert!(((theta - low) - (high - theta)).abs() < 1e-15, "symmetric about θ̂");
}

#[test]
fn the_interactive_estimate_is_the_hand_worked_one() {
    let treatment = [0.0, 1.0, 0.0, 1.0, 0.0, 1.0, 1.0, 0.0];
    let outcome = [1.0, 4.0, 2.0, 5.0, 0.0, 3.0, 6.0, 2.0];
    let confounders = [0.0; 8];
    let folds = vec![((4..8).collect(), (0..4).collect()), ((0..4).collect(), (4..8).collect())];
    let data = DoubleMlData {
        confounders: &confounders,
        column_count: 1,
        treatment: &treatment,
        outcome: &outcome,
    };
    let e = interactive(&data, &MeanLearner, &MeanLearner, &cross(&folds), 0.01).unwrap();

    // Fold 0 (trained on rows 4..8): g₀ = mean(0, 2) = 1, g₁ = mean(3, 6) = 4.5, m = 0.5.
    // Fold 1 (trained on rows 0..4): g₀ = mean(1, 2) = 1.5, g₁ = mean(4, 5) = 4.5, m = 0.5.
    // Scores g₁ − g₀ + D(Y − g₁)/m − (1 − D)(Y − g₀)/(1 − m) = (3.5, 2.5, 1.5, 4.5, 6, 0, 6, 2):
    // θ̂ = 26/8 = 3.25, Σ(ψ − θ̂)² = 32.5, σ̂² = 32.5/8, se = √(σ̂²/8). Every step is exact in binary.
    assert_eq!(e.effect, 3.25);
    assert_eq!(e.standard_error, (32.5f64 / 8.0 / 8.0).sqrt());
    assert_eq!(e.rows, 8);
}

/// `rows` rows of the partially linear model with θ = 0.5: three standard-normal confounders,
/// `D = 0.8x₁ − 0.5x₂ + 0.3x₃ + v` and `Y = θD + x₁ + 0.7x₂ − 0.4x₃ + ε`, `v` and `ε` standard
/// normal. Both nuisances are linear, so ordinary least squares is correctly specified for them.
fn linear_plr(rows: usize, seed: u64, theta: f64) -> (Vec<f64>, Vec<f64>, Vec<f64>) {
    let mut s = seed;
    let (mut x, mut d, mut y) = (Vec::new(), Vec::new(), Vec::new());
    for _ in 0..rows {
        let row = [normal(&mut s), normal(&mut s), normal(&mut s)];
        let treatment = 0.8 * row[0] - 0.5 * row[1] + 0.3 * row[2] + normal(&mut s);
        y.push(theta * treatment + row[0] + 0.7 * row[1] - 0.4 * row[2] + normal(&mut s));
        d.push(treatment);
        x.extend(row);
    }
    (x, d, y)
}

/// Over `estimates.len()` independent simulations of a known `theta`: the 95% interval's coverage
/// count, the mean estimate's distance from `theta` in units of its own standard error across
/// simulations, and the mean reported standard error over the empirical spread of the estimates.
fn coverage(estimates: &[DoubleMlEstimate], theta: f64) -> (usize, f64, f64) {
    let n = estimates.len() as f64;
    let covered = estimates
        .iter()
        .filter(|e| {
            let (low, high) = e.confidence_interval(0.95).unwrap();
            low <= theta && theta <= high
        })
        .count();
    let mean = estimates.iter().map(|e| e.effect).sum::<f64>() / n;
    let spread = (estimates.iter().map(|e| (e.effect - mean) * (e.effect - mean)).sum::<f64>()
        / (n - 1.0))
        .sqrt();
    let mean_standard_error = estimates.iter().map(|e| e.standard_error).sum::<f64>() / n;
    (covered, (mean - theta) / (spread / n.sqrt()), mean_standard_error / spread)
}

/// The bounds `coverage` is judged by, for 200 simulations at 95%: the covered count is
/// Binomial(200, 0.95), mean 190 and sd 3.08, so [181, 199] is ±3 sd; the mean estimate within 4
/// of its standard errors of θ; the reported standard error within 20% of the empirical spread,
/// whose own relative sd at 200 draws is 1/√398 = 5%.
fn assert_calibrated(what: &str, (covered, bias_in_errors, error_ratio): (usize, f64, f64)) {
    assert!(
        (181..=199).contains(&covered),
        "{what}: the 95% interval covered θ {covered}/200 times"
    );
    assert!(
        bias_in_errors.abs() < 4.0,
        "{what}: the mean estimate is {bias_in_errors} errors off θ"
    );
    assert!((0.8..1.2).contains(&error_ratio), "{what}: se / spread = {error_ratio}");
}

#[test]
fn the_partially_linear_interval_covers_a_known_effect_at_its_stated_rate() {
    const THETA: f64 = 0.5;
    let folds = interleaved(400, 5);
    let estimates: Vec<DoubleMlEstimate> = (0..200)
        .map(|seed| {
            let (x, d, y) = linear_plr(400, 1_000 + seed, THETA);
            let data =
                DoubleMlData { confounders: &x, column_count: 3, treatment: &d, outcome: &y };
            partially_linear(&data, &ols(), &ols(), &cross(&folds)).unwrap()
        })
        .collect();
    assert_calibrated("partially linear", coverage(&estimates, THETA));
}

/// `rows` rows of the interactive model with a constant effect θ = 1: two standard-normal
/// confounders, `P(D = 1 | X) = 1 / (1 + exp(−(0.6x₁ − 0.6x₂)))` and `Y = θD + x₁ + 0.5x₂ + ε`.
/// Each arm's `E[Y | X]` is linear and the propensity logistic, so the nuisances are correctly
/// specified.
fn binary_treatment(rows: usize, seed: u64, theta: f64) -> (Vec<f64>, Vec<f64>, Vec<f64>) {
    let mut s = seed;
    let (mut x, mut d, mut y) = (Vec::new(), Vec::new(), Vec::new());
    for _ in 0..rows {
        let row = [normal(&mut s), normal(&mut s)];
        let propensity = vike_math::special_functions::logistic(0.6 * row[0] - 0.6 * row[1]);
        let treatment = if lcg(&mut s) < propensity { 1.0 } else { 0.0 };
        y.push(theta * treatment + row[0] + 0.5 * row[1] + normal(&mut s));
        d.push(treatment);
        x.extend(row);
    }
    (x, d, y)
}

#[test]
fn the_interactive_interval_covers_a_known_effect_at_its_stated_rate() {
    const THETA: f64 = 1.0;
    let folds = interleaved(600, 5);
    let logistic = LinearLearner { spec: LinearSpec::Logistic { penalty: 0.0, l1_ratio: 0.0 } };
    let estimates: Vec<DoubleMlEstimate> = (0..200)
        .map(|seed| {
            let (x, d, y) = binary_treatment(600, 5_000 + seed, THETA);
            let data =
                DoubleMlData { confounders: &x, column_count: 2, treatment: &d, outcome: &y };
            interactive(&data, &ols(), &logistic, &cross(&folds), 0.01).unwrap()
        })
        .collect();
    assert_calibrated("interactive", coverage(&estimates, THETA));
}

/// A confounder that drives both: `x` uniform on [−2, 2), `D = sin(1.5x) + 0.5v` and
/// `Y = θD + 2 sin(1.5x) + 0.5x² + 0.5ε`. Regressing `Y` on `D` alone loads the `2 sin(1.5x)` term
/// onto `D`: its slope converges to `θ + 2·var(sin 1.5x)/var(D) ≈ θ + 1.35`.
fn confounded(rows: usize, seed: u64, theta: f64) -> (Vec<f64>, Vec<f64>, Vec<f64>) {
    let mut s = seed;
    let (mut x, mut d, mut y) = (Vec::new(), Vec::new(), Vec::new());
    for _ in 0..rows {
        let confounder = 4.0 * lcg(&mut s) - 2.0;
        let wave = libm::sin(1.5 * confounder);
        let treatment = wave + 0.5 * normal(&mut s);
        y.push(
            theta * treatment + 2.0 * wave + 0.5 * confounder * confounder + 0.5 * normal(&mut s),
        );
        d.push(treatment);
        x.push(confounder);
    }
    (x, d, y)
}

/// The naive slope is biased by the confounder; double machine learning with Gaussian-process
/// nuisances (a `dyn` learner, so any model goes through the same call) recovers θ.
#[test]
fn a_confounded_effect_biases_the_naive_slope_and_not_the_estimate() {
    const THETA: f64 = 0.5;
    const SEEDS: u64 = 12;
    let gp = AsAnyModel(GaussianProcessLearner {
        params: GaussianProcessParams {
            length_scale: 0.7,
            signal_variance: 4.0,
            noise_variance: 0.3,
        },
    });
    let nuisance: &dyn Learner<Model = AnyModel> = &gp;
    let folds = interleaved(200, 5);
    let (mut covered, mut naive_sum, mut estimate_sum) = (0, 0.0, 0.0);
    for seed in 0..SEEDS {
        let (x, d, y) = confounded(200, 9_000 + seed, THETA);
        let y32: Vec<f32> = y.iter().map(|&v| v as f32).collect();
        let naive =
            fit_linear(&TrainData::new(&d, &y32, 1).unwrap(), LinearSpec::OrdinaryLeastSquares)
                .unwrap()
                .coefficients[0];
        let data = DoubleMlData { confounders: &x, column_count: 1, treatment: &d, outcome: &y };
        let e = partially_linear(&data, nuisance, nuisance, &cross(&folds)).unwrap();
        let (low, high) = e.confidence_interval(0.95).unwrap();
        covered += usize::from(low <= THETA && THETA <= high);
        // The naive slope sits far outside the estimate's own interval.
        assert!(naive > high + 0.5, "seed {seed}: naive {naive}, interval ({low}, {high})");
        naive_sum += naive;
        estimate_sum += e.effect;
    }
    let (naive_mean, estimate_mean) = (naive_sum / SEEDS as f64, estimate_sum / SEEDS as f64);
    assert!((naive_mean - THETA - 1.35).abs() < 0.25, "naive mean {naive_mean}");
    assert!((estimate_mean - THETA).abs() < 0.1, "estimate mean {estimate_mean}");
    // Binomial(12, 0.95): P(covered <= 8) is below 0.3%.
    assert!(covered >= 9, "covered θ {covered}/{SEEDS} times");
}

#[test]
fn folds_that_would_leak_or_skip_a_row_are_refused() {
    let (x, d, y) = linear_plr(6, 1, 0.5);
    let data = DoubleMlData { confounders: &x, column_count: 3, treatment: &d, outcome: &y };
    let run = |folds: Vec<(Vec<usize>, Vec<usize>)>| {
        partially_linear(&data, &MeanLearner, &MeanLearner, &cross(&folds)).unwrap_err()
    };
    let leak = run(vec![(vec![0, 1, 2, 3], vec![0, 1, 2]), (vec![0, 1, 2], vec![3, 4, 5])]);
    assert!(leak.contains("fold 0 trains on row 0, which it also tests"), "{leak}");
    let skipped = run(vec![(vec![3, 4, 5], vec![0, 1, 2]), (vec![0, 1, 2], vec![3, 4])]);
    assert!(skipped.contains("row 5 is in 0 test folds"), "{skipped}");
    let twice = run(vec![(vec![3, 4, 5], vec![0, 1, 2, 3]), (vec![0, 1, 2], vec![3, 4, 5])]);
    assert!(twice.contains("row 3 is in 2 test folds"), "{twice}");
    let outside = run(vec![(vec![3, 4, 5, 9], vec![0, 1, 2]), (vec![0, 1, 2], vec![3, 4, 5])]);
    assert!(outside.contains("fold 0 trains on row 9 of 6 rows"), "{outside}");
}

#[test]
fn shapes_and_parameters_out_of_range_are_refused() {
    let (x, d, y) = linear_plr(6, 2, 0.5);
    let folds = interleaved(6, 2);
    let plr = |data: DoubleMlData<'_>| {
        partially_linear(&data, &MeanLearner, &MeanLearner, &cross(&folds)).unwrap_err()
    };
    let short =
        plr(DoubleMlData { confounders: &x, column_count: 3, treatment: &d[..5], outcome: &y });
    assert!(short.contains("6 outcomes but 5 treatments"), "{short}");
    let ragged =
        plr(DoubleMlData { confounders: &x[..17], column_count: 3, treatment: &d, outcome: &y });
    assert!(ragged.contains("17 confounder values for 6 rows of 3 columns"), "{ragged}");
    let mut missing = y.clone();
    missing[2] = f64::NAN;
    let nan =
        plr(DoubleMlData { confounders: &x, column_count: 3, treatment: &d, outcome: &missing });
    assert!(nan.contains("must be finite"), "{nan}");

    // A treatment the confounders explain exactly (here: constant) identifies nothing.
    let constant = [2.0; 6];
    let flat =
        plr(DoubleMlData { confounders: &x, column_count: 3, treatment: &constant, outcome: &y });
    assert!(flat.contains("not identified"), "{flat}");

    let binary = [0.0, 1.0, 0.0, 1.0, 0.0, 1.0];
    let data = DoubleMlData { confounders: &x, column_count: 3, treatment: &binary, outcome: &y };
    let irm = |data: &DoubleMlData<'_>, trim: f64| {
        interactive(data, &MeanLearner, &MeanLearner, &cross(&folds), trim).unwrap_err()
    };
    assert!(irm(&data, 0.5).contains("trim 0.5 is not in [0, 0.5)"));
    assert!(irm(&data, -0.1).contains("is not in [0, 0.5)"));
    let continuous = DoubleMlData { treatment: &d, ..data };
    assert!(irm(&continuous, 0.01).contains("is not 0 or 1"), "{}", irm(&continuous, 0.01));
    // Interleaved halves of an alternating treatment: fold 0 trains on the odd rows, all treated.
    let no_control = irm(&data, 0.01);
    assert!(
        no_control.contains("the control outcome nuisance: fold 0 has no training row"),
        "{no_control}"
    );

    let e = DoubleMlEstimate { effect: 1.0, standard_error: 0.1, rows: 6 };
    for level in [0.0, 1.0, -0.5, f64::NAN] {
        assert!(e.confidence_interval(level).is_err(), "level {level}");
    }
}

/// A nuisance learner's refusal comes back naming the nuisance and the fold.
#[test]
fn a_learner_refusal_names_the_nuisance_and_the_fold() {
    let (x, d, y) = linear_plr(6, 3, 0.5);
    // Two identical columns: ordinary least squares refuses the rank-deficient design.
    let doubled: Vec<f64> = x.chunks(3).flat_map(|r| [r[0], r[0]]).collect();
    let data = DoubleMlData { confounders: &doubled, column_count: 2, treatment: &d, outcome: &y };
    let folds = interleaved(6, 2);
    let error = partially_linear(&data, &ols(), &MeanLearner, &cross(&folds)).unwrap_err();
    assert!(error.starts_with("the outcome nuisance, fold 0: "), "{error}");
}
