//! SPLIT CONFORMAL prediction (Vovk, Gammerman and Shafer, 2005; Lei et al., "Distribution-free
//! predictive inference for regression", 2018): a model plus one THRESHOLD calibrated on rows it
//! was not fitted on, giving a regression an INTERVAL and a classifier a prediction SET that
//! contain the truth with probability at least `1 − α` whenever the calibration rows and the new
//! row are exchangeable.
//!
//! - **The score** ([`nonconformity_score`]): `|y − ŷ|` for a [`PredictionKind::Value`] model;
//!   `1 − p̂_y`, one minus the probability the model gave the TRUE class ([`true_class`]), for a
//!   classifier (a [`PredictionKind::Probability`] model's classes are `0` with `1 − p` and `1` with `p`).
//! - **The threshold** `q̂` is fitted in `vike-ml-training` (`crates/vike-ml-training/src/conformal.rs`):
//!   the `⌈(n + 1)(1 − α)⌉`-th smallest of the `n` calibration scores.
//! - **The answer**: the interval `[ŷ − q̂, ŷ + q̂]` ([`ConformalModel::interval`]); the classes
//!   whose score `1 − p̂_k` is at most `q̂` ([`ConformalModel::prediction_set`]).
//!
//! The guarantee is MARGINAL (over calibration draws and the new row), not per row, and it holds
//! only while the rows are exchangeable: a market that moved since the calibration window voids it.
//!
//! # The text, `vike-ml conformal v1`
//!
//! ```text
//! vike-ml conformal v1
//! miscoverage=<α, in (0, 1)>
//! threshold=<q̂, finite, not negative>
//! model=<byte count>            (followed by the calibrated model's text)
//! end of model
//! ```

use crate::any_model::AnyModel;
use crate::error::MlError;
use crate::predictor::{PredictionKind, Predictor};
use crate::text_format::{Reader, Writer, nested_error};

/// The kind word of this model's text.
pub const KIND: &str = "conformal";

/// A model and the threshold that turns its point prediction into a set.
#[derive(Clone, Debug, PartialEq)]
pub struct ConformalModel {
    model: Box<AnyModel>,
    miscoverage: f64,
    threshold: f64,
}

impl ConformalModel {
    /// `miscoverage` is `α` in `(0, 1)`; `threshold` is the calibrated `q̂`, finite and not
    /// negative. Anything else is refused.
    pub fn new(model: AnyModel, miscoverage: f64, threshold: f64) -> Result<Self, MlError> {
        if miscoverage.is_nan() || miscoverage <= 0.0 || miscoverage >= 1.0 {
            return Err(MlError::Shape(format!("the miscoverage {miscoverage} is not in (0, 1)")));
        }
        if !threshold.is_finite() || threshold < 0.0 {
            return Err(MlError::Shape(format!(
                "the threshold {threshold} is not a finite score at or above zero"
            )));
        }
        Ok(Self { model: Box::new(model), miscoverage, threshold })
    }

    /// The calibrated model.
    pub fn model(&self) -> &AnyModel {
        &self.model
    }

    /// `α`: the set misses the truth with probability at most this.
    pub fn miscoverage(&self) -> f64 {
        self.miscoverage
    }

    /// `q̂`, the calibrated score threshold.
    pub fn threshold(&self) -> f64 {
        self.threshold
    }

    /// `[ŷ − q̂, ŷ + q̂]` for a [`PredictionKind::Value`] model; another kind is refused.
    pub fn interval(&self, row: &[f64]) -> Result<(f64, f64), MlError> {
        if self.model.kind() != PredictionKind::Value {
            return Err(MlError::Unsupported(format!(
                "an interval needs a regression; this model predicts {:?} (ask `prediction_set`)",
                self.model.kind()
            )));
        }
        let point = self.model.predict_one(row).map_err(MlError::Shape)?;
        Ok((point - self.threshold, point + self.threshold))
    }

    /// The classes, ascending, whose score `1 − p̂_k` is at most `q̂`, for a classifier; a
    /// regression is refused. The set may be empty or hold every class: both are honest answers.
    pub fn prediction_set(&self, row: &[f64]) -> Result<Vec<usize>, MlError> {
        let probabilities = class_probabilities(&*self.model, row)?;
        Ok((0..probabilities.len()).filter(|&k| 1.0 - probabilities[k] <= self.threshold).collect())
    }

    /// The `vike-ml conformal v1` text of `model_text` calibrated at `miscoverage` to `threshold`.
    pub fn text_from_parts(model_text: &str, miscoverage: f64, threshold: f64) -> String {
        let mut w = Writer::new(KIND);
        w.number("miscoverage", miscoverage);
        w.number("threshold", threshold);
        w.nested("model", model_text);
        w.finish()
    }

    /// Read a `vike-ml conformal v1` text, the nested model through [`AnyModel::from_text`].
    pub fn from_text(text: &str) -> Result<Self, MlError> {
        let mut r = Reader::open(text, KIND)?;
        let miscoverage = r.number("miscoverage")?;
        let threshold = r.number("threshold")?;
        let (at, model) = r.nested("model")?;
        let model = AnyModel::from_text(model).map_err(|e| nested_error(at, "the model", e))?;
        r.finish()?;
        Self::new(model, miscoverage, threshold)
    }
}

/// The conformal score of `(row, label)` under `model`: `|label − ŷ|` for a regression,
/// `1 − p̂_label` for a classifier, whose `label` must be a class number (`0` or `1` for a
/// [`PredictionKind::Probability`] model). The calibration and the prediction set read this ONE
/// definition.
pub fn nonconformity_score(model: &dyn Predictor, row: &[f64], label: f64) -> Result<f64, MlError> {
    if model.kind() == PredictionKind::Value {
        let point = model.predict_one(row).map_err(MlError::Shape)?;
        return Ok((label - point).abs());
    }
    let (class, probabilities) = true_class(model, row, label)?;
    Ok(1.0 - probabilities[class])
}

/// The TRUE class `label` names and `model`'s probability of each class of `row` (`[1 − p, p]` for a
/// [`PredictionKind::Probability`] model), so `probabilities[class]` is the probability of the true
/// class: the ONE label check of every reader of it. A regression is refused, and so is a `label` that
/// is not a class number of the model (`0` or `1` for a binary one).
pub fn true_class<P: Predictor + ?Sized>(
    model: &P,
    row: &[f64],
    label: f64,
) -> Result<(usize, Vec<f64>), MlError> {
    let probabilities = class_probabilities(model, row)?;
    let class = label as usize;
    if label.fract() != 0.0 || label < 0.0 || class >= probabilities.len() {
        return Err(MlError::Shape(format!(
            "the label {label} is not a class of a {}-class model",
            probabilities.len()
        )));
    }
    Ok((class, probabilities))
}

/// One probability per class: `[1 − p, p]` for a binary model, the K probabilities of a multiclass
/// one; a regression is refused.
fn class_probabilities<P: Predictor + ?Sized>(model: &P, row: &[f64]) -> Result<Vec<f64>, MlError> {
    match model.kind() {
        PredictionKind::Value => Err(MlError::Unsupported(
            "a prediction set needs a classifier; this model predicts a value (ask `interval`)"
                .into(),
        )),
        PredictionKind::Probability => {
            let p = model.predict_one(row).map_err(MlError::Shape)?;
            Ok(vec![1.0 - p, p])
        }
        PredictionKind::ClassProbabilities { class_count } => {
            let mut out = vec![0.0; class_count];
            model.predict_into(row, &mut out).map_err(MlError::Shape)?;
            Ok(out)
        }
    }
}

impl Predictor for ConformalModel {
    /// The calibrated model's own kind: the point prediction is unchanged by the calibration.
    fn kind(&self) -> PredictionKind {
        self.model.kind()
    }

    fn predict_into(&self, row: &[f64], out: &mut [f64]) -> Result<(), String> {
        self.model.predict_into(row, out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::linear::{LinearLink, LinearModel};

    fn regression() -> AnyModel {
        LinearModel { link: LinearLink::Identity, intercept: 1.0, coefficients: vec![2.0] }.into()
    }

    fn classifier() -> AnyModel {
        LinearModel { link: LinearLink::Logistic, intercept: 0.0, coefficients: vec![1.0] }.into()
    }

    #[test]
    fn the_interval_is_the_point_plus_and_minus_the_threshold() {
        let c = ConformalModel::new(regression(), 0.1, 0.5).unwrap();
        assert_eq!(c.interval(&[2.0]).unwrap(), (4.5, 5.5));
        assert_eq!(c.predict_one(&[2.0]), Ok(5.0), "the point prediction is the model's");
        assert!(c.prediction_set(&[2.0]).is_err(), "a regression has no prediction set");
        assert_eq!(nonconformity_score(c.model(), &[2.0], 3.5).unwrap(), 1.5);
    }

    #[test]
    fn the_prediction_set_keeps_the_classes_whose_score_clears_the_threshold() {
        // η = 0: p = 1/2, both scores 1/2.
        let set = |threshold| ConformalModel::new(classifier(), 0.1, threshold).unwrap();
        assert_eq!(set(0.5).prediction_set(&[0.0]).unwrap(), vec![0, 1]);
        assert_eq!(set(0.49).prediction_set(&[0.0]).unwrap(), Vec::<usize>::new());
        // η = 3: p ≈ 0.95, so class 1 scores ≈ 0.05 and class 0 ≈ 0.95.
        assert_eq!(set(0.5).prediction_set(&[3.0]).unwrap(), vec![1]);
        assert!(set(0.5).interval(&[3.0]).is_err(), "a classifier has no interval");
        let p = classifier().predict_one(&[3.0]).unwrap();
        assert_eq!(nonconformity_score(&classifier(), &[3.0], 1.0).unwrap(), 1.0 - p);
        assert_eq!(nonconformity_score(&classifier(), &[3.0], 0.0).unwrap(), 1.0 - (1.0 - p));
        assert!(nonconformity_score(&classifier(), &[3.0], 2.0).is_err(), "class 2 of two");
        assert!(nonconformity_score(&classifier(), &[3.0], 0.5).is_err(), "not a class number");
    }

    #[test]
    fn a_miscoverage_or_threshold_out_of_range_is_refused() {
        for (alpha, q) in
            [(0.0, 1.0), (1.0, 1.0), (f64::NAN, 1.0), (0.1, -1.0), (0.1, f64::INFINITY)]
        {
            assert!(ConformalModel::new(regression(), alpha, q).is_err(), "{alpha} {q}");
        }
    }

    #[test]
    fn the_text_reads_back() {
        let inner =
            LinearModel { link: LinearLink::Identity, intercept: 1.0, coefficients: vec![2.0] };
        let text = ConformalModel::text_from_parts(&inner.to_text(), 0.1, 0.25);
        let c = ConformalModel::from_text(&text).unwrap();
        assert_eq!(c, ConformalModel::new(inner.into(), 0.1, 0.25).unwrap());
        assert_eq!(AnyModel::from_text(&text).unwrap(), AnyModel::Conformal(c));
        assert!(
            ConformalModel::from_text(&text.replace("miscoverage=1e-1", "miscoverage=2e0"))
                .is_err()
        );
    }
}
