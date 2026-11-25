//! Hand-computed cases for `model_drift`: every expected number below is worked from the published
//! formula in the comment beside it, never captured from a run.

use super::*;

fn close_enough(a: f64, b: f64) -> bool {
    (a - b).abs() <= 1e-12 * b.abs().max(1.0)
}

#[test]
fn model_drift_compares_every_feature_column_and_the_prediction() {
    // Column 0: [1, 2, 3, 4] -> [1, 1, 1, 4] (the first PSI case; D = |1/4 - 3/4| = 1/2).
    // Column 1: [10, 20, 30, 40] -> the same values in another order (no drift).
    let training = [1.0, 10.0, 2.0, 20.0, 3.0, 30.0, 4.0, 40.0];
    let later = [1.0, 40.0, 1.0, 30.0, 1.0, 20.0, 4.0, 10.0];
    let predictions = [0.1, 0.2, 0.3, 0.4];
    let report = model_drift(&training, &later, 2, &predictions, &[0.6, 0.7, 0.8, 0.9], 2);

    assert_eq!(report.features.len(), 2);
    assert!(close_enough(report.features[0].population_stability_index, libm::log(3.0) / 4.0));
    assert_eq!(report.features[0].kolmogorov_smirnov.statistic, 0.5);
    assert_eq!(report.features[1].population_stability_index, 0.0);
    assert_eq!(
        report.features[1].kolmogorov_smirnov,
        KolmogorovSmirnov { statistic: 0.0, p_value: 1.0 }
    );
    // The predictions moved wholesale above the training ones: D = 1.
    assert_eq!(report.prediction.kolmogorov_smirnov.statistic, 1.0);
    assert_eq!(
        report.prediction,
        ModelDriftColumn {
            population_stability_index: population_stability_index(
                &predictions,
                &[0.6, 0.7, 0.8, 0.9],
                2
            ),
            kolmogorov_smirnov: kolmogorov_smirnov_two_sample(&predictions, &[0.6, 0.7, 0.8, 0.9]),
        }
    );
}

#[test]
#[should_panic(expected = "not whole 2-wide rows")]
fn model_drift_refuses_a_ragged_matrix() {
    model_drift(&[1.0, 2.0, 3.0], &[1.0, 2.0], 2, &[0.5], &[0.5], 2);
}

#[test]
#[should_panic(expected = "one prediction per row")]
fn model_drift_refuses_a_prediction_count_that_is_not_the_row_count() {
    model_drift(&[1.0, 2.0], &[1.0, 2.0], 2, &[0.5, 0.6], &[0.5], 2);
}
