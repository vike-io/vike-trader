//! Bagging and stacking against fits done by hand from the same pieces.

use super::*;
use crate::gaussian_process::{GaussianProcessLearner, GaussianProcessParams};
use crate::linear::{LinearLearner, LinearSpec, fit_linear};
use crate::search::DEFAULT_POINT;
use vike_ml_prediction::linear::LinearModel;
use vike_model::test_support::corpus::lcg;

fn data(rows: usize, seed: u64) -> (Vec<f64>, Vec<f32>) {
    let mut s = seed;
    let mut x = Vec::with_capacity(rows * 2);
    let mut y = Vec::with_capacity(rows);
    for _ in 0..rows {
        let (a, b) = (2.0 * lcg(&mut s) - 1.0, 2.0 * lcg(&mut s) - 1.0);
        x.extend([a, b]);
        y.push((1.0 + 2.0 * a - b + 0.3 * a * b + 0.2 * (lcg(&mut s) - 0.5)) as f32);
    }
    (x, y)
}

/// `count` contiguous test folds, each trained on every other row.
fn contiguous_folds(rows: usize, count: usize) -> Vec<(Vec<usize>, Vec<usize>)> {
    (0..count)
        .map(|k| {
            let (start, end) = (k * rows / count, (k + 1) * rows / count);
            ((0..rows).filter(|&i| i < start || i >= end).collect(), (start..end).collect())
        })
        .collect()
}

fn bits(m: &LinearModel) -> Vec<u64> {
    std::iter::once(m.intercept).chain(m.coefficients.iter().copied()).map(f64::to_bits).collect()
}

fn ols() -> AsAnyModel<LinearLearner> {
    AsAnyModel(LinearLearner { spec: LinearSpec::OrdinaryLeastSquares })
}

#[test]
fn a_bagged_model_is_the_mean_of_one_fit_per_bag_and_its_text_predicts_the_same_bits() {
    let (x, y) = data(30, 1);
    let d = TrainData::new(&x, &y, 2).unwrap();
    let member = ols();
    let bags = vec![
        (0..20).collect::<Vec<_>>(),
        (10..30).collect(),
        vec![0, 0, 5, 7, 9, 11, 13, 21, 22, 29],
    ];
    let bagging = BaggingLearner { member: &member, bags: bags.clone() };
    let want = Capture { importance: false, text: true };
    let (ensemble, _, text) = bagging.fit_captured(&d, &DEFAULT_POINT, 3, want).unwrap();
    for (bag, fitted) in bags.iter().zip(ensemble.members()) {
        let (bx, by) = subset(&d, bag, "bag").unwrap();
        let by_hand =
            fit_linear(&TrainData::new(&bx, &by, 2).unwrap(), LinearSpec::OrdinaryLeastSquares)
                .unwrap();
        let AnyModel::Linear(fitted) = fitted else { panic!("a linear member: {fitted:?}") };
        assert_eq!(bits(fitted), bits(&by_hand));
    }
    let back = AnyModel::from_text(&text.unwrap()).unwrap();
    for i in 0..d.n_rows {
        let row = d.row(i);
        let mut mean = 0.0;
        for member in ensemble.members() {
            mean += member.predict_one(row).unwrap();
        }
        mean /= 3.0;
        assert_eq!(ensemble.predict_one(row).unwrap().to_bits(), mean.to_bits());
        assert_eq!(back.predict_one(row).unwrap().to_bits(), mean.to_bits());
    }
}

/// A learner of `AnyModel`s that proves no identity — the shape of the seam's test double.
struct Unkeyed;

impl Learner for Unkeyed {
    type Model = AnyModel;
    fn fit(&self, d: &TrainData<'_>, _p: &GridPoint, _seed: u64) -> Result<AnyModel, String> {
        fit_linear(d, LinearSpec::OrdinaryLeastSquares).map(AnyModel::from)
    }
}

#[test]
fn a_bagged_identity_covers_every_bag_and_needs_every_members_identity() {
    let (x, y) = data(20, 2);
    let d = TrainData::new(&x, &y, 2).unwrap();
    let member = ols();
    let identity = |bags: Vec<Vec<usize>>| {
        BaggingLearner { member: &member, bags }.fit_identity(&d, &DEFAULT_POINT, 0)
    };
    let one = identity(vec![(0..10).collect(), (5..20).collect()]);
    assert!(one.is_some());
    assert_eq!(identity(vec![(0..10).collect(), (5..20).collect()]), one);
    assert_ne!(identity(vec![(0..10).collect(), (4..20).collect()]), one);
    assert_eq!(identity(vec![(0..10).collect(), vec![20]]), None, "a row past the data");
    assert_eq!(identity(Vec::new()), None);
    let unkeyed = BaggingLearner { member: &Unkeyed, bags: vec![(0..10).collect()] };
    assert_eq!(unkeyed.fit_identity(&d, &DEFAULT_POINT, 0), None);
    assert!(unkeyed.fit(&d, &DEFAULT_POINT, 0).is_ok(), "...and it still fits");
    let empty = BaggingLearner { member: &member, bags: Vec::new() };
    assert!(empty.fit(&d, &DEFAULT_POINT, 0).is_err());
    let past = BaggingLearner { member: &member, bags: vec![vec![0, 1, 2, 99]] };
    assert!(past.fit(&d, &DEFAULT_POINT, 0).unwrap_err().contains("row 99"));
}

#[test]
fn stacking_fits_the_meta_model_on_out_of_fold_predictions_by_hand() {
    let (x, y) = data(36, 4);
    let d = TrainData::new(&x, &y, 2).unwrap();
    let ridge_spec = LinearSpec::Ridge { penalty: 0.5 };
    let (first, second, meta) = (ols(), AsAnyModel(LinearLearner { spec: ridge_spec }), ols());
    let folds = contiguous_folds(36, 3);
    let stacking =
        StackingLearner { members: vec![&first, &second], meta: &meta, folds: folds.clone() };
    let want = Capture { importance: false, text: true };
    let (ensemble, _, text) = stacking.fit_captured(&d, &DEFAULT_POINT, 0, want).unwrap();

    let specs = [LinearSpec::OrdinaryLeastSquares, ridge_spec];
    let mut out_of_fold = vec![[0.0; 2]; 36];
    for (train, test) in &folds {
        let (tx, ty) = subset(&d, train, "fold").unwrap();
        for (m, spec) in specs.iter().enumerate() {
            let model = fit_linear(&TrainData::new(&tx, &ty, 2).unwrap(), *spec).unwrap();
            for &i in test {
                out_of_fold[i][m] = model.predict_one(d.row(i)).unwrap();
            }
        }
    }
    let meta_x: Vec<f64> = out_of_fold.concat();
    let meta_by_hand =
        fit_linear(&TrainData::new(&meta_x, &y, 2).unwrap(), LinearSpec::OrdinaryLeastSquares)
            .unwrap();
    let Some(AnyModel::Linear(meta_fitted)) = ensemble.meta() else {
        panic!("a linear meta model")
    };
    assert_eq!(bits(meta_fitted), bits(&meta_by_hand));
    for (member, spec) in ensemble.members().iter().zip(specs) {
        let AnyModel::Linear(member) = member else { panic!("a linear member") };
        assert_eq!(bits(member), bits(&fit_linear(&d, spec).unwrap()), "refitted on all rows");
    }
    let back = AnyModel::from_text(&text.unwrap()).unwrap();
    for i in 0..36 {
        let row = d.row(i);
        let stacked = meta_by_hand
            .predict_one(&[
                ensemble.members()[0].predict_one(row).unwrap(),
                ensemble.members()[1].predict_one(row).unwrap(),
            ])
            .unwrap();
        assert_eq!(ensemble.predict_one(row).unwrap().to_bits(), stacked.to_bits());
        assert_eq!(back.predict_one(row).unwrap().to_bits(), stacked.to_bits());
    }
    assert_eq!(stacking.fit_identity(&d, &DEFAULT_POINT, 0), None, "a stacked fit is never cached");
}

#[test]
fn stacking_takes_members_of_different_kinds() {
    let (x, y) = data(24, 6);
    let d = TrainData::new(&x, &y, 2).unwrap();
    let process = AsAnyModel(GaussianProcessLearner {
        params: GaussianProcessParams {
            length_scale: 0.8,
            signal_variance: 1.0,
            noise_variance: 0.05,
        },
    });
    let (linear, meta) = (ols(), ols());
    let stacking = StackingLearner {
        members: vec![&process, &linear],
        meta: &meta,
        folds: contiguous_folds(24, 4),
    };
    let want = Capture { importance: false, text: true };
    let (ensemble, _, text) = stacking.fit_captured(&d, &DEFAULT_POINT, 0, want).unwrap();
    assert!(matches!(ensemble.members()[0], AnyModel::GaussianProcess(_)));
    let back = AnyModel::from_text(&text.unwrap()).unwrap();
    for i in 0..24 {
        let row = d.row(i);
        assert_eq!(
            back.predict_one(row).unwrap().to_bits(),
            ensemble.predict_one(row).unwrap().to_bits()
        );
    }
}

#[test]
fn stacking_refuses_folds_that_do_not_test_every_row_once() {
    let (x, y) = data(12, 5);
    let d = TrainData::new(&x, &y, 2).unwrap();
    let (member, meta) = (ols(), ols());
    let stacking = |folds| StackingLearner { members: vec![&member], meta: &meta, folds };
    let mut twice = contiguous_folds(12, 3);
    twice[0].1.push(11);
    assert!(stacking(twice).fit(&d, &DEFAULT_POINT, 0).unwrap_err().contains("row 11"));
    let mut missing = contiguous_folds(12, 3);
    missing[2].1.pop();
    assert!(stacking(missing).fit(&d, &DEFAULT_POINT, 0).unwrap_err().contains("row 11"));
    let mut past = contiguous_folds(12, 3);
    past[1].1.push(40);
    assert!(stacking(past).fit(&d, &DEFAULT_POINT, 0).is_err());
    let no_members =
        StackingLearner { members: Vec::new(), meta: &meta, folds: contiguous_folds(12, 3) };
    assert!(no_members.fit(&d, &DEFAULT_POINT, 0).is_err());
}
