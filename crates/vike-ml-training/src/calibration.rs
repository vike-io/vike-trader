//! PROBABILITY CALIBRATION, the scoring and fitting half: how good a set of forecast probabilities
//! is against what happened (proper scoring rules and the reliability diagram), and the two fits that
//! repair them, each ending in `vike-ml-prediction`'s [`Calibration`] and its
//! `vike-ml calibration v1` text, so a strategy file applies the fit exactly as it runs a model.
//!
//! `n` forecasts `p_i` in `[0, 1]` (the probability that event `i` happens) and `n` outcomes `o_i`
//! (`true`, read as 1, when it happened).
//!
//! # Scoring
//!
//! - [`brier_score`] (Brier, 1950): `(1/n) Σ (p_i − o_i)²`; 0 is perfect, always 1/4 a coin's.
//! - [`log_loss`]: `−(1/n) Σ [o_i ln p_i + (1 − o_i) ln(1 − p_i)]`. A forecast of exactly 0 for an
//!   event that happened (or 1 for one that did not) scores `+∞`: that is the rule's own answer.
//!   Each row's term is [`log_loss_of`], the workspace's ONE log loss: a caller that must not see
//!   `+∞` (a classifier scored per fold) passes a clip such as [`LOG_LOSS_CLIP`] to it.
//! - [`reliability_bins`]: the reliability diagram's points. `K` equal-width bins; bin `k` holds the
//!   forecasts with `⌊p·K⌋ = k` (the product rounded in floating point), so `[k/K, (k+1)/K)`, the
//!   last bin closed at 1. Each bin's count, mean forecast and observed frequency.
//! - [`expected_calibration_error`]: `Σ_k (n_k/n) |ō_k − p̄_k|` over those bins (Naeini, Cooper and
//!   Hauskrecht, "Obtaining well calibrated probabilities using Bayesian binning", 2015).
//! - [`brier_decomposition`]: Murphy's (1973) reliability `(1/n) Σ_k n_k (p̄_k − ō_k)²`, resolution
//!   `(1/n) Σ_k n_k (ō_k − ō)²` and uncertainty `ō (1 − ō)` over the same bins, plus the two
//!   within-bin terms of Stephenson, Coelho and Jolliffe ("Two extra components in the Brier score
//!   decomposition", 2008), `WBV = (1/n) Σ (p_i − p̄_k)²` and `WBC = (1/n) Σ (p_i − p̄_k)(o_i − ō_k)`,
//!   so that `brier = reliability − resolution + uncertainty + WBV − 2·WBC` holds for ANY forecasts,
//!   not only ones that take one value per bin.
//!
//! # Fitting (the module doc of `crates/vike-ml-prediction/src/calibration.rs` defines both maps)
//!
//! - [`fit_platt`]: the logistic regression of the outcome on `log_odds(p)` — [`fit_linear`]'s
//!   logistic fit over that one column, the penalty `λ` on the slope only (the intercept is free,
//!   so a penalty pulls toward the base rate, not toward the forecast). Platt's (1999) smoothed
//!   targets are not used: the logistic fit reads labels 0 and 1. Separable rows have no
//!   unpenalised fit and are refused by that fit, which asks for `λ > 0`.
//! - [`fit_isotonic`]: pool adjacent violators (Ayer et al., 1955) over the forecasts sorted
//!   ascending, EQUAL forecasts pooled first so they get one value; a step's value is the share of
//!   its events that happened. Steps are compared as integer counts, so the fit is exact. Of every
//!   non-decreasing function of the forecast it has the lowest Brier score on the rows it was fitted
//!   on, which is why it over-fits a small sample.
//!
//! ⚠ Fit a calibration on rows the forecaster was NOT fitted on, and EARLIER than every row it will
//! be applied to: a calibration fitted on its own evaluation rows reports a score it cannot repeat.
//!
//! Every sum is a naive left fold in row order, and the one transcendental beyond those
//! `vike-ml-prediction` owns is [`log_loss_of`]'s `libm::log` (decision 0032).

use vike_ml_prediction::calibration::{Calibration, log_odds};

use crate::TrainData;
use crate::linear::{LinearSpec, fit_linear};

/// One bin of a reliability diagram ([`reliability_bins`]).
#[derive(Clone, Debug, PartialEq)]
pub struct ReliabilityBin {
    /// `k/K`: the lowest forecast the bin holds.
    pub lower: f64,
    /// `(k+1)/K`: the bin holds forecasts below it (the last bin holds 1 too).
    pub upper: f64,
    pub count: usize,
    /// The mean of the bin's forecasts; NaN for an empty bin.
    pub mean_forecast: f64,
    /// The share of the bin's events that happened; NaN for an empty bin.
    pub observed_frequency: f64,
}

/// [`brier_decomposition`]'s five terms (the module doc defines each).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct BrierDecomposition {
    pub reliability: f64,
    pub resolution: f64,
    pub uncertainty: f64,
    pub within_bin_variance: f64,
    pub within_bin_covariance: f64,
}

/// One outcome per forecast, at least one row, every forecast a probability in `[0, 1]`.
fn check(forecasts: &[f64], outcomes: &[bool]) -> Result<(), String> {
    if forecasts.len() != outcomes.len() {
        return Err(format!(
            "{} forecasts and {} outcomes: one outcome per forecast",
            forecasts.len(),
            outcomes.len()
        ));
    }
    if forecasts.is_empty() {
        return Err("no forecast to score or fit".into());
    }
    if let Some(i) = forecasts.iter().position(|p| !(0.0..=1.0).contains(p)) {
        return Err(format!("forecast {i} is {}, not a probability in [0, 1]", forecasts[i]));
    }
    Ok(())
}

fn as_number(outcome: bool) -> f64 {
    if outcome { 1.0 } else { 0.0 }
}

/// The Brier score of `forecasts` against `outcomes`.
pub fn brier_score(forecasts: &[f64], outcomes: &[bool]) -> Result<f64, String> {
    check(forecasts, outcomes)?;
    let mut total = 0.0;
    for (p, &o) in forecasts.iter().zip(outcomes) {
        let error = p - as_number(o);
        total += error * error;
    }
    Ok(total / forecasts.len() as f64)
}

/// The clip `[1e-15, 1 − 1e-15]` a classifier's log loss is scored with: scikit-learn's `log_loss`
/// default `eps`, which AFML's snippets score with. Without it one confident miss is `−ln 0 = +∞`
/// and the whole fold's score is lost.
pub const LOG_LOSS_CLIP: f64 = 1e-15;

/// The log loss of ONE forecast, in nats: `−ln q`, where `q` is the probability the forecast gave
/// to what happened (`p` for an event that happened, `1 − p` for one that did not, the true
/// class's probability for a classifier), first held in `[clip, 1 − clip]`. `clip = 0` scores the
/// rule as it is (`q = 0` is `+∞`). `clip` must lie in `[0, 0.5]` (`f64::clamp` panics above).
pub fn log_loss_of(probability_of_truth: f64, clip: f64) -> f64 {
    -libm::log(probability_of_truth.clamp(clip, 1.0 - clip))
}

/// The mean log loss (cross-entropy) of `forecasts` against `outcomes`, in nats, unclipped.
pub fn log_loss(forecasts: &[f64], outcomes: &[bool]) -> Result<f64, String> {
    check(forecasts, outcomes)?;
    let mut total = 0.0;
    for (&p, &o) in forecasts.iter().zip(outcomes) {
        total += log_loss_of(if o { p } else { 1.0 - p }, 0.0);
    }
    Ok(total / forecasts.len() as f64)
}

/// The bin of the forecast `p` among `bin_count` equal-width bins.
fn bin_of(p: f64, bin_count: usize) -> usize {
    ((p * bin_count as f64) as usize).min(bin_count - 1)
}

/// The `bin_count` equal-width bins of the reliability diagram, empty ones included (with NaN
/// means), lowest first. Refused: no bin, or the inputs [`brier_score`] refuses.
pub fn reliability_bins(
    forecasts: &[f64],
    outcomes: &[bool],
    bin_count: usize,
) -> Result<Vec<ReliabilityBin>, String> {
    check(forecasts, outcomes)?;
    if bin_count == 0 {
        return Err("a reliability diagram needs at least one bin".into());
    }
    let mut count = vec![0usize; bin_count];
    let mut forecast_sum = vec![0.0; bin_count];
    let mut happened = vec![0.0; bin_count];
    for (&p, &o) in forecasts.iter().zip(outcomes) {
        let k = bin_of(p, bin_count);
        count[k] += 1;
        forecast_sum[k] += p;
        happened[k] += as_number(o);
    }
    Ok((0..bin_count)
        .map(|k| {
            let rows = count[k] as f64;
            let mean = |sum: f64| if count[k] == 0 { f64::NAN } else { sum / rows };
            ReliabilityBin {
                lower: k as f64 / bin_count as f64,
                upper: (k + 1) as f64 / bin_count as f64,
                count: count[k],
                mean_forecast: mean(forecast_sum[k]),
                observed_frequency: mean(happened[k]),
            }
        })
        .collect())
}

/// The expected calibration error over `bins` ([`reliability_bins`]); empty bins weigh nothing.
/// NaN when every bin is empty.
pub fn expected_calibration_error(bins: &[ReliabilityBin]) -> f64 {
    let rows: usize = bins.iter().map(|b| b.count).sum();
    let mut total = 0.0;
    for bin in bins.iter().filter(|b| b.count > 0) {
        total += bin.count as f64 * (bin.observed_frequency - bin.mean_forecast).abs();
    }
    total / rows as f64
}

/// The Brier score's decomposition over `bin_count` equal-width bins.
pub fn brier_decomposition(
    forecasts: &[f64],
    outcomes: &[bool],
    bin_count: usize,
) -> Result<BrierDecomposition, String> {
    let bins = reliability_bins(forecasts, outcomes, bin_count)?;
    let n = forecasts.len() as f64;
    let base_rate = outcomes.iter().filter(|&&o| o).count() as f64 / n;
    let (mut reliability, mut resolution) = (0.0, 0.0);
    for bin in bins.iter().filter(|b| b.count > 0) {
        let weight = bin.count as f64;
        let miscalibration = bin.mean_forecast - bin.observed_frequency;
        let separation = bin.observed_frequency - base_rate;
        reliability += weight * miscalibration * miscalibration;
        resolution += weight * separation * separation;
    }
    let (mut variance, mut covariance) = (0.0, 0.0);
    for (&p, &o) in forecasts.iter().zip(outcomes) {
        let bin = &bins[bin_of(p, bin_count)];
        let spread = p - bin.mean_forecast;
        variance += spread * spread;
        covariance += spread * (as_number(o) - bin.observed_frequency);
    }
    Ok(BrierDecomposition {
        reliability: reliability / n,
        resolution: resolution / n,
        uncertainty: base_rate * (1.0 - base_rate),
        within_bin_variance: variance / n,
        within_bin_covariance: covariance / n,
    })
}

/// Fit a Platt calibration of `forecasts` to `outcomes`, with the slope penalised by `penalty`
/// (`0` for the plain maximum-likelihood fit). Refused: the inputs [`brier_score`] refuses, rows
/// where only one outcome occurs (the intercept has no finite fit), and whatever [`fit_linear`]
/// refuses (separable rows without a penalty, a penalty out of range).
pub fn fit_platt(
    forecasts: &[f64],
    outcomes: &[bool],
    penalty: f64,
) -> Result<Calibration, String> {
    check(forecasts, outcomes)?;
    if outcomes.iter().all(|&o| o) || outcomes.iter().all(|&o| !o) {
        return Err("a Platt fit needs both outcomes among its rows; with one, the intercept has \
                    no finite fit"
            .into());
    }
    let x: Vec<f64> = forecasts.iter().map(|&p| log_odds(p)).collect();
    let y: Vec<f32> = outcomes.iter().map(|&o| if o { 1.0 } else { 0.0 }).collect();
    let data = TrainData::new(&x, &y, 1).map_err(|e| e.to_string())?;
    let model = fit_linear(&data, LinearSpec::Logistic { penalty, l1_ratio: 0.0 })?;
    Ok(Calibration::Platt { slope: model.coefficients[0], intercept: model.intercept })
}

/// Fit an isotonic calibration of `forecasts` to `outcomes`. Refused: the inputs [`brier_score`]
/// refuses.
pub fn fit_isotonic(forecasts: &[f64], outcomes: &[bool]) -> Result<Calibration, String> {
    check(forecasts, outcomes)?;
    /// A step: its lowest and highest forecast, its rows and how many of their events happened.
    struct Step {
        start: f64,
        end: f64,
        rows: u64,
        happened: u64,
    }
    let mut order: Vec<usize> = (0..forecasts.len()).collect();
    order.sort_by(|&a, &b| forecasts[a].total_cmp(&forecasts[b]));
    let mut steps: Vec<Step> = Vec::new();
    for i in order {
        let (p, happened) = (forecasts[i], u64::from(outcomes[i]));
        match steps.last_mut() {
            Some(last) if last.end == p => {
                last.rows += 1;
                last.happened += happened;
            }
            _ => steps.push(Step { start: p, end: p, rows: 1, happened }),
        }
        // Pool while the step before shares at least the last one's rate: a/b >= c/d ⇔ a·d >= c·b.
        while let [.., before, last] = steps.as_slice() {
            if before.happened * last.rows < last.happened * before.rows {
                break;
            }
            let last = steps.pop().expect("two steps");
            let before = steps.last_mut().expect("one step");
            before.end = last.end;
            before.rows += last.rows;
            before.happened += last.happened;
        }
    }
    Ok(Calibration::Isotonic {
        starts: steps.iter().map(|s| s.start).collect(),
        values: steps.iter().map(|s| s.happened as f64 / s.rows as f64).collect(),
    })
}

#[path = "calibration_tests.rs"]
#[cfg(test)]
mod calibration_tests;
