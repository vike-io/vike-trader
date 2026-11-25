//! DOUBLE / DEBIASED MACHINE LEARNING (Chernozhukov, Chetverikov, Demirer, Duflo, Hansen, Newey and
//! Robins, "Double/debiased machine learning for treatment and structural parameters", *The
//! Econometrics Journal* 21(1), 2018): the effect of a treatment `D` on an outcome `Y` once the
//! confounders `X` are partialled out by machine-learned NUISANCE models, with a standard error and
//! a confidence interval that stay valid although those models are regularised and biased.
//!
//! Two models, each with the paper's Neyman-orthogonal score and its DML2 estimator (the score's
//! mean solved over ALL out-of-fold rows at once, not per fold and then averaged):
//!
//! - [`partially_linear`]: the partially linear regression (PLR), `Y = θ·D + g(X) + ε`,
//!   `D = m(X) + v`. Two nuisances, `ℓ(X) = E[Y | X]` and `m(X) = E[D | X]`; the partialling-out
//!   score `ψ = (Ỹ − θ·D̃)·D̃` on the residuals `Ỹ = Y − ℓ̂(X)` and `D̃ = D − m̂(X)` gives
//!   `θ̂ = Σ D̃·Ỹ / Σ D̃²` (the paper's §4.1). `D` may be continuous or binary.
//! - [`interactive`]: the interactive regression model (IRM), `Y = g(D, X) + U` with `D ∈ {0, 1}`
//!   and `P(D = 1 | X) = m(X)`: the AVERAGE treatment effect `θ = E[g(1, X) − g(0, X)]` through the
//!   doubly robust score `g₁ − g₀ + D·(Y − g₁)/m − (1 − D)·(Y − g₀)/(1 − m) − θ` (§5.1). Three
//!   nuisances: `g₀` and `g₁` (the outcome learner fitted on a fold's control and treated training
//!   rows apart) and the propensity `m`, clipped to `[trim, 1 − trim]`.
//!
//! # Cross-fitting on the caller's folds
//!
//! Every nuisance is fitted on a fold's TRAINING rows and predicts its TEST rows, so no row's
//! residual comes from a model that saw it. The folds are the CALLER's and this module draws none:
//! a time series wants purged ones (`vike-analytics`' `validation::purged_kfold_indices`, or its
//! label-span purge `purged_kfold_label_spans`), so a nuisance never trains on a label that overlaps
//! the rows it is judged on. Each row must be in exactly one test set and no fold may train on a
//! row it tests; both are refused, because either one quietly brings back the overfitting bias
//! cross-fitting exists to remove.
//!
//! # Any learner is a nuisance
//!
//! The nuisances go through the learner seam ([`Learner`]): `crate::linear::LinearLearner`,
//! `crate::gaussian_process::GaussianProcessLearner`, `crate::GbdtLearner` with a `regression`
//! objective (or `binary` for a propensity), an ensemble, or a `dyn Learner<Model = AnyModel>`: any
//! ONE-output model (`vike_ml_prediction::Predictor::predict_one` refuses a K-class one). Fold `k`
//! is fitted with seed `seed + k` (the ensembles' member rule); the grid point goes to every fit
//! unchanged. The learners see the labels as `f32` ([`TrainData`]'s width, LightGBM's own); the
//! residuals and the scores are formed from the caller's `f64` values.
//!
//! # The standard error and the interval
//!
//! From the score's own variance (the paper's Theorem 3.1): `σ̂² = mean(ψ²) / J²`, with `J` the
//! mean derivative of the score in θ (`mean(D̃²)` for the PLR, `1` for the IRM), and
//! `se = √(σ̂² / n)`. [`DoubleMlEstimate::confidence_interval`] is the two-sided normal interval
//! `θ̂ ± Φ⁻¹((1 + level)/2)·se` (`vike_math::distributions::normal_quantile`). One split; the paper's
//! median over repeated splits is the caller's loop over its own folds.

use vike_math::distributions::normal_quantile;
use vike_math::statistics::mean_variance;
use vike_ml_prediction::Predictor;

use crate::ensemble::{check_out_of_fold, member_seed, subset, view};
use crate::{GridPoint, Learner, TrainData};

/// The rows of one effect estimate. `confounders` is row-major like [`TrainData::x`]: `column_count`
/// values per row; `treatment` and `outcome` hold one value per row.
#[derive(Clone, Copy, Debug)]
pub struct DoubleMlData<'a> {
    pub confounders: &'a [f64],
    pub column_count: usize,
    pub treatment: &'a [f64],
    pub outcome: &'a [f64],
}

/// How the nuisance models are cross-fitted: the module doc's "Cross-fitting on the caller's folds".
#[derive(Clone, Copy, Debug)]
pub struct CrossFitting<'a> {
    /// `(training rows, test rows)` per fold: every row in exactly one test set, and no fold
    /// training on a row it tests.
    pub folds: &'a [(Vec<usize>, Vec<usize>)],
    /// Handed to every nuisance fit unchanged; of this crate's learners only LightGBM reads it.
    pub point: &'a GridPoint,
    /// Fold `k` is fitted with `seed + k` (wrapping).
    pub seed: u64,
}

/// One estimated effect, its standard error and the rows it was estimated on.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct DoubleMlEstimate {
    /// θ̂: the PLR's coefficient on the treatment, or the IRM's average treatment effect.
    pub effect: f64,
    pub standard_error: f64,
    pub rows: usize,
}

impl DoubleMlEstimate {
    /// The two-sided normal interval `effect ± Φ⁻¹((1 + level)/2)·standard_error`, `(low, high)`:
    /// `level = 0.95` is the usual 95% interval. A level outside `(0, 1)` is refused.
    pub fn confidence_interval(&self, level: f64) -> Result<(f64, f64), String> {
        if !(level > 0.0 && level < 1.0) {
            return Err(format!("the confidence level {level} is not in (0, 1)"));
        }
        let half_width = normal_quantile((1.0 + level) / 2.0) * self.standard_error;
        Ok((self.effect - half_width, self.effect + half_width))
    }
}

/// The partially linear regression's θ (module doc): `outcome_learner` fits `E[Y | X]`,
/// `treatment_learner` fits `E[D | X]`, both cross-fitted on `cross`'s folds.
///
/// Refused besides the shape and fold checks: residual treatment that is exactly zero on every row
/// (the confounders explain the treatment, so its effect is not identified).
pub fn partially_linear<O, T>(
    data: &DoubleMlData<'_>,
    outcome_learner: &O,
    treatment_learner: &T,
    cross: &CrossFitting<'_>,
) -> Result<DoubleMlEstimate, String>
where
    O: Learner + ?Sized,
    T: Learner + ?Sized,
{
    let rows = check(data, cross)?;
    let every_row = |_: usize| true;
    let outcome_fit =
        out_of_fold(outcome_learner, data, data.outcome, cross, every_row, "the outcome nuisance")?;
    let treatment_fit = out_of_fold(
        treatment_learner,
        data,
        data.treatment,
        cross,
        every_row,
        "the treatment nuisance",
    )?;
    let outcome_residual: Vec<f64> =
        data.outcome.iter().zip(&outcome_fit).map(|(y, fit)| y - fit).collect();
    let treatment_residual: Vec<f64> =
        data.treatment.iter().zip(&treatment_fit).map(|(d, fit)| d - fit).collect();

    let (mut cross_sum, mut square_sum) = (0.0, 0.0);
    for (d, y) in treatment_residual.iter().zip(&outcome_residual) {
        cross_sum += d * y;
        square_sum += d * d;
    }
    if square_sum == 0.0 {
        return Err("the treatment's out-of-fold residual is zero on every row: the confounders \
                    explain the treatment exactly, so its effect is not identified"
            .into());
    }
    let effect = cross_sum / square_sum;
    let n = rows as f64;
    let mut score_square_sum = 0.0;
    for (d, y) in treatment_residual.iter().zip(&outcome_residual) {
        let score = (y - effect * d) * d;
        score_square_sum += score * score;
    }
    let derivative = square_sum / n;
    let variance = score_square_sum / n / (derivative * derivative);
    finish(effect, (variance / n).sqrt(), rows)
}

/// The interactive regression model's average treatment effect (module doc): `outcome_learner`
/// fits `E[Y | X]` on a fold's control rows (`g₀`) and on its treated rows (`g₁`),
/// `propensity_learner` fits `P(D = 1 | X)`, clipped to `[propensity_trim, 1 − propensity_trim]`
/// (`0.01` is the usual choice; `0` clips nothing, and a propensity of 0 or 1 is then refused as a
/// non-finite estimate).
///
/// Refused besides the shape and fold checks: a treatment other than `0` or `1`, a trim outside
/// `[0, 0.5)`, a fold with no control or no treated training row.
pub fn interactive<O, P>(
    data: &DoubleMlData<'_>,
    outcome_learner: &O,
    propensity_learner: &P,
    cross: &CrossFitting<'_>,
    propensity_trim: f64,
) -> Result<DoubleMlEstimate, String>
where
    O: Learner + ?Sized,
    P: Learner + ?Sized,
{
    let rows = check(data, cross)?;
    if !(0.0..0.5).contains(&propensity_trim) {
        return Err(format!("the propensity trim {propensity_trim} is not in [0, 0.5)"));
    }
    if let Some((i, d)) = data.treatment.iter().enumerate().find(|(_, d)| **d != 0.0 && **d != 1.0)
    {
        return Err(format!(
            "row {i}'s treatment {d} is not 0 or 1; the interactive model needs a binary treatment \
             (the partially linear one takes any)"
        ));
    }
    let treated = |i: usize| data.treatment[i] == 1.0;
    let control_fit = out_of_fold(
        outcome_learner,
        data,
        data.outcome,
        cross,
        |i| !treated(i),
        "the control outcome nuisance",
    )?;
    let treated_fit = out_of_fold(
        outcome_learner,
        data,
        data.outcome,
        cross,
        treated,
        "the treated outcome nuisance",
    )?;
    let propensity = out_of_fold(
        propensity_learner,
        data,
        data.treatment,
        cross,
        |_| true,
        "the propensity nuisance",
    )?;

    let mut scores = Vec::with_capacity(rows);
    for i in 0..rows {
        let (y, d) = (data.outcome[i], data.treatment[i]);
        let (g0, g1) = (control_fit[i], treated_fit[i]);
        let m = propensity[i].clamp(propensity_trim, 1.0 - propensity_trim);
        scores.push(g1 - g0 + d * (y - g1) / m - (1.0 - d) * (y - g0) / (1.0 - m));
    }
    let (effect, variance) = mean_variance(scores.iter().copied(), 0.0);
    finish(effect, (variance / rows as f64).sqrt(), rows)
}

/// The shape of `data` and the folds of `cross`; answers the row count.
fn check(data: &DoubleMlData<'_>, cross: &CrossFitting<'_>) -> Result<usize, String> {
    let rows = data.outcome.len();
    if data.column_count == 0 {
        return Err("double machine learning needs at least one confounder column".into());
    }
    if data.treatment.len() != rows {
        return Err(format!("{rows} outcomes but {} treatments", data.treatment.len()));
    }
    if Some(data.confounders.len()) != rows.checked_mul(data.column_count) {
        return Err(format!(
            "{} confounder values for {rows} rows of {} columns",
            data.confounders.len(),
            data.column_count
        ));
    }
    if rows < 2 {
        return Err(format!("{rows} rows: a standard error needs at least two"));
    }
    // The confounders are the learners' to judge (LightGBM reads a NaN as missing); the two
    // columns this module does arithmetic on are not.
    if data.outcome.iter().chain(data.treatment).any(|v| !v.is_finite()) {
        return Err("the outcome and the treatment must be finite on every row".into());
    }
    check_out_of_fold(cross.folds, rows, "cross-fitting")?;
    for (k, (train, test)) in cross.folds.iter().enumerate() {
        let mut tested = vec![false; rows];
        for &i in test {
            tested[i] = true;
        }
        for &i in train {
            if i >= rows {
                return Err(format!("fold {k} trains on row {i} of {rows} rows"));
            }
            if tested[i] {
                return Err(format!(
                    "fold {k} trains on row {i}, which it also tests: its residual would come \
                     from a model that saw it"
                ));
            }
        }
    }
    Ok(rows)
}

/// Out-of-fold predictions of `learner` fitted to `labels`: per fold, on the training rows `keep`
/// accepts, predicting every one of the fold's test rows. `nuisance` names it in a refusal.
fn out_of_fold<L: Learner + ?Sized>(
    learner: &L,
    data: &DoubleMlData<'_>,
    labels: &[f64],
    cross: &CrossFitting<'_>,
    keep: impl Fn(usize) -> bool,
    nuisance: &str,
) -> Result<Vec<f64>, String> {
    let labels: Vec<f32> = labels.iter().map(|&v| v as f32).collect();
    let all = TrainData {
        x: data.confounders,
        y: &labels,
        n_rows: labels.len(),
        n_cols: data.column_count,
        categorical: &[],
    };
    let mut predictions = vec![f64::NAN; labels.len()];
    for (k, (train, test)) in cross.folds.iter().enumerate() {
        let train: Vec<usize> = train.iter().copied().filter(|&i| keep(i)).collect();
        if train.is_empty() {
            return Err(format!("{nuisance}: fold {k} has no training row to fit on"));
        }
        let (x, y) = subset(&all, &train, &format!("fold {k}"))?;
        let model = learner
            .fit(&view(&all, &x, &y), cross.point, member_seed(cross.seed, k))
            .map_err(|e| format!("{nuisance}, fold {k}: {e}"))?;
        for &i in test {
            let prediction = model
                .predict_one(all.row(i))
                .map_err(|e| format!("{nuisance}, fold {k}, row {i}: {e}"))?;
            if !prediction.is_finite() {
                return Err(format!("{nuisance}, fold {k}: row {i} predicted {prediction}"));
            }
            predictions[i] = prediction;
        }
    }
    Ok(predictions)
}

fn finish(effect: f64, standard_error: f64, rows: usize) -> Result<DoubleMlEstimate, String> {
    if !effect.is_finite() || !standard_error.is_finite() {
        return Err(format!(
            "the estimate is not finite (effect {effect}, standard error {standard_error}): a \
             propensity of exactly 0 or 1 with no trim, or residuals too large for f64"
        ));
    }
    Ok(DoubleMlEstimate { effect, standard_error, rows })
}

#[path = "double_ml_tests.rs"]
#[cfg(test)]
mod double_ml_tests;
