//! Sample weights for OVERLAPPING labels: concurrency, average uniqueness and the sequential
//! bootstrap (López de Prado, *Advances in Financial Machine Learning* ch. 4, snippets 4.1 to 4.5).
//!
//! Labels built over windows (`crate::labels`) overlap, so their outcomes are not independent and a
//! bagger that draws them uniformly over-samples the redundant ones. These functions measure the
//! overlap and draw with less of it.
//!
//! # The contract
//!
//! - A label span is `(start, end)`, bar indices, `start <= end`, INCLUSIVE at both ends: label `i`
//!   occupies every bar `t` with `start <= t <= end`. That is AFML's code (`count.loc[tIn:tOut] += 1`
//!   and the indicator matrix `indM.loc[t0:t1, i] = 1` of snippets 4.1 and 4.3), and its section
//!   4.5.3 example is reproduced exactly in the tests.
//! - Pure and deterministic: the bootstrap's draws come from a caller-given seed.
//!
//! # Panics
//!
//! Every function panics on a span with `start > end`, or on a drawn index out of range: both are
//! caller bugs, not data.

use rand::rngs::StdRng;
use rand::{RngExt, SeedableRng};
use vike_math::random::draw_from_cumulative;

/// How many labels occupy each bar: `c_t = Σ_i 1{start_i <= t <= end_i}` (AFML snippet 4.1).
/// The vector runs from bar 0 to the largest `end`.
pub fn label_concurrency(label_spans: &[(usize, usize)]) -> Vec<usize> {
    occupancy(label_spans, 0..label_spans.len())
}

/// Each label's average uniqueness `ū_i = mean_{t in span_i} 1 / c_t` (AFML snippet 4.2), in
/// `(0, 1]`: `1` for a label that shares no bar, `1 / k` for `k` labels on the same span. The
/// usual sample weight for a bagger and for the `ū`-sized bootstrap.
pub fn average_uniqueness(label_spans: &[(usize, usize)]) -> Vec<f64> {
    // Every bar of a label's own span counts the label itself, so `c_t` is `1 +` the others on it.
    let others: Vec<usize> =
        label_concurrency(label_spans).into_iter().map(|c| c.saturating_sub(1)).collect();
    uniqueness_beside(label_spans, &others)
}

/// The sequential bootstrap's draw probabilities after `drawn` (AFML section 4.5.3, snippet 4.5).
///
/// Candidate `i`'s uniqueness is its average uniqueness among the drawn labels plus itself,
/// `mean_{t in span_i} 1 / (1 + Σ_{j in drawn} 1{t in span_j})` (a label drawn twice counts twice),
/// and the probabilities are those uniquenesses normalised to sum to one. With nothing drawn every
/// label is equally likely.
pub fn sequential_bootstrap_probabilities(
    label_spans: &[(usize, usize)],
    drawn: &[usize],
) -> Vec<f64> {
    let occupied = occupancy(label_spans, drawn.iter().copied());
    normalised(uniqueness_beside(label_spans, &occupied))
}

/// `sample_size` label indices drawn by the sequential bootstrap (AFML snippet 4.5), seeded.
///
/// Each draw is taken from [`sequential_bootstrap_probabilities`] given the draws so far, so a label
/// that overlaps what is already in the sample becomes less likely. The stream is reproducible run
/// to run for one seed; it is not meant to match NumPy's generator.
pub fn sequential_bootstrap(
    label_spans: &[(usize, usize)],
    sample_size: usize,
    seed: u64,
) -> Vec<usize> {
    if label_spans.is_empty() {
        return Vec::new();
    }
    let mut occupied = occupancy(label_spans, std::iter::empty());
    let mut rng = StdRng::seed_from_u64(seed);
    let mut drawn = Vec::with_capacity(sample_size);
    for _ in 0..sample_size {
        let probabilities = normalised(uniqueness_beside(label_spans, &occupied));
        let draw = rng.random::<f64>();
        let cumulative: Vec<f64> = probabilities
            .iter()
            .scan(0.0, |total, p| {
                *total += p;
                Some(*total)
            })
            .collect();
        // The last label takes any rounding shortfall of the cumulative sum.
        let chosen = draw_from_cumulative(&cumulative, draw);
        let (start, end) = label_spans[chosen];
        for count in &mut occupied[start..=end] {
            *count += 1;
        }
        drawn.push(chosen);
    }
    drawn
}

/// Bars `0..=max end` of `label_spans`, each counting the `labels` (indices, repeats count) on it.
fn occupancy(label_spans: &[(usize, usize)], labels: impl Iterator<Item = usize>) -> Vec<usize> {
    let bars = label_spans.iter().map(|&(start, end)| {
        assert!(start <= end, "a label span ends before it starts: ({start}, {end})");
        end + 1
    });
    let mut counts = vec![0; bars.max().unwrap_or(0)];
    for i in labels {
        let (start, end) = label_spans[i];
        for count in &mut counts[start..=end] {
            *count += 1;
        }
    }
    counts
}

/// Each label's average of `1 / (1 + occupied_t)` over its own span.
fn uniqueness_beside(label_spans: &[(usize, usize)], occupied: &[usize]) -> Vec<f64> {
    label_spans
        .iter()
        .map(|&(start, end)| {
            let mut total = 0.0;
            for &c in &occupied[start..=end] {
                total += 1.0 / (c + 1) as f64;
            }
            total / (end - start + 1) as f64
        })
        .collect()
}

/// `weights` over their compensated total; every entry `NaN` when the total is not a positive finite
/// number (it is, for every uniqueness this module computes: each is in `(0, 1]`).
fn normalised(weights: Vec<f64>) -> Vec<f64> {
    vike_math::statistics::normalised_to_unit_sum(&weights)
        .unwrap_or_else(|| vec![f64::NAN; weights.len()])
}

#[cfg(test)]
mod tests {
    use super::*;

    /// AFML section 4.5.3: y1 on r(0,3), y2 on r(2,4), y3 on r(4,6), written as snippet 4.7's
    /// `t1 = pd.Series([2, 3, 5], index=[0, 2, 4])`.
    const AFML_SPANS: [(usize, usize); 3] = [(0, 2), (2, 3), (4, 5)];

    fn assert_close(actual: &[f64], expected: &[f64]) {
        assert_eq!(actual.len(), expected.len());
        for (a, e) in actual.iter().zip(expected) {
            assert!((a - e).abs() < 1e-14, "{actual:?} != {expected:?}");
        }
    }

    #[test]
    fn concurrency_and_average_uniqueness_of_the_afml_example() {
        assert_eq!(label_concurrency(&AFML_SPANS), vec![1, 1, 2, 1, 1, 1]);
        // y1: (1 + 1 + 1/2) / 3; y2: (1/2 + 1) / 2; y3: 1.
        assert_close(&average_uniqueness(&AFML_SPANS), &[5.0 / 6.0, 3.0 / 4.0, 1.0]);
    }

    #[test]
    fn sequential_bootstrap_probabilities_of_the_afml_example() {
        assert_close(&sequential_bootstrap_probabilities(&AFML_SPANS, &[]), &[1.0 / 3.0; 3]);
        // After y2: ū = (5/6, 1/2, 1) -> δ = (5/14, 3/14, 6/14).
        assert_close(
            &sequential_bootstrap_probabilities(&AFML_SPANS, &[1]),
            &[5.0 / 14.0, 3.0 / 14.0, 6.0 / 14.0],
        );
        // After y2, y3: ū = (5/6, 1/2, 1/2) -> δ = (5/11, 3/11, 3/11).
        assert_close(
            &sequential_bootstrap_probabilities(&AFML_SPANS, &[1, 2]),
            &[5.0 / 11.0, 3.0 / 11.0, 3.0 / 11.0],
        );
    }

    #[test]
    fn the_bootstrap_is_seeded_and_in_range() {
        let a = sequential_bootstrap(&AFML_SPANS, 50, 7);
        assert_eq!(a, sequential_bootstrap(&AFML_SPANS, 50, 7));
        assert_eq!(a.len(), 50);
        assert!(a.iter().all(|&i| i < 3));
        assert!(sequential_bootstrap(&[], 5, 7).is_empty());
    }

    #[test]
    #[should_panic(expected = "ends before it starts")]
    fn a_reversed_span_is_refused() {
        label_concurrency(&[(3, 2)]);
    }
}
