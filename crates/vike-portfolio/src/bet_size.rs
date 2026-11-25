//! The size of a bet from a classifier's predicted probability (López de Prado, *Advances in
//! Financial Machine Learning*, 2018, ch. 10).
//!
//! [`bet_size`] tests "the predicted label is right" against "every label is as likely":
//! `z = (p − 1/K) / √(p (1 − p))`, and sizes the bet `m = 2 Φ(z) − 1` (section 10.3, snippet 10.1
//! `getSignal`), in `[−1, 1]`: `0` at the uninformed `p = 1/K`, `±1` at certainty. Multiply it by the
//! predicted side (`+1` buy, `−1` sell) and by the position limit. [`discretise_bet_size`] rounds it
//! to a step so a position is not traded on every small change of `p` (snippet 10.3 `discreteSignal`).
//!
//! `Φ` is `vike_math::distributions::normal_cdf`, `libm`'s `erf`, so the size is the same bits everywhere.

use vike_math::distributions::normal_cdf;

/// `2 Φ(z) − 1` with `z = (p − 1/K) / √(p (1 − p))`, for the probability `probability` a classifier
/// gives its predicted label among `classes` labels.
///
/// `None` when `probability` is outside `[0, 1]` or `classes` is below 2.
pub fn bet_size(probability: f64, classes: usize) -> Option<f64> {
    if !(0.0..=1.0).contains(&probability) || classes < 2 {
        return None;
    }
    // `p (1 − p) = 0` at certainty, where `z` is `±∞` and the size `±1`.
    let z = (probability - 1.0 / classes as f64) / (probability * (1.0 - probability)).sqrt();
    Some(2.0 * normal_cdf(z) - 1.0)
}

/// `size` rounded to the nearest multiple of `step`, a half to the even multiple (NumPy's `round`,
/// which the book's snippet calls), and kept inside `[−1, 1]`.
///
/// `None` when `size` is not finite or `step` is not in `(0, 1]`.
pub fn discretise_bet_size(size: f64, step: f64) -> Option<f64> {
    if !size.is_finite() || step.is_nan() || step <= 0.0 || step > 1.0 {
        return None;
    }
    Some(((size / step).round_ties_even() * step).clamp(-1.0, 1.0))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `p = 0.7` of two labels: `z = 0.2 / √0.21 = 0.43644`, `Φ(z) = 0.66874` (normal table), so
    /// `m = 0.33747`. Uninformed, it is `0`; certain, `±1`.
    #[test]
    fn the_bet_size_by_hand() {
        assert!((bet_size(0.7, 2).unwrap() - 0.33747).abs() < 1e-4);
        assert_eq!(bet_size(0.5, 2), Some(0.0));
        assert_eq!(bet_size(1.0 / 3.0, 3), Some(0.0));
        assert_eq!(bet_size(1.0, 2), Some(1.0));
        assert_eq!(bet_size(0.0, 2), Some(-1.0));
        // Below `1/K` the predicted label is less likely than a guess: a negative size.
        assert!(bet_size(0.3, 2).unwrap() < 0.0);
        assert!(bet_size(1.5, 2).is_none());
        assert!(bet_size(f64::NAN, 2).is_none());
        assert!(bet_size(0.7, 1).is_none());
    }

    /// Halves go to the even multiple: `0.25 / 0.5 = 0.5 → 0`, `0.75 / 0.5 = 1.5 → 2`; a rounding past
    /// 1 is capped.
    #[test]
    fn the_discretised_bet_size_by_hand() {
        assert!((discretise_bet_size(0.33747, 0.1).unwrap() - 0.3).abs() < 1e-15);
        assert_eq!(discretise_bet_size(0.25, 0.5), Some(0.0));
        assert_eq!(discretise_bet_size(0.75, 0.5), Some(1.0));
        assert_eq!(discretise_bet_size(-0.75, 0.5), Some(-1.0));
        assert_eq!(discretise_bet_size(0.95, 0.6), Some(1.0));
        assert!(discretise_bet_size(0.5, 0.0).is_none());
        assert!(discretise_bet_size(0.5, 1.5).is_none());
        assert!(discretise_bet_size(f64::NAN, 0.1).is_none());
    }
}
