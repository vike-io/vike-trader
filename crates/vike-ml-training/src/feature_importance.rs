//! Feature importance from what a feature does OUT OF SAMPLE: López de Prado, *Advances in
//! Financial Machine Learning*, ch. 8 — mean decrease accuracy (MDA, snippet 8.3) and single
//! feature importance (SFI, snippet 8.4).
//!
//! * [`mean_decrease_accuracy`]: fit on each fold's training rows, score the test rows, then score
//!   them again with ONE feature column permuted; a feature matters as much as the permutation
//!   hurts. Substitution effects are its blind spot: two features carrying the same signal each
//!   look unimportant, because the other still carries it.
//! * [`single_feature_importance`]: fit and score on each feature ALONE. No substitution effect,
//!   but no joint effect either.
//!
//! # The third method is not here, on purpose
//!
//! Mean decrease impurity (MDI, snippet 8.2) is an IN-SAMPLE fact about the fitted trees, and for a
//! gradient-boosted model it is the per-feature split gain `vike_ml_prediction::FitImportance`
//! already reports ([`crate::Learner::fit_captured`] with [`crate::Capture`]'s `importance`). One
//! copy of it exists, and this module is not a second.
//!
//! # The folds are the caller's
//!
//! Both functions score on the caller's `(train, test)` row-index folds and never split by
//! themselves. A label built over a window (a triple-barrier or trend-scanning label) leaks the test
//! outcome unless the folds are PURGED by label span, which is
//! `vike_analytics::validation::purged_kfold_label_spans` (AFML snippet 7.3, the `PurgedKFold`
//! AFML's own snippet 8.3 takes): its return value is this parameter's exact type. Taking the folds
//! as a parameter keeps this crate's one `vike-*` edge (`crates/vike-ml-training/CLAUDE.md`'s
//! "Crate rules") and lets a caller bring any other splitter.
//!
//! # The fit is the caller's too
//!
//! `fit` turns one training subset into a boxed [`Predictor`]: a [`crate::Learner`] at one grid
//! point and seed is `|d| learner.fit(d, &point, seed).map(Box::new)`, and a study's host-side
//! learner already answers a `Box<dyn Predictor>`. A refused fit is returned as the error, naming
//! its fold (and its feature, for SFI).
//!
//! # Scoring
//!
//! AFML's two scores, on a CLASSIFIER: [`Scoring::NegativeLogLoss`] (the mean log probability of
//! the true class, so minus the workspace's one log loss [`log_loss_of`], clipped at
//! [`LOG_LOSS_CLIP`]) and [`Scoring::Accuracy`] (the predicted class is
//! the most probable one; a binary model predicts class 1 only when `p > 0.5`). A
//! [`PredictionKind::Probability`] model takes labels `0`/`1`, a
//! [`PredictionKind::ClassProbabilities`] model labels `0..K`, and a [`PredictionKind::Value`]
//! model (a regression) is REFUSED: both methods and their normalisations are defined for a
//! classifier. A NaN prediction makes its fold's score NaN rather than a plausible number.
//!
//! MDA's per-fold importance is AFML's normalised decrease: `(score - permuted) / -permuted` for
//! the log loss, `(score - permuted) / (1 - permuted)` for accuracy. That is `0 / 0` (NaN) when the
//! permuted model still scores perfectly, as in AFML.
//!
//! # Not taken: sample weights
//!
//! AFML weighs both the fit and the score by sample uniqueness. The learner seam fits without
//! weights, and weighing the score alone would be half the method, so neither is weighed.
//!
//! # Determinism
//!
//! MDA's permutations come from ONE `SplitMix64` stream seeded by `seed`, drawn fold by fold and
//! column by column, so the same seed, data, folds and fit give the same numbers on every box (the
//! log loss's `log` is [`log_loss_of`]'s `libm::log`, decision 0032).

use vike_math::random::{SplitMix64, shuffle};
use vike_ml_prediction::conformal::true_class;
use vike_ml_prediction::{PredictionKind, Predictor};

use crate::calibration::{LOG_LOSS_CLIP, log_loss_of};
use crate::ensemble::{subset, view};
use crate::train::TrainData;

/// How a fold's test rows are scored. Higher is better for both.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Scoring {
    /// The mean natural log of the probability given to the true class: minus the log loss.
    NegativeLogLoss,
    /// The fraction of rows whose most probable class is the true one.
    Accuracy,
}

/// One importance per feature over the folds.
#[derive(Clone, Debug, PartialEq)]
pub struct ImportanceEstimate {
    /// Per feature column: the mean over folds.
    pub mean: Vec<f64>,
    /// Per feature column: the folds' standard deviation (`n - 1` denominator) divided by
    /// `sqrt(n)` — AFML's `std() * n ** -0.5`. NaN with one fold.
    pub standard_error: Vec<f64>,
    /// `per_fold[fold][column]`: the values the two summarise.
    pub per_fold: Vec<Vec<f64>>,
}

/// MDA (AFML snippet 8.3): per fold, fit on the training rows, score the test rows, then score
/// them again with each feature column permuted in turn (the other columns untouched).
///
/// Returns the importance per feature and the mean unpermuted out-of-fold score (AFML's
/// `scr0.mean()`).
pub fn mean_decrease_accuracy<P, F>(
    data: &TrainData<'_>,
    folds: &[(Vec<usize>, Vec<usize>)],
    scoring: Scoring,
    seed: u64,
    mut fit: F,
) -> Result<(ImportanceEstimate, f64), String>
where
    P: Predictor + ?Sized,
    F: FnMut(&TrainData<'_>) -> Result<Box<P>, String>,
{
    check_folds(data, folds)?;
    let n_cols = data.n_cols;
    let mut rng = SplitMix64::new(seed);
    let mut baseline_total = 0.0;
    let mut per_fold = Vec::with_capacity(folds.len());
    for (fold, (train, test)) in folds.iter().enumerate() {
        let at = |e: String| format!("fold {fold}: {e}");
        let (train_x, train_y) = subset(data, train, &format!("fold {fold}"))?;
        let model = fit(&view(data, &train_x, &train_y)).map_err(at)?;
        let (mut x, y) = subset(data, test, &format!("fold {fold}"))?;
        let score = score_of(&*model, &x, n_cols, &y, scoring).map_err(at)?;
        let mut importance = Vec::with_capacity(n_cols);
        let mut column = vec![0.0; test.len()];
        for feature in 0..n_cols {
            for (value, row) in column.iter_mut().zip(x.chunks_exact(n_cols)) {
                *value = row[feature];
            }
            let mut permuted = column.clone();
            shuffle(&mut permuted, &mut rng);
            for (value, row) in permuted.iter().zip(x.chunks_exact_mut(n_cols)) {
                row[feature] = *value;
            }
            let permuted_score = score_of(&*model, &x, n_cols, &y, scoring).map_err(at)?;
            importance.push(match scoring {
                Scoring::NegativeLogLoss => (score - permuted_score) / -permuted_score,
                Scoring::Accuracy => (score - permuted_score) / (1.0 - permuted_score),
            });
            for (value, row) in column.iter().zip(x.chunks_exact_mut(n_cols)) {
                row[feature] = *value;
            }
        }
        baseline_total += score;
        per_fold.push(importance);
    }
    Ok((estimate(per_fold, n_cols), baseline_total / folds.len() as f64))
}

/// SFI (AFML snippet 8.4): per feature column, per fold, fit on that ONE column of the training
/// rows and score it on the test rows. The importance is the out-of-fold score itself.
///
/// A categorical column stays categorical in its one-column fit.
pub fn single_feature_importance<P, F>(
    data: &TrainData<'_>,
    folds: &[(Vec<usize>, Vec<usize>)],
    scoring: Scoring,
    mut fit: F,
) -> Result<ImportanceEstimate, String>
where
    P: Predictor + ?Sized,
    F: FnMut(&TrainData<'_>) -> Result<Box<P>, String>,
{
    check_folds(data, folds)?;
    let mut per_fold = vec![Vec::with_capacity(data.n_cols); folds.len()];
    for feature in 0..data.n_cols {
        let alone: Vec<f64> = (0..data.n_rows).map(|row| data.row(row)[feature]).collect();
        let categorical: &[usize] = if data.categorical.contains(&feature) { &[0] } else { &[] };
        let one_column = TrainData::new(&alone, data.y, 1)
            .and_then(|d| d.with_categorical(categorical))
            .map_err(|e| e.to_string())?;
        for (fold, (train, test)) in folds.iter().enumerate() {
            let at = |e: String| format!("feature {feature}, fold {fold}: {e}");
            let (train_x, train_y) = subset(&one_column, train, &format!("fold {fold}"))?;
            let model = fit(&view(&one_column, &train_x, &train_y)).map_err(at)?;
            let (x, y) = subset(&one_column, test, &format!("fold {fold}"))?;
            per_fold[fold].push(score_of(&*model, &x, 1, &y, scoring).map_err(at)?);
        }
    }
    Ok(estimate(per_fold, data.n_cols))
}

/// Refuse what would otherwise be a panic or a silent nothing: no fold, a fold with no test row
/// (no score), an index past the data.
fn check_folds(data: &TrainData<'_>, folds: &[(Vec<usize>, Vec<usize>)]) -> Result<(), String> {
    data.validate().map_err(|e| e.to_string())?;
    if folds.is_empty() {
        return Err("no folds: an out-of-fold importance needs at least one test fold".into());
    }
    for (fold, (train, test)) in folds.iter().enumerate() {
        if test.is_empty() {
            return Err(format!("fold {fold} has no test row, so it has no score"));
        }
        if let Some(&row) = train.iter().chain(test).find(|&&row| row >= data.n_rows) {
            return Err(format!("fold {fold} names row {row}, but the data has {}", data.n_rows));
        }
    }
    Ok(())
}

/// The mean of one [`Scoring`] term over the rows of `x` (row-major, `n_cols` wide) against `y`.
fn score_of<P: Predictor + ?Sized>(
    model: &P,
    x: &[f64],
    n_cols: usize,
    y: &[f32],
    scoring: Scoring,
) -> Result<f64, String> {
    if model.kind() == PredictionKind::Value {
        return Err("a regression model has no class to score: MDA and SFI score a classifier by \
                    log loss or accuracy (AFML ch. 8)"
            .into());
    }
    let mut total = 0.0;
    for (row, &label) in x.chunks_exact(n_cols).zip(y) {
        let (class, probabilities) =
            true_class(model, row, f64::from(label)).map_err(|e| e.to_string())?;
        if probabilities.iter().any(|p| p.is_nan()) {
            return Ok(f64::NAN);
        }
        total += match scoring {
            Scoring::NegativeLogLoss => -log_loss_of(probabilities[class], LOG_LOSS_CLIP),
            // A binary model's `[1 − p, p]` has its first largest at class 1 exactly when `p > 0.5`.
            Scoring::Accuracy => f64::from(u8::from(most_probable(&probabilities) == class)),
        };
    }
    Ok(total / y.len() as f64)
}

/// The first index of the largest value, as `argmax` breaks ties.
fn most_probable(probabilities: &[f64]) -> usize {
    let mut best = 0;
    for (class, &p) in probabilities.iter().enumerate().skip(1) {
        if p > probabilities[best] {
            best = class;
        }
    }
    best
}

/// Mean and standard error per column over the folds.
fn estimate(per_fold: Vec<Vec<f64>>, n_cols: usize) -> ImportanceEstimate {
    let folds = per_fold.len() as f64;
    let (mean, standard_error): (Vec<f64>, Vec<f64>) = (0..n_cols)
        .map(|j| {
            let column = per_fold.iter().map(|values| values[j]);
            let (mean, variance) = vike_math::statistics::mean_variance(column, 1.0);
            (mean, variance.sqrt() / folds.sqrt())
        })
        .unzip();
    ImportanceEstimate { mean, standard_error, per_fold }
}

#[path = "feature_importance_tests.rs"]
#[cfg(test)]
mod feature_importance_tests;
