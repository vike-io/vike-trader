//! SPLIT CONFORMAL calibration: the threshold `q̂` that turns a fitted model into
//! `vike-ml-prediction`'s [`ConformalModel`] — intervals for a regression, prediction sets for a
//! classifier — covering the truth with probability at least `1 − α` (the marginal guarantee of
//! split conformal prediction; Lei et al., "Distribution-free predictive inference for
//! regression", 2018, theorem 2.2).
//!
//! The recipe: fit the model on one part of the data, score every row of a SEPARATE calibration
//! part with `vike-ml-prediction`'s one [`nonconformity_score`], and take the
//! `⌈(n + 1)(1 − α)⌉`-th smallest of the `n` scores ([`conformal_threshold`]). With fewer than
//! `⌈1/α⌉ − 1` calibration rows that rank does not exist and no finite threshold covers: refused.
//!
//! ⚠ The guarantee needs the calibration rows and the future rows to be EXCHANGEABLE. Rows the model
//! was fitted on are not (their scores are optimistic), and neither are rows from a market that has
//! since changed: calibrate on the most recent held-out window, and re-calibrate as it ages.

use vike_ml_prediction::any_model::AnyModel;
use vike_ml_prediction::conformal::{ConformalModel, nonconformity_score};

use crate::TrainData;

/// The `⌈(n + 1)(1 − α)⌉`-th smallest of the `n` `scores`: the split-conformal threshold at
/// miscoverage `α` in `(0, 1)`. Refused: no score, a NaN score, an `α` out of range, or too few
/// scores for the rank to exist.
pub fn conformal_threshold(scores: &[f64], miscoverage: f64) -> Result<f64, String> {
    if miscoverage.is_nan() || miscoverage <= 0.0 || miscoverage >= 1.0 {
        return Err(format!("the miscoverage {miscoverage} is not in (0, 1)"));
    }
    if scores.iter().any(|s| s.is_nan()) {
        return Err("a calibration score is NaN".into());
    }
    let n = scores.len();
    let rank = ((n + 1) as f64 * (1.0 - miscoverage)).ceil() as usize;
    if rank == 0 || rank > n {
        return Err(format!(
            "{n} calibration rows cannot give a {}% coverage: the rank ⌈(n + 1)(1 − α)⌉ = {rank} \
             is past the last score; calibrate on more rows or raise the miscoverage",
            100.0 * (1.0 - miscoverage)
        ));
    }
    let mut sorted = scores.to_vec();
    sorted.sort_by(f64::total_cmp);
    Ok(sorted[rank - 1])
}

/// Calibrate `model` on `calibration` (rows the model was NOT fitted on; labels are targets for a
/// regression, class numbers for a classifier) at miscoverage `α`.
pub fn calibrate_conformal(
    model: AnyModel,
    calibration: &TrainData<'_>,
    miscoverage: f64,
) -> Result<ConformalModel, String> {
    calibration.validate().map_err(|e| e.to_string())?;
    let mut scores = Vec::with_capacity(calibration.n_rows);
    for (i, &label) in calibration.y.iter().enumerate() {
        let score = nonconformity_score(&model, calibration.row(i), f64::from(label))
            .map_err(|e| format!("calibration row {i}: {e}"))?;
        scores.push(score);
    }
    let threshold = conformal_threshold(&scores, miscoverage)?;
    ConformalModel::new(model, miscoverage, threshold).map_err(|e| e.to_string())
}

#[path = "conformal_tests.rs"]
#[cfg(test)]
mod conformal_tests;
