//! Change detection: is a sample still drawn from what it was compared against, and where in a
//! stream did its mean move?
//!
//! - **Comparing one column between two samples**: a REFERENCE sample and a CURRENT one.
//!   [`population_stability_index`] (PSI) measures how much mass moved between the reference's
//!   quantile bins; [`kolmogorov_smirnov_two_sample`] is the two-sample Kolmogorov–Smirnov test (the
//!   largest gap between the two empirical CDFs, with its asymptotic p-value).
//! - **Sequential detectors over a stream** (oldest first) that name the positions where its mean
//!   ROSE: [`page_hinkley_alarms`] and [`cusum_alarms`].
//! - **The symmetric CUSUM filter** ([`cusum_events_of_series`], AFML snippet 2.4, over a series'
//!   differences; [`symmetric_cusum`], over the steps themselves): the workspace's ONE CUSUM
//!   recursion, which [`cusum_alarms`] runs too.
//!
//! # The contract
//!
//! - Pure functions: no clock, no file, no randomness; `libm` for the transcendentals (decision 0032).
//! - Only FINITE values are compared: a NaN or infinite entry is left out of its sample before a
//!   comparison, and a non-finite value is skipped by both detectors (it neither moves nor resets
//!   one). A comparison with an empty (or all non-finite) sample answers NaN.
//! - A detector alarm is a position in the stream. A detector RESTARTS after each alarm, so a stream
//!   may raise several.
//! - A caller's shape error (zero bins) panics: it is a bug in the caller, not data.

use crate::statistics::percentile;
use crate::summation::compensated_sum;

/// The share a bin with NO observation is given in [`population_stability_index`], so that its
/// `ln(current / reference)` term stays finite. `1e-4` is the conventional floor (0.01% of a sample);
/// it bounds one empty bin's term to roughly `share × 9.2`.
pub const POPULATION_STABILITY_EMPTY_BIN_SHARE: f64 = 1e-4;

/// The population stability index of `current` against `reference`, over `bins` bins cut at the
/// reference's quantiles.
///
/// The interior edges are the reference's `k / bins` quantiles, `k = 1 .. bins - 1`
/// ([`percentile`]'s linear interpolation); equal edges (a feature with ties) merge, so a constant
/// reference has fewer bins than asked. Bin `k` holds `(edge[k-1], edge[k]]`, the first is open
/// below and the last open above. With `e` the reference's share of a bin and `a` the current one's,
/// each floored at [`POPULATION_STABILITY_EMPTY_BIN_SHARE`]:
///
/// `PSI = Σ (a - e) · ln(a / e)`
///
/// It is `0` for two samples with the same bin shares and grows with the moved mass; the customary
/// reading is below `0.1` stable, `0.1 .. 0.25` a moderate shift, above `0.25` a major one. NaN when
/// either sample has no finite value. Panics when `bins == 0`.
pub fn population_stability_index(reference: &[f64], current: &[f64], bins: usize) -> f64 {
    assert!(bins > 0, "population_stability_index needs at least one bin");
    let reference = sorted_finite(reference);
    let current = sorted_finite(current);
    if reference.is_empty() || current.is_empty() {
        return f64::NAN;
    }
    let mut edges: Vec<f64> =
        (1..bins).map(|k| percentile(&reference, k as f64 / bins as f64)).collect();
    edges.dedup();
    let reference_shares = bin_shares(&reference, &edges);
    let current_shares = bin_shares(&current, &edges);
    compensated_sum(reference_shares.iter().zip(&current_shares).map(|(&expected, &actual)| {
        let expected = expected.max(POPULATION_STABILITY_EMPTY_BIN_SHARE);
        let actual = actual.max(POPULATION_STABILITY_EMPTY_BIN_SHARE);
        (actual - expected) * libm::log(actual / expected)
    }))
}

/// The two-sample Kolmogorov–Smirnov test of [`kolmogorov_smirnov_two_sample`].
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct KolmogorovSmirnov {
    /// `D = sup_x |F_reference(x) - F_current(x)|` over the two empirical CDFs, in `[0, 1]`.
    pub statistic: f64,
    /// The probability of a `D` at least this large when both samples come from one continuous
    /// distribution, by the asymptotic Kolmogorov distribution (see
    /// [`kolmogorov_smirnov_two_sample`]). Small means the samples differ.
    pub p_value: f64,
}

/// The two-sample Kolmogorov–Smirnov test of `current` against `reference`.
///
/// `D` is exact, ties included (both CDFs step past a shared value together). The p-value is
/// Numerical Recipes' `kstwo`: `Q_KS((√nₑ + 0.12 + 0.11 / √nₑ) · D)` with `nₑ = n·m / (n + m)` and
/// `Q_KS(λ) = 2 Σ_{j≥1} (-1)^{j-1} e^{-2 j² λ²}` (Stephens' correction makes it usable from
/// `nₑ ≈ 4`). It assumes a continuous distribution, so on a heavily tied (discrete) column it is
/// conservative. Both fields are NaN when either sample has no finite value.
pub fn kolmogorov_smirnov_two_sample(reference: &[f64], current: &[f64]) -> KolmogorovSmirnov {
    let reference = sorted_finite(reference);
    let current = sorted_finite(current);
    if reference.is_empty() || current.is_empty() {
        return KolmogorovSmirnov { statistic: f64::NAN, p_value: f64::NAN };
    }
    let (n, m) = (reference.len() as f64, current.len() as f64);
    let (mut i, mut j, mut statistic) = (0, 0, 0.0_f64);
    while i < reference.len() && j < current.len() {
        let x = reference[i].min(current[j]);
        while i < reference.len() && reference[i] <= x {
            i += 1;
        }
        while j < current.len() && current[j] <= x {
            j += 1;
        }
        statistic = statistic.max((i as f64 / n - j as f64 / m).abs());
    }
    // Past the end of either sample the gap only closes, so the maximum is already taken.
    let effective = f64::sqrt(n * m / (n + m));
    let p_value = kolmogorov_survival((effective + 0.12 + 0.11 / effective) * statistic);
    KolmogorovSmirnov { statistic, p_value }
}

/// `Q_KS(λ) = 2 Σ_{j≥1} (-1)^{j-1} e^{-2 j² λ²}`, the Kolmogorov distribution's upper tail
/// (Numerical Recipes' `probks`). Where the alternating series has not converged in 100 terms (a
/// tiny `λ`), the tail is 1 to double precision and 1 is the answer, as `probks` gives.
fn kolmogorov_survival(lambda: f64) -> f64 {
    let exponent = -2.0 * lambda * lambda;
    let (mut sum, mut sign) = (0.0_f64, 2.0_f64);
    for j in 1..=100_u32 {
        let j = f64::from(j);
        let term = sign * libm::exp(exponent * j * j);
        sum += term;
        if term.abs() <= f64::EPSILON * sum.abs() {
            return sum.clamp(0.0, 1.0);
        }
        sign = -sign;
    }
    1.0
}

/// The Page–Hinkley test over an error stream: the positions where its mean rose.
///
/// With `x̄ₜ` the running mean of the errors since the last restart (`xₜ` included),
/// `mₜ = Σ (xᵢ - x̄ᵢ - tolerance)` and `Mₜ = min(m₁ .. mₜ)`, an alarm is position `t` where
/// `mₜ - Mₜ > threshold`; the detector then restarts empty (Page 1954; Hinkley 1971; the streaming
/// form of Gama et al., "A survey on concept drift adaptation", 2014). `tolerance` is the rise the
/// test ignores and `threshold` trades detection delay against false alarms. A FALL in the mean is
/// the same call on the negated errors.
pub fn page_hinkley_alarms(errors: &[f64], tolerance: f64, threshold: f64) -> Vec<usize> {
    let mut alarms = Vec::new();
    let (mut count, mut mean, mut cumulative, mut minimum) =
        (0_usize, 0.0_f64, 0.0_f64, f64::INFINITY);
    for (position, &error) in errors.iter().enumerate() {
        if !error.is_finite() {
            continue;
        }
        count += 1;
        mean += (error - mean) / count as f64;
        cumulative += error - mean - tolerance;
        minimum = minimum.min(cumulative);
        if cumulative - minimum > threshold {
            alarms.push(position);
            (count, mean, cumulative, minimum) = (0, 0.0, 0.0, f64::INFINITY);
        }
    }
    alarms
}

/// Page's one-sided CUSUM over an error stream: the positions where its mean rose above
/// `reference_mean + allowance`.
///
/// `Sₜ = max(0, Sₜ₋₁ + xₜ - reference_mean - allowance)`, an alarm where `Sₜ > threshold`, and `S`
/// restarts at 0 after it (Page, "Continuous inspection schemes", 1954). `reference_mean` is the
/// in-control error (the training window's mean), `allowance` is half the shift worth detecting. A
/// FALL in the mean is the same call on the negated errors and the negated reference mean.
///
/// ⚠ It is [`symmetric_cusum`]'s recursion, the workspace's ONE CUSUM, not a second copy: fed
/// the steps `xₜ - reference_mean - allowance` themselves, the
/// filter's upper sum IS `Sₜ`, exactly, and an event of the filter is the UPPER side's exactly when
/// its step is positive (each side keeps its own sum, and the lower one can only cross on a negative
/// step).
pub fn cusum_alarms(
    errors: &[f64],
    reference_mean: f64,
    allowance: f64,
    threshold: f64,
) -> Vec<usize> {
    let steps: Vec<f64> = errors
        .iter()
        .map(|&error| if error.is_finite() { error - reference_mean - allowance } else { 0.0 })
        .collect();
    symmetric_cusum(steps.iter().copied(), threshold)
        .into_iter()
        .filter(|&position| steps[position] > 0.0)
        .collect()
}

/// The bars at which a symmetric CUSUM filter fires (AFML snippet 2.4).
///
/// Over `series[i] - series[i - 1]`, `S⁺ = max(0, S⁺ + Δ)` and `S⁻ = min(0, S⁻ + Δ)`. An event is
/// bar `i` when `S⁻ < -threshold` (then `S⁻` resets to 0) or else when `S⁺ > threshold` (then `S⁺`
/// resets). Only the side that fired resets, exactly as the snippet writes it, and both tests are
/// STRICT. Feed it log prices to sample on a cumulative return of `threshold`.
///
/// Returns the event bars in increasing order; bar 0 is never one (it has no difference).
pub fn cusum_events_of_series(series: &[f64], threshold: f64) -> Vec<usize> {
    symmetric_cusum(series.windows(2).map(|pair| pair[1] - pair[0]), threshold)
        .into_iter()
        .map(|step| step + 1)
        .collect()
}

/// [`cusum_events_of_series`] fed the differences themselves: the positions in `steps` where it
/// fires. The one CUSUM recursion; [`cusum_alarms`] hands it its steps directly, so its upper sum is
/// Page's `Sₜ` exactly, with no running sum's rounding in between.
pub fn symmetric_cusum(steps: impl IntoIterator<Item = f64>, threshold: f64) -> Vec<usize> {
    let mut events = Vec::new();
    let (mut positive, mut negative) = (0.0_f64, 0.0_f64);
    for (i, difference) in steps.into_iter().enumerate() {
        positive = (positive + difference).max(0.0);
        negative = (negative + difference).min(0.0);
        if negative < -threshold {
            negative = 0.0;
            events.push(i);
        } else if positive > threshold {
            positive = 0.0;
            events.push(i);
        }
    }
    events
}

/// The finite values of `sample`, ascending.
fn sorted_finite(sample: &[f64]) -> Vec<f64> {
    let mut finite: Vec<f64> = sample.iter().copied().filter(|value| value.is_finite()).collect();
    finite.sort_by(f64::total_cmp);
    finite
}

/// The share of `sorted` in each bin cut by the ascending `edges`: `edges.len() + 1` shares.
fn bin_shares(sorted: &[f64], edges: &[f64]) -> Vec<f64> {
    let total = sorted.len() as f64;
    let mut shares = Vec::with_capacity(edges.len() + 1);
    let mut lower = 0;
    for &edge in edges {
        let upper = sorted.partition_point(|&value| value <= edge);
        shares.push((upper - lower) as f64 / total);
        lower = upper;
    }
    shares.push((sorted.len() - lower) as f64 / total);
    shares
}

#[cfg(test)]
#[path = "change_detection_tests.rs"]
mod tests;
