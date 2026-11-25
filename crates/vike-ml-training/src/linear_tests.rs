//! The linear fits against hand-worked answers and their optimality (KKT) conditions.

use super::*;
use crate::search::DEFAULT_POINT;
use vike_model::test_support::corpus::lcg;

/// `rows` rows of `columns` uniform features in `[-1, 1)` and the target `f(row) + noise · u`.
fn synthetic(
    rows: usize,
    columns: usize,
    seed: u64,
    noise: f64,
    f: impl Fn(&[f64]) -> f64,
) -> (Vec<f64>, Vec<f32>) {
    let mut s = seed;
    let mut x = Vec::with_capacity(rows * columns);
    let mut y = Vec::with_capacity(rows);
    for _ in 0..rows {
        let row: Vec<f64> = (0..columns).map(|_| 2.0 * lcg(&mut s) - 1.0).collect();
        y.push((f(&row) + noise * (2.0 * lcg(&mut s) - 1.0)) as f32);
        x.extend(row);
    }
    (x, y)
}

fn fit(x: &[f64], y: &[f32], columns: usize, spec: LinearSpec) -> LinearModel {
    fit_linear(&TrainData::new(x, y, columns).unwrap(), spec).unwrap()
}

fn close(a: f64, b: f64, tolerance: f64) -> bool {
    (a - b).abs() <= tolerance * (1.0 + b.abs())
}

/// Two centred, orthogonal columns with `(1/n) Σ x² = 1`, and a target `2 + 3 x₁ + 0.5 x₂ + r` whose
/// residual `r` is orthogonal to both and to the constant: the ordinary least squares answer is
/// exactly `(2, 3, 0.5)`, and every penalised answer has a closed form.
fn orthogonal() -> (Vec<f64>, Vec<f32>) {
    let x1 = [1.0, -1.0, 1.0, -1.0];
    let x2 = [1.0, 1.0, -1.0, -1.0];
    let r = [0.125, -0.125, -0.125, 0.125];
    let x = (0..4).flat_map(|i| [x1[i], x2[i]]).collect();
    let y = (0..4).map(|i| (2.0 + 3.0 * x1[i] + 0.5 * x2[i] + r[i]) as f32).collect();
    (x, y)
}

#[test]
fn ordinary_least_squares_recovers_an_exact_plane() {
    let (x, y) = synthetic(40, 3, 7, 0.0, |r| 1.0 + 2.0 * r[0] - 3.0 * r[1] + 0.25 * r[2]);
    let m = fit(&x, &y, 3, LinearSpec::OrdinaryLeastSquares);
    // The labels are f32, so the plane is exact only to f32's rounding of each label.
    for (got, want) in
        std::iter::once(m.intercept).chain(m.coefficients).zip([1.0, 2.0, -3.0, 0.25])
    {
        assert!(close(got, want, 1e-6), "{got} vs {want}");
    }
}

#[test]
fn ordinary_least_squares_refuses_a_design_with_no_unique_answer() {
    // The second column is twice the first.
    let x = [1.0, 2.0, 2.0, 4.0, 3.0, 6.0, 5.0, 10.0];
    let y = [1.0, 2.0, 3.0, 4.0];
    let error = fit_linear(&TrainData::new(&x, &y, 2).unwrap(), LinearSpec::OrdinaryLeastSquares);
    assert!(error.unwrap_err().contains("no unique answer"));
    // Ridge has one.
    assert!(
        fit_linear(&TrainData::new(&x, &y, 2).unwrap(), LinearSpec::Ridge { penalty: 0.1 }).is_ok()
    );
}

#[test]
fn every_penalty_on_an_orthogonal_design_matches_its_closed_form() {
    let (x, y) = orthogonal();
    let ols = fit(&x, &y, 2, LinearSpec::OrdinaryLeastSquares);
    assert!(close(ols.intercept, 2.0, 1e-14) && close(ols.coefficients[0], 3.0, 1e-14));
    assert!(close(ols.coefficients[1], 0.5, 1e-14));
    // With (1/n) Σ x_j² = 1 and orthogonal columns: Ridge b_j = b_ols / (1 + λ); Elastic Net
    // b_j = S(b_ols, λα) / (1 + λ(1 − α)); LASSO the soft threshold itself.
    let cases = [
        (LinearSpec::Ridge { penalty: 0.5 }, [3.0 / 1.5, 0.5 / 1.5]),
        (LinearSpec::ElasticNet { penalty: 1.0, l1_ratio: 1.0 }, [2.0, 0.0]),
        (LinearSpec::ElasticNet { penalty: 0.4, l1_ratio: 1.0 }, [2.6, 0.1]),
        (LinearSpec::ElasticNet { penalty: 1.0, l1_ratio: 0.5 }, [2.5 / 1.5, 0.0]),
        (LinearSpec::ElasticNet { penalty: 0.5, l1_ratio: 0.0 }, [3.0 / 1.5, 0.5 / 1.5]),
    ];
    for (spec, want) in cases {
        let m = fit(&x, &y, 2, spec);
        assert!(close(m.intercept, 2.0, 1e-12), "{spec:?}: intercept {}", m.intercept);
        for (got, want) in m.coefficients.iter().zip(want) {
            assert!(close(*got, want, 1e-12), "{spec:?}: {got} vs {want}");
        }
    }
}

#[test]
fn ridge_in_closed_form_and_by_coordinate_descent_agree() {
    let (x, y) = synthetic(60, 4, 11, 0.3, |r| 0.5 - r[0] + 2.0 * r[1] + 0.1 * r[3]);
    let closed = fit(&x, &y, 4, LinearSpec::Ridge { penalty: 0.2 });
    let descent = fit(&x, &y, 4, LinearSpec::ElasticNet { penalty: 0.2, l1_ratio: 0.0 });
    assert!(close(closed.intercept, descent.intercept, 1e-9));
    for (a, b) in closed.coefficients.iter().zip(&descent.coefficients) {
        assert!(close(*a, *b, 1e-9), "{a} vs {b}");
    }
}

/// The optimality conditions of `(1/2n) Σ r² + λ (α ‖b‖₁ + (1 − α)/2 ‖b‖²)` — and, with `r = y − p`,
/// of the logistic objective: `g_j = (1/n) Σ x_ij r_i` equals `λ(1 − α) b_j + λα sign(b_j)` where
/// `b_j ≠ 0` and has `|g_j| <= λα` where `b_j = 0`; the residuals sum to zero (the free intercept).
fn assert_optimal(d: &TrainData<'_>, m: &LinearModel, penalty: f64, l1_ratio: f64, tolerance: f64) {
    let n = d.n_rows as f64;
    let mut gradient = vec![0.0; d.n_cols];
    let mut residual_sum = 0.0;
    for i in 0..d.n_rows {
        let r = f64::from(d.y[i]) - m.predict_one(d.row(i)).unwrap();
        residual_sum += r;
        for (g, x) in gradient.iter_mut().zip(d.row(i)) {
            *g += x * r / n;
        }
    }
    assert!(residual_sum.abs() / n < tolerance, "the intercept is not optimal: {residual_sum}");
    for (j, (g, b)) in gradient.iter().zip(&m.coefficients).enumerate() {
        let ridge = g - penalty * (1.0 - l1_ratio) * b;
        if *b == 0.0 {
            assert!(ridge.abs() <= penalty * l1_ratio + tolerance, "b_{j} = 0 but |g| = {ridge}");
        } else {
            let want = penalty * l1_ratio * b.signum();
            assert!((ridge - want).abs() < tolerance, "b_{j} = {b}: {ridge} vs {want}");
        }
    }
}

#[test]
fn the_elastic_net_meets_its_optimality_conditions_and_lasso_zeroes_the_noise() {
    let (x, y) = synthetic(200, 6, 3, 0.5, |r| 1.0 + 3.0 * r[0] - 2.0 * r[1]);
    let d = TrainData::new(&x, &y, 6).unwrap();
    for (penalty, l1_ratio) in [(0.05, 1.0), (0.2, 1.0), (0.1, 0.5), (0.3, 0.2)] {
        let m = fit_linear(&d, LinearSpec::ElasticNet { penalty, l1_ratio }).unwrap();
        assert_optimal(&d, &m, penalty, l1_ratio, 1e-9);
    }
    let lasso = fit_linear(&d, LinearSpec::ElasticNet { penalty: 0.2, l1_ratio: 1.0 }).unwrap();
    assert!(lasso.coefficients[0] > 2.0 && lasso.coefficients[1] < -1.0, "{lasso:?}");
    assert!(lasso.coefficients[2..].iter().all(|&b| b == 0.0), "{lasso:?}");
}

/// Labels drawn from a known logistic model, so the classes overlap.
fn logistic_data(rows: usize, seed: u64) -> (Vec<f64>, Vec<f32>) {
    let truth = LinearModel {
        link: LinearLink::Logistic,
        intercept: -0.5,
        coefficients: vec![2.0, -1.0, 0.0],
    };
    let mut s = seed;
    let mut x = Vec::with_capacity(rows * 3);
    let mut y = Vec::with_capacity(rows);
    for _ in 0..rows {
        let row: Vec<f64> = (0..3).map(|_| 2.0 * lcg(&mut s) - 1.0).collect();
        let p = truth.predict_one(&row).unwrap();
        y.push(if lcg(&mut s) < p { 1.0 } else { 0.0 });
        x.extend(row);
    }
    (x, y)
}

#[test]
fn logistic_regression_meets_its_optimality_conditions_under_l2_and_l1() {
    let (x, y) = logistic_data(400, 5);
    let d = TrainData::new(&x, &y, 3).unwrap();
    for (penalty, l1_ratio) in [(0.0, 0.0), (0.01, 0.0), (0.02, 1.0), (0.05, 0.5)] {
        let m = fit_linear(&d, LinearSpec::Logistic { penalty, l1_ratio }).unwrap();
        assert_eq!(m.link, LinearLink::Logistic);
        assert_optimal(&d, &m, penalty, l1_ratio, 1e-8);
    }
    let unpenalised = fit_linear(&d, LinearSpec::Logistic { penalty: 0.0, l1_ratio: 0.0 }).unwrap();
    assert!(
        unpenalised.coefficients[0] > 1.0 && unpenalised.coefficients[1] < -0.3,
        "{unpenalised:?}"
    );
}

#[test]
fn a_large_l1_penalty_leaves_only_the_base_rate() {
    let (x, y) = logistic_data(300, 9);
    let d = TrainData::new(&x, &y, 3).unwrap();
    let m = fit_linear(&d, LinearSpec::Logistic { penalty: 10.0, l1_ratio: 1.0 }).unwrap();
    assert!(m.coefficients.iter().all(|&b| b == 0.0), "{m:?}");
    let rate = y.iter().map(|&v| f64::from(v)).sum::<f64>() / y.len() as f64;
    assert!(close(m.intercept, libm::log(rate / (1.0 - rate)), 1e-9), "{}", m.intercept);
}

#[test]
fn separable_classes_with_no_penalty_are_refused() {
    let x = [-2.0, -1.0, 1.0, 2.0];
    let y = [0.0, 0.0, 1.0, 1.0];
    let d = TrainData::new(&x, &y, 1).unwrap();
    let error = fit_linear(&d, LinearSpec::Logistic { penalty: 0.0, l1_ratio: 0.0 }).unwrap_err();
    assert!(error.contains("separable"), "{error}");
    assert!(fit_linear(&d, LinearSpec::Logistic { penalty: 0.1, l1_ratio: 0.0 }).is_ok());
}

#[test]
fn bad_data_and_bad_specs_are_refused() {
    let x = [1.0, 2.0, 3.0, 4.0];
    let y = [1.0, 0.0, 1.0, 0.0];
    let d = TrainData::new(&x, &y, 1).unwrap();
    for spec in [
        LinearSpec::Ridge { penalty: -1.0 },
        LinearSpec::Ridge { penalty: f64::NAN },
        LinearSpec::ElasticNet { penalty: 1.0, l1_ratio: 1.5 },
        LinearSpec::Logistic { penalty: 1.0, l1_ratio: f64::NAN },
    ] {
        assert!(fit_linear(&d, spec).is_err(), "{spec:?}");
    }
    let categorical = d.with_categorical(&[0]).unwrap();
    assert!(
        fit_linear(&categorical, LinearSpec::OrdinaryLeastSquares).unwrap_err().contains("one-hot")
    );
    let x_nan = [1.0, f64::NAN, 3.0, 4.0];
    assert!(
        fit_linear(&TrainData::new(&x_nan, &y, 1).unwrap(), LinearSpec::OrdinaryLeastSquares)
            .is_err()
    );
    let y_three = [1.0, 0.0, 2.0, 0.0];
    let three = TrainData::new(&x, &y_three, 1).unwrap();
    assert!(fit_linear(&three, LinearSpec::Logistic { penalty: 1.0, l1_ratio: 0.0 }).is_err());
}

#[test]
fn the_learner_keys_its_fit_on_the_spec_and_the_data_only() {
    let (x, y) = synthetic(30, 2, 1, 0.1, |r| r[0] + r[1]);
    let d = TrainData::new(&x, &y, 2).unwrap();
    let learner = LinearLearner { spec: LinearSpec::Ridge { penalty: 0.1 } };
    let other_point = GridPoint { learning_rate: 0.5, ..DEFAULT_POINT };
    let identity = learner.fit_identity(&d, &DEFAULT_POINT, 0).unwrap();
    assert_eq!(
        learner.fit_identity(&d, &other_point, 99),
        Some(identity),
        "point and seed are not read"
    );
    let lasso = LinearLearner { spec: LinearSpec::ElasticNet { penalty: 0.1, l1_ratio: 1.0 } };
    assert_ne!(lasso.fit_identity(&d, &DEFAULT_POINT, 0), Some(identity));
    let (x2, y2) = synthetic(30, 2, 2, 0.1, |r| r[0] + r[1]);
    assert_ne!(
        learner.fit_identity(&TrainData::new(&x2, &y2, 2).unwrap(), &DEFAULT_POINT, 0),
        Some(identity)
    );
    let categorical = d.with_categorical(&[1]).unwrap();
    assert_eq!(
        learner.fit_identity(&categorical, &DEFAULT_POINT, 0),
        None,
        "a refused fit has no identity"
    );
}

#[test]
fn the_captured_text_is_the_fitted_model_bit_for_bit_and_a_refit_is_identical() {
    let (x, y) = logistic_data(200, 4);
    let d = TrainData::new(&x, &y, 3).unwrap();
    let learner = LinearLearner { spec: LinearSpec::Logistic { penalty: 0.01, l1_ratio: 0.5 } };
    let want = Capture { importance: true, text: true };
    let (model, importance, text) = learner.fit_captured(&d, &DEFAULT_POINT, 0, want).unwrap();
    assert_eq!(importance, None);
    let back = LinearModel::from_text(&text.unwrap()).unwrap();
    let bits = |m: &LinearModel| -> Vec<u64> {
        std::iter::once(m.intercept)
            .chain(m.coefficients.iter().copied())
            .map(f64::to_bits)
            .collect()
    };
    assert_eq!(bits(&back), bits(&model));
    assert_eq!(bits(&learner.fit(&d, &DEFAULT_POINT, 1).unwrap()), bits(&model), "deterministic");
    assert_eq!(learner.fit_captured(&d, &DEFAULT_POINT, 0, Capture::default()).unwrap().2, None);
}
