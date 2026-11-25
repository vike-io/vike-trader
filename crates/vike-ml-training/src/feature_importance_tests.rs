use super::*;

use vike_analytics::validation::purged_kfold_label_spans;
use vike_ml_prediction::PredictionKind;

use crate::seam::learner::Learner;
use crate::seam::test_support::ScriptedLearner;
use crate::search::DEFAULT_POINT;

/// Answers 0.9 when feature 0 is positive, 0.1 otherwise; reads no other feature.
struct SignOfFirst;

impl Predictor for SignOfFirst {
    fn kind(&self) -> PredictionKind {
        PredictionKind::Probability
    }
    fn predict_into(&self, row: &[f64], out: &mut [f64]) -> Result<(), String> {
        out[0] = if row[0] > 0.0 { 0.9 } else { 0.1 };
        Ok(())
    }
}

/// A fixed answer of any kind, for the scorer's cases.
struct Fixed(PredictionKind, Vec<f64>);

impl Predictor for Fixed {
    fn kind(&self) -> PredictionKind {
        self.0
    }
    fn predict_into(&self, _row: &[f64], out: &mut [f64]) -> Result<(), String> {
        out.copy_from_slice(&self.1);
        Ok(())
    }
}

fn probability(p: f64) -> Fixed {
    Fixed(PredictionKind::Probability, vec![p])
}

#[test]
fn the_log_loss_scores_the_true_class_probability_clipped() {
    let x = [0.0, 0.0];
    // Label 1 at p = 0.9 and label 0 at p = 0.9: ln 0.9 and ln 0.1.
    let got = score_of(&probability(0.9), &x, 1, &[1.0, 0.0], Scoring::NegativeLogLoss).unwrap();
    assert_eq!(got, (libm::log(0.9) + libm::log(1.0 - 0.9)) / 2.0);
    // A certain miss is clipped, not -inf.
    let got = score_of(&probability(1.0), &x[..1], 1, &[0.0], Scoring::NegativeLogLoss).unwrap();
    assert_eq!(got, libm::log(LOG_LOSS_CLIP));
    // Multiclass: the true class's own probability.
    let three = Fixed(PredictionKind::ClassProbabilities { class_count: 3 }, vec![0.2, 0.5, 0.3]);
    let got = score_of(&three, &x[..1], 1, &[2.0], Scoring::NegativeLogLoss).unwrap();
    assert_eq!(got, libm::log(0.3));
}

#[test]
fn accuracy_predicts_the_most_probable_class_and_a_tie_goes_to_the_first() {
    let x = [0.0; 4];
    let labels = [1.0, 1.0, 0.0, 0.0];
    assert_eq!(score_of(&probability(0.75), &x, 1, &labels, Scoring::Accuracy), Ok(0.5));
    // p = 0.5 is a tie between the two classes: class 0, as `argmax` breaks it.
    assert_eq!(score_of(&probability(0.5), &x, 1, &labels, Scoring::Accuracy), Ok(0.5));
    assert_eq!(score_of(&probability(0.5), &x[..2], 1, &labels[..2], Scoring::Accuracy), Ok(0.0));
    let tie = Fixed(PredictionKind::ClassProbabilities { class_count: 3 }, vec![0.4, 0.4, 0.2]);
    assert_eq!(score_of(&tie, &x[..2], 1, &[0.0, 1.0], Scoring::Accuracy), Ok(0.5));
}

#[test]
fn a_regression_model_a_foreign_label_and_a_nan_prediction_are_not_scored_as_numbers() {
    let x = [0.0];
    let value = Fixed(PredictionKind::Value, vec![1.5]);
    assert!(score_of(&value, &x, 1, &[1.0], Scoring::Accuracy).unwrap_err().contains("regression"));
    for label in [2.0, 0.5, -1.0] {
        let refusal = score_of(&probability(0.5), &x, 1, &[label], Scoring::Accuracy).unwrap_err();
        assert!(refusal.contains("not a class"), "{label}: {refusal}");
    }
    let nan = score_of(&probability(f64::NAN), &x, 1, &[1.0], Scoring::Accuracy).unwrap();
    assert!(nan.is_nan());
}

/// 40 rows: feature 0 decides the label (its sign), feature 1 is noise, feature 2 is constant.
fn signal_noise_constant() -> (Vec<f64>, Vec<f32>) {
    let mut x = Vec::new();
    let mut y = Vec::new();
    for i in 0..40u32 {
        let signal = (if i % 2 == 0 { 1.0 } else { -1.0 }) * f64::from(i + 1);
        x.extend_from_slice(&[signal, f64::from(i * 7 % 11), 4.0]);
        y.push(if signal > 0.0 { 1.0 } else { 0.0 });
    }
    (x, y)
}

/// Labels spanning three bars each, purged by B2's splitter: the folds this module is built for.
fn purged_folds(n: usize) -> Vec<(Vec<usize>, Vec<usize>)> {
    let spans: Vec<(usize, usize)> = (0..n).map(|i| (i, i + 3)).collect();
    purged_kfold_label_spans(&spans, 4, 1)
}

#[test]
fn mda_finds_the_signal_and_gives_an_unread_or_constant_column_exactly_nothing() {
    let (x, y) = signal_noise_constant();
    let data = TrainData::new(&x, &y, 3).unwrap();
    let folds = purged_folds(data.n_rows);
    let mut fits = 0;
    let fit = |d: &TrainData<'_>| {
        fits += 1;
        assert!(d.n_rows < 40 && d.n_cols == 3, "a fold's training rows, every column");
        Ok(Box::new(SignOfFirst))
    };
    let (importance, baseline) =
        mean_decrease_accuracy(&data, &folds, Scoring::NegativeLogLoss, 7, fit).unwrap();
    assert_eq!(fits, 4, "one fit per fold, never one per permutation");
    assert_eq!(importance.per_fold.len(), 4);
    for fold in &importance.per_fold {
        assert!(fold[0] > 0.0, "permuting the signal must hurt: {fold:?}");
        assert_eq!((fold[1], fold[2]), (0.0, 0.0), "unread and constant columns");
    }
    assert!((baseline - libm::log(0.9)).abs() < 1e-15, "every row scores ln 0.9: {baseline}");
    // Same seed, same numbers.
    let again =
        mean_decrease_accuracy(&data, &folds, Scoring::NegativeLogLoss, 7, |_: &TrainData<'_>| {
            Ok(Box::new(SignOfFirst))
        })
        .unwrap();
    assert_eq!(again, (importance, baseline));
}

/// With a perfect unpermuted score, AFML's accuracy normalisation `(1 - s) / (1 - s)` is exactly
/// one whenever the permutation broke anything.
#[test]
fn mda_by_accuracy_normalises_against_the_permuted_score() {
    let (x, y) = signal_noise_constant();
    let data = TrainData::new(&x, &y, 3).unwrap();
    let folds = purged_folds(data.n_rows);
    let (importance, baseline) =
        mean_decrease_accuracy(&data, &folds, Scoring::Accuracy, 1, |_: &TrainData<'_>| {
            Ok(Box::new(SignOfFirst))
        })
        .unwrap();
    assert_eq!(baseline, 1.0);
    assert_eq!(importance.mean[0], 1.0);
    assert_eq!(importance.standard_error[0], 0.0);
    // The model still scores perfectly with a column it never reads permuted: 0 / 0, as in AFML.
    assert!(importance.mean[1].is_nan());
}

/// Fits the training labels' mean as a constant probability, and checks it was handed ONE column.
fn mean_of_labels(d: &TrainData<'_>) -> Result<Box<dyn Predictor>, String> {
    assert_eq!(d.n_cols, 1, "SFI fits one feature at a time");
    let mean = d.y.iter().map(|&label| f64::from(label)).sum::<f64>() / d.n_rows as f64;
    Ok(Box::new(probability(mean)))
}

/// Hand-computed. Labels `[1, 0, 1, 1]`, folds `{0,1}` and `{2,3}`. Fold 0 trains on labels
/// `1, 1` (p = 1, predicts 1) and meets `1, 0`: accuracy 1/2. Fold 1 trains on `1, 0` (p = 0.5,
/// a tie, predicts 0) and meets `1, 1`: accuracy 0. Mean 1/4; standard deviation `√(1/8)`, over
/// `√2`: standard error 1/4.
#[test]
fn sfi_scores_each_feature_alone_by_hand() {
    let x = [0.0, 10.0, 1.0, 11.0, 2.0, 12.0, 3.0, 13.0];
    let y = [1.0, 0.0, 1.0, 1.0];
    let data = TrainData::new(&x, &y, 2).unwrap();
    let folds = vec![(vec![2, 3], vec![0, 1]), (vec![0, 1], vec![2, 3])];
    let got = single_feature_importance(&data, &folds, Scoring::Accuracy, mean_of_labels).unwrap();
    assert_eq!(got.per_fold, vec![vec![0.5, 0.5], vec![0.0, 0.0]]);
    assert_eq!(got.mean, vec![0.25, 0.25]);
    for se in got.standard_error {
        assert!((se - 0.25).abs() < 1e-16, "{se}");
    }
}

#[test]
fn sfi_keeps_a_categorical_column_categorical_in_its_one_column_fit() {
    let x = [0.0, 1.0, 1.0, 2.0, 0.0, 3.0, 1.0, 0.0];
    let y = [1.0, 0.0, 1.0, 1.0];
    let data = TrainData::new(&x, &y, 2).unwrap().with_categorical(&[1]).unwrap();
    let folds = vec![(vec![2, 3], vec![0, 1]), (vec![0, 1], vec![2, 3])];
    let mut seen = Vec::new();
    single_feature_importance(&data, &folds, Scoring::Accuracy, |d: &TrainData<'_>| {
        seen.push(d.categorical.to_vec());
        mean_of_labels(d)
    })
    .unwrap();
    assert_eq!(seen, vec![vec![], vec![], vec![0], vec![0]]);
}

/// The real seam plugs in as the module doc says, and a refused fit names its fold.
#[test]
fn a_learner_is_the_fit_closure_and_its_refusal_names_the_fold() {
    let (x, y) = signal_noise_constant();
    let data = TrainData::new(&x, &y, 3).unwrap();
    let folds = purged_folds(data.n_rows);
    let learner = ScriptedLearner::constant(0.7);
    let fit = |d: &TrainData<'_>| learner.fit(d, &DEFAULT_POINT, 0).map(Box::new);
    let (importance, _) =
        mean_decrease_accuracy(&data, &folds, Scoring::NegativeLogLoss, 3, fit).unwrap();
    assert!(importance.mean.iter().all(|&m| m == 0.0), "a constant model reads no feature");

    let refusing = ScriptedLearner::always_err("no binary");
    let fit = |d: &TrainData<'_>| refusing.fit(d, &DEFAULT_POINT, 0).map(Box::new);
    let refusal = single_feature_importance(&data, &folds, Scoring::Accuracy, fit).unwrap_err();
    assert_eq!(refusal, "feature 0, fold 0: no binary");
}

#[test]
fn folds_that_cannot_be_scored_are_refused() {
    let (x, y) = signal_noise_constant();
    let data = TrainData::new(&x, &y, 3).unwrap();
    let fit = |_: &TrainData<'_>| Ok(Box::new(SignOfFirst));
    for (folds, says) in [
        (vec![], "no folds"),
        (vec![(vec![0, 1], vec![])], "no test row"),
        (vec![(vec![0, 40], vec![2])], "names row 40"),
    ] {
        let refusal = mean_decrease_accuracy(&data, &folds, Scoring::Accuracy, 0, fit).unwrap_err();
        assert!(refusal.contains(says), "{refusal}");
    }
}
