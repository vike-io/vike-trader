//! Property tests for the instrument graph's price features (`vike_analytics::instrument_graph`),
//! over random series and random weighted graphs:
//!
//! - `lead_lag` of `(a, b)` at lag `k` is `(b, a)` at lag `-k`, bit for bit, and the two lead-lag
//!   ratios are each other's inverse;
//! - eigenvector centrality is non-negative and of unit length whenever it answers.

use proptest::prelude::*;
use vike_analytics::instrument_graph::eigenvector_centrality;
use vike_math::time_series::lead_lag;

/// Two aligned series of 8 to 60 returns, and a lag bound that leaves at least two pairs.
fn pair() -> impl Strategy<Value = (Vec<f64>, Vec<f64>, usize)> {
    (8usize..60).prop_flat_map(|n| {
        (
            prop::collection::vec(-0.05f64..0.05, n),
            prop::collection::vec(-0.05f64..0.05, n),
            1usize..=(n - 2).min(6),
        )
    })
}

/// A symmetric weight matrix of 1 to 8 nodes, zero on the diagonal, each pair's weight in `[0, 2)`
/// or exactly zero (no edge).
fn graph() -> impl Strategy<Value = Vec<Vec<f64>>> {
    (1usize..=8).prop_flat_map(|n| {
        prop::collection::vec(prop_oneof![Just(0.0), 0.0f64..2.0], n * n).prop_map(move |draw| {
            let mut weights = vec![vec![0.0; n]; n];
            for i in 0..n {
                for j in 0..i {
                    weights[i][j] = draw[i * n + j];
                    weights[j][i] = draw[i * n + j];
                }
            }
            weights
        })
    })
}

proptest! {
    #[test]
    fn lead_lag_mirrors_when_the_two_series_swap((a, b, max_lag) in pair()) {
        let forward = lead_lag(&a, &b, max_lag).unwrap();
        let backward = lead_lag(&b, &a, max_lag).unwrap();
        let mirrored: Vec<u64> = backward.correlations.iter().rev().map(|c| c.to_bits()).collect();
        let bits: Vec<u64> = forward.correlations.iter().map(|c| c.to_bits()).collect();
        prop_assert_eq!(bits, mirrored);
        let (f, g) = (forward.lead_lag_ratio, backward.lead_lag_ratio);
        if f.is_infinite() {
            prop_assert_eq!(g, 0.0);
        } else if g.is_infinite() {
            prop_assert_eq!(f, 0.0);
        } else {
            prop_assert!((f * g - 1.0).abs() <= 1e-12, "{} · {} is not one", f, g);
        }
    }

    #[test]
    fn centrality_is_non_negative_with_unit_length(weights in graph()) {
        if let Some(scores) = eigenvector_centrality(&weights) {
            prop_assert_eq!(scores.len(), weights.len());
            prop_assert!(scores.iter().all(|s| *s >= 0.0), "{:?}", scores);
            let mut length = 0.0;
            for s in &scores {
                length += s * s;
            }
            prop_assert!((length - 1.0).abs() <= 1e-12, "squared length {}", length);
        }
    }
}
