//! Hypothesis testing: the two-sided normal p-value of a test statistic and the Benjamini–Hochberg
//! false-discovery correction over many p-values.

use crate::distributions::normal_cdf_complementary_form;

/// Two-sided normal p-value for a t-statistic: `2 * (1 − Φ(|t|))`, with Φ the complementary form
/// (precise in the tail). A `NaN` statistic answers `1.0` (reject nothing).
///
/// The normal rather than Student's t: the series it is read on are thousands of bars long, where the
/// two agree to well past any threshold anyone gates on.
pub fn two_sided_normal_p_value(t_statistic: f64) -> f64 {
    if t_statistic.is_nan() {
        return 1.0;
    }
    (2.0 * (1.0 - normal_cdf_complementary_form(t_statistic.abs()))).clamp(0.0, 1.0)
}

/// Benjamini-Hochberg step-up procedure. Returns a rejection mask **in the caller's order**.
///
/// Sweeping 300 signal variants and keeping whichever cleared `p < 0.05` would produce ~15 false
/// positives from noise alone. This controls the expected FALSE-DISCOVERY RATE — the share of
/// rejections that are wrong — at `q`.
///
/// Step-UP is the part worth understanding: it finds the LARGEST rank `k` whose sorted p-value is
/// at or below `q·k/n`, then rejects every hypothesis at rank `k` and below. So a middling p-value
/// can be rejected on the strength of the ones beneath it, which is exactly the intended behaviour
/// and not a bug to "fix".
pub fn benjamini_hochberg(p_values: &[f64], q: f64) -> Vec<bool> {
    let n = p_values.len();
    if n == 0 {
        return Vec::new();
    }
    let mut order: Vec<usize> = (0..n).collect();
    order.sort_by(|a, b| {
        p_values[*a].partial_cmp(&p_values[*b]).unwrap_or(std::cmp::Ordering::Equal)
    });

    let mut largest_pass: Option<usize> = None;
    for (rank, index) in order.iter().enumerate() {
        let threshold = q * (rank as f64 + 1.0) / n as f64;
        if p_values[*index] <= threshold {
            largest_pass = Some(rank);
        }
    }

    let mut out = vec![false; n];
    if let Some(k) = largest_pass {
        for index in order.iter().take(k + 1) {
            out[*index] = true;
        }
    }
    out
}

#[cfg(test)]
#[path = "hypothesis_testing_tests.rs"]
mod tests;
