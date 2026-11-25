//! Model DRIFT: is what a model sees now still what it was trained on? [`model_drift`] compares
//! every feature column and the prediction between a TRAINING window and a LATER window, with the
//! population stability index and the two-sample Kolmogorov–Smirnov test of
//! `vike_math::change_detection` (decision 0294: the statistics, and the sequential detectors
//! `page_hinkley_alarms` and `cusum_alarms` a study runs over an error stream, are `vike-math`'s; the
//! report over `vike_ml_training::TrainData`'s row-major matrix is this crate's). It takes the matrix
//! `vike_ml_training::TrainData::x` holds, so a study hands over what it already has.
//!
//! # The contract
//!
//! - Pure functions: no clock, no file, no randomness; `libm` for the transcendentals (decision 0032).
//! - Only FINITE values are compared: a NaN or infinite entry is left out of its sample before a
//!   comparison. A comparison with an empty (or all non-finite) sample answers NaN.
//! - A caller's shape error (zero bins, zero columns, a ragged matrix, a prediction count that is not
//!   the row count) panics: it is a bug in the caller, not data.

use vike_math::change_detection::{
    KolmogorovSmirnov, kolmogorov_smirnov_two_sample, population_stability_index,
};

/// [`population_stability_index`] and [`kolmogorov_smirnov_two_sample`] of one column.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ModelDriftColumn {
    /// The column's [`population_stability_index`], training as the reference.
    pub population_stability_index: f64,
    /// The column's [`kolmogorov_smirnov_two_sample`], training as the reference.
    pub kolmogorov_smirnov: KolmogorovSmirnov,
}

/// What [`model_drift`] answers: one [`ModelDriftColumn`] per feature, and one for the prediction.
#[derive(Clone, Debug, PartialEq)]
pub struct ModelDrift {
    /// One entry per feature column, in column order.
    pub features: Vec<ModelDriftColumn>,
    /// The model's prediction, one value per row (what `vike_ml_prediction::Predictor` answers).
    pub prediction: ModelDriftColumn,
}

/// The drift of every feature and of the prediction from a TRAINING window to a LATER window.
///
/// `training_features` and `later_features` are row-major matrices `n_features` wide (the layout of
/// `vike_ml_training::TrainData::x`); `training_predictions` and `later_predictions` hold one prediction per
/// row of the matching matrix. Each column is compared with [`population_stability_index`] over
/// `bins` bins and [`kolmogorov_smirnov_two_sample`], training as the reference. Panics on a shape
/// error: `n_features == 0`, a matrix that is not whole rows, a prediction count that is not its
/// matrix's row count, or `bins == 0`.
pub fn model_drift(
    training_features: &[f64],
    later_features: &[f64],
    n_features: usize,
    training_predictions: &[f64],
    later_predictions: &[f64],
    bins: usize,
) -> ModelDrift {
    assert!(n_features > 0, "model_drift needs at least one feature column");
    for (name, matrix, predictions) in [
        ("training", training_features, training_predictions),
        ("later", later_features, later_predictions),
    ] {
        assert!(
            matrix.len().is_multiple_of(n_features),
            "the {name} matrix ({} values) is not whole {n_features}-wide rows",
            matrix.len()
        );
        assert_eq!(
            predictions.len(),
            matrix.len() / n_features,
            "the {name} window needs one prediction per row"
        );
    }
    let column = |matrix: &[f64], index: usize| -> Vec<f64> {
        matrix.iter().skip(index).step_by(n_features).copied().collect()
    };
    let features = (0..n_features)
        .map(|index| {
            column_drift(&column(training_features, index), &column(later_features, index), bins)
        })
        .collect();
    ModelDrift { features, prediction: column_drift(training_predictions, later_predictions, bins) }
}

fn column_drift(reference: &[f64], current: &[f64], bins: usize) -> ModelDriftColumn {
    ModelDriftColumn {
        population_stability_index: population_stability_index(reference, current, bins),
        kolmogorov_smirnov: kolmogorov_smirnov_two_sample(reference, current),
    }
}

#[path = "model_drift_tests.rs"]
#[cfg(test)]
mod model_drift_tests;
