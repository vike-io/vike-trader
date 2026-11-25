//! The PREDICTION trait: a trained model, read one feature row at a time — one probability, K class
//! probabilities, or one value, declared by the model itself.
//!
//! It lives here, beside the models that implement it, rather than beside the learner that fits
//! them (`vike-ml-training`'s `seam::learner`): a strategy that only predicts links this crate and
//! nothing that spawns a process or writes a file.
//!
//! # One trait for every output shape
//!
//! [`Predictor::kind`] says what one prediction MEANS and so how many numbers it is
//! ([`PredictionKind::output_count`]); [`Predictor::predict_into`] writes exactly that many. A
//! caller that wants the single number of a one-output model asks [`Predictor::predict_one`], which
//! REFUSES a K-output model rather than handing back one of its K numbers. A caller scoring many
//! rows asks [`Predictor::predict_batch`]: the same numbers in row order, and a model answering
//! across a process boundary crosses it once for all of them instead of once per row.
//!
//! Errors are a `String`, as `vike-ml-training`'s `Learner::fit` is: an implementation that answers
//! across a process boundary has failures no [`crate::MlError`] variant names, and the whole value
//! of the refusal is its sentence.

use crate::model::{GbdtModel, Objective};

/// What the numbers of one prediction mean.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PredictionKind {
    /// One number: the probability of the positive class (a binary classifier).
    Probability,
    /// `class_count` numbers: one probability per class, summing to one (a multiclass softmax).
    ClassProbabilities { class_count: usize },
    /// One number that is not a probability (a regression).
    Value,
}

impl PredictionKind {
    /// How many numbers one prediction is.
    pub fn output_count(self) -> usize {
        match self {
            PredictionKind::ClassProbabilities { class_count } => class_count,
            PredictionKind::Probability | PredictionKind::Value => 1,
        }
    }
}

/// A trained model, asked one feature row at a time, or a matrix of them ([`Predictor::predict_batch`]).
///
/// `Send + Sync` because a caller scoring a grid predicts from several threads at once. ⚠ If an
/// implementation's model type cannot be `Sync`, this bound is the thing that has to give — and
/// the caller then scores its grid SERIALLY.
pub trait Predictor: Send + Sync {
    /// What one prediction means. Constant for the life of the model.
    fn kind(&self) -> PredictionKind;

    /// One prediction for `row`, written into `out`, whose length must be
    /// `self.kind().output_count()`; an implementation refuses any other length.
    fn predict_into(&self, row: &[f64], out: &mut [f64]) -> Result<(), String>;

    /// The one number of a one-output model: the probability of a [`PredictionKind::Probability`]
    /// model, the value of a [`PredictionKind::Value`] one. A
    /// [`PredictionKind::ClassProbabilities`] model is REFUSED — any one of its K numbers would be
    /// a plausible answer to the wrong question.
    fn predict_one(&self, row: &[f64]) -> Result<f64, String> {
        let kind = self.kind();
        if kind.output_count() != 1 {
            return Err(format!(
                "this model predicts {} numbers per row ({kind:?}); ask `predict_into`",
                kind.output_count()
            ));
        }
        let mut out = [f64::NAN];
        self.predict_into(row, &mut out)?;
        Ok(out[0])
    }

    /// [`Predictor::predict_into`] over a row-major matrix of `n_features`-wide rows: each row's
    /// `self.kind().output_count()` numbers, row-major, in row order. The default asks
    /// `predict_into` row by row and answers the first row's refusal; a model held across a process
    /// boundary overrides it to cross ONCE for the matrix, answering the same numbers and the same
    /// refusal. A length that is not whole rows (or rows of no width) is refused before any row.
    fn predict_batch(&self, flat_x: &[f64], n_features: usize) -> Result<Vec<f64>, String> {
        if n_features == 0 || !flat_x.len().is_multiple_of(n_features) {
            return Err(format!("{} numbers are not whole rows of {n_features}", flat_x.len()));
        }
        let mut answer = vec![0.0; self.kind().output_count()];
        let mut out = Vec::with_capacity(flat_x.len() / n_features * answer.len());
        for row in flat_x.chunks_exact(n_features) {
            self.predict_into(row, &mut answer)?;
            out.extend_from_slice(&answer);
        }
        Ok(out)
    }
}

/// [`GbdtModel`]'s objective decides its kind, and its inherent `predict_into` already is this
/// trait's method; the error crosses as its sentence.
///
/// Spelled with the fully-qualified call so the delegation names the inherent method it forwards
/// to: an inherent method shadows a trait one of the same name.
impl Predictor for GbdtModel {
    fn kind(&self) -> PredictionKind {
        match self.objective {
            Objective::Binary { .. } => PredictionKind::Probability,
            Objective::Regression => PredictionKind::Value,
            Objective::Multiclass { class_count } => {
                PredictionKind::ClassProbabilities { class_count }
            }
        }
    }

    fn predict_into(&self, row: &[f64], out: &mut [f64]) -> Result<(), String> {
        GbdtModel::predict_into(self, row, out).map_err(|e| e.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::Tree;

    /// One constant tree per value, under `objective`. An empty `split_feature` is LightGBM's own
    /// CONSTANT tree (`GBDT::TrainOneIter` emits one through `AsConstantTree` when it could not
    /// split at all) — rare and legal.
    fn constant_model(objective: Objective, leaves: &[f64]) -> GbdtModel {
        GbdtModel {
            max_feature_index: 0,
            objective,
            feature_names: vec!["f0".to_string()],
            trees: leaves
                .iter()
                .map(|&leaf| Tree {
                    num_leaves: 1,
                    split_feature: Vec::new(),
                    threshold: Vec::new(),
                    decision_type: Vec::new(),
                    left_child: Vec::new(),
                    right_child: Vec::new(),
                    leaf_value: vec![leaf],
                    cat_boundaries: Vec::new(),
                    cat_threshold: Vec::new(),
                    leaf_count: Vec::new(),
                    internal_count: Vec::new(),
                })
                .collect(),
        }
    }

    fn binary(leaf: f64) -> GbdtModel {
        constant_model(Objective::Binary { sigmoid: 1.0 }, &[leaf])
    }

    /// The impl is a delegation, and this is the assertion that says so.
    #[test]
    fn the_trait_answer_on_a_binary_model_is_the_inherent_probability() {
        for leaf in [0.0, 1.5, -2.0, f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
            let m = binary(leaf);
            assert_eq!(Predictor::kind(&m), PredictionKind::Probability);
            let (via_trait, inherent) =
                (Predictor::predict_one(&m, &[0.0]).unwrap(), m.predict_proba(&[0.0]));
            assert_eq!(via_trait.to_bits(), inherent.to_bits(), "leaf {leaf}");
        }
    }

    /// ⚠ The impl does NOT sanitise, deliberately: a NaN raw score reaches the caller as a NaN
    /// probability, so a ranking caller can score the degenerate fit worst. Clamping to a
    /// plausible `0.5` would let such a fit rank like a mediocre model — and WIN, silently.
    #[test]
    fn a_non_finite_leaf_reaches_the_caller_unsanitised() {
        let one = |leaf: f64| Predictor::predict_one(&binary(leaf), &[0.0]).unwrap();
        assert_eq!(
            one(0.0),
            0.5,
            "the control case — without it every assertion below could be a broken harness"
        );
        assert!(one(f64::NAN).is_nan());
        // ...and the two deliberately NOT NaN: `1/(1+exp(-x))` SATURATES, so an infinite raw
        // score is a usable probability of exactly 1 or 0.
        assert_eq!(one(f64::INFINITY), 1.0);
        assert_eq!(one(f64::NEG_INFINITY), 0.0);
    }

    #[test]
    fn a_regression_model_answers_its_value_as_its_one_number() {
        let m = constant_model(Objective::Regression, &[0.25, 1.0]);
        assert_eq!(Predictor::kind(&m), PredictionKind::Value);
        assert_eq!(Predictor::predict_one(&m, &[0.0]), Ok(1.25));
    }

    #[test]
    fn a_multiclass_model_answers_k_probabilities_and_refuses_one_number() {
        let m = constant_model(Objective::Multiclass { class_count: 2 }, &[0.0, 0.0]);
        assert_eq!(Predictor::kind(&m), PredictionKind::ClassProbabilities { class_count: 2 });
        assert_eq!(Predictor::kind(&m).output_count(), 2);
        let mut out = [0.0; 2];
        Predictor::predict_into(&m, &[0.0], &mut out).unwrap();
        assert_eq!(out, [0.5, 0.5]);
        assert!(Predictor::predict_one(&m, &[0.0]).unwrap_err().contains("predict_into"));
        assert!(Predictor::predict_into(&m, &[0.0], &mut [0.0; 3]).is_err(), "a wrong buffer");
    }

    /// The default batch is `predict_into` row by row: the same numbers in row order, the first
    /// row's refusal, and a matrix that is not whole rows refused before any row.
    #[test]
    fn the_default_batch_answers_what_each_row_answers_in_row_order() {
        let m = constant_model(Objective::Multiclass { class_count: 2 }, &[0.0, 1.0]);
        let flat = [0.0, 1.0, 2.0, 3.0, 4.0, 5.0];
        let mut per_row = Vec::new();
        for row in flat.chunks(2) {
            let mut out = [0.0; 2];
            Predictor::predict_into(&m, row, &mut out).unwrap();
            per_row.extend(out);
        }
        assert_eq!(Predictor::predict_batch(&m, &flat, 2), Ok(per_row));
        assert_eq!(Predictor::predict_batch(&m, &[], 2), Ok(vec![]));
        assert!(Predictor::predict_batch(&m, &flat, 4).unwrap_err().contains("whole rows"));
        assert!(Predictor::predict_batch(&m, &flat, 0).unwrap_err().contains("whole rows"));

        struct RefusesNegative;
        impl Predictor for RefusesNegative {
            fn kind(&self) -> PredictionKind {
                PredictionKind::Value
            }
            fn predict_into(&self, row: &[f64], out: &mut [f64]) -> Result<(), String> {
                if row[0] < 0.0 {
                    return Err(format!("row {} refused", row[0]));
                }
                out[0] = row[0];
                Ok(())
            }
        }
        let error = RefusesNegative.predict_batch(&[1.0, -2.0, -3.0], 1).unwrap_err();
        assert_eq!(error, RefusesNegative.predict_one(&[-2.0]).unwrap_err(), "the first refusal");
    }

    /// Compile-time: a caller scoring a grid in parallel shares the model across threads, and a
    /// study holds one behind `dyn`.
    #[test]
    fn a_gbdt_model_is_a_shareable_object_safe_predictor() {
        fn send_sync<T: Send + Sync>() {}
        send_sync::<GbdtModel>();
        let boxed: Box<dyn Predictor> = Box::new(binary(0.0));
        assert_eq!(boxed.kind(), PredictionKind::Probability);
    }
}
