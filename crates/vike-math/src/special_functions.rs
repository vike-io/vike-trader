//! The logistic pair and the softmax: [`logistic`] `1/(1+exp(−x))` and its inverse [`logit`]
//! `ln(p/(1−p))`, one copy each for the model link and sigmoid, the Platt calibration, the GARCH search
//! coordinates, the overfitting logits and the LMSR quote; [`softmax`], LightGBM's steps, for every
//! classifier's class probabilities.
//!
//! ⚠ Results here are compared BIT FOR BIT (the model probes, the market-making `PINNED` rows), so an
//! expression's operand order is part of its contract. Every `exp` and `log` is `libm`'s (decision
//! 0032).

/// The logistic function `1 / (1 + exp(−x))`, with `exp` from `libm`: `0` at `−∞`, `½` at 0, `1` at
/// `+∞` (it saturates, never NaN for an infinite `x`). The binary sigmoid of a gradient-boosted
/// model and the logistic link of a linear one go through here, so LightGBM's `1/(1+exp(-s·raw))`
/// is matched bit for bit: `-x` then `exp`, never `exp(x)/(1+exp(x))`.
pub fn logistic(x: f64) -> f64 {
    1.0 / (1.0 + libm::exp(-x))
}

/// The logit (log-odds) `ln(p / (1 − p))`, with `log` from `libm`: the inverse of [`logistic`].
/// UNCLAMPED: `p = 0` is `−∞`, `p = 1` is `+∞` and a `p` outside `[0, 1]` is NaN. A caller that must
/// stay finite clamps first (`vike-ml-prediction`'s `calibration::log_odds`).
pub fn logit(p: f64) -> f64 {
    libm::log(p / (1.0 - p))
}

/// **The one softmax**, in place: `scores[i]` becomes `exp(scores[i] - max) / Σ exp(scores[j] - max)`,
/// with `exp` from `libm`. The steps are LightGBM's `Common::Softmax`, in its order: the max taken
/// left to right as `if score < max { max } else { score }`, then each `exp(score - max)` summed left
/// to right, then one division each. A classifier's class probabilities (`vike-nlp`'s sentiment head,
/// a multiclass booster's) go through here, so two crates never disagree in the last bit.
/// An empty slice is left empty.
pub fn softmax(scores: &mut [f64]) {
    let Some((&first, rest)) = scores.split_first() else {
        return;
    };
    let mut max = first;
    for &score in rest {
        max = if score < max { max } else { score };
    }
    let mut sum = 0.0;
    for score in scores.iter_mut() {
        *score = libm::exp(*score - max);
        sum += *score;
    }
    for score in scores.iter_mut() {
        *score /= sum;
    }
}

#[cfg(test)]
#[path = "special_functions_tests.rs"]
mod tests;
