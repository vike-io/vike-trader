//! Property tests for the supervised-learning research modules: `labels`, `sample_weights`,
//! `factor_evaluation` and `validation::purged_kfold_label_spans`. The hand-computed cases live
//! beside each module; these hold the invariants over random inputs. A failure seed belongs beside
//! this file, as `research_property_tests.proptest-regressions`.

use proptest::prelude::*;
use vike_analytics::factor_evaluation::{quantile_returns, rank_information_coefficient};
use vike_analytics::labels::{
    TripleBarrierLabelling, trend_scanning_labels, triple_barrier_labels,
};
use vike_analytics::sample_weights::{
    average_uniqueness, sequential_bootstrap, sequential_bootstrap_probabilities,
};
use vike_analytics::validation::{
    combinatorial_purged_label_spans, combinatorial_purged_splits, purged_kfold_indices,
    purged_kfold_label_spans,
};

/// Label spans sorted by start: each start a gap after the previous one, each a length.
fn spans() -> impl Strategy<Value = Vec<(usize, usize)>> {
    prop::collection::vec((0usize..3, 0usize..6), 1..30).prop_map(|steps| {
        let mut start = 0;
        steps
            .into_iter()
            .map(|(gap, length)| {
                start += gap;
                (start, start + length)
            })
            .collect()
    })
}

/// A positive price path.
fn prices() -> impl Strategy<Value = Vec<f64>> {
    prop::collection::vec(-0.05f64..0.05, 2..60).prop_map(|steps| {
        let mut price = 100.0;
        let mut out = vec![price];
        for step in steps {
            price *= 1.0 + step;
            out.push(price);
        }
        out
    })
}

proptest! {
    #[test]
    fn uniqueness_is_in_the_unit_interval_and_probabilities_sum_to_one(
        spans in spans(),
        draws in prop::collection::vec(any::<prop::sample::Index>(), 0..10),
    ) {
        for u in average_uniqueness(&spans) {
            prop_assert!(u > 0.0 && u <= 1.0, "{u}");
        }
        let drawn: Vec<usize> = draws.iter().map(|d| d.index(spans.len())).collect();
        let p = sequential_bootstrap_probabilities(&spans, &drawn);
        prop_assert!(p.iter().all(|&x| x > 0.0));
        prop_assert!((p.iter().copied().fold(0.0, |a, b| a + b) - 1.0).abs() < 1e-12);
        let sample = sequential_bootstrap(&spans, 20, 11);
        prop_assert!(sample.len() == 20 && sample.iter().all(|&i| i < spans.len()));
    }

    #[test]
    fn disjoint_labels_are_fully_unique(lengths in prop::collection::vec(0usize..5, 1..20)) {
        let mut start = 0;
        let spans: Vec<(usize, usize)> = lengths
            .into_iter()
            .map(|length| {
                let span = (start, start + length);
                start += length + 1;
                span
            })
            .collect();
        prop_assert!(average_uniqueness(&spans).iter().all(|&u| u == 1.0));
    }

    #[test]
    fn a_purged_split_never_trains_on_a_label_overlapping_its_test_window(
        spans in spans(),
        k in 1usize..6,
        embargo in 0usize..4,
    ) {
        let splits = purged_kfold_label_spans(&spans, k, embargo);
        let mut tested: Vec<usize> = splits.iter().flat_map(|(_, test)| test.clone()).collect();
        tested.sort_unstable();
        prop_assert_eq!(tested, (0..spans.len()).collect::<Vec<_>>());
        for (train, test) in &splits {
            if test.is_empty() {
                continue;
            }
            let window_start = test.iter().map(|&i| spans[i].0).min().unwrap();
            let window_end = test.iter().map(|&i| spans[i].1).max().unwrap();
            for &i in train {
                prop_assert!(!test.contains(&i));
                let (start, end) = spans[i];
                prop_assert!(start >= window_end || end <= window_start, "{i} overlaps");
            }
        }
    }

    #[test]
    fn point_labels_reproduce_the_point_purged_kfold(
        n in 1usize..40,
        k in 1usize..6,
        embargo in 0usize..5,
    ) {
        // With k > n a fold is EMPTY, and the point splitter still embargoes after it (the seed
        // beside this file); a label window does not exist there, so the two agree only for k <= n.
        prop_assume!(k <= n);
        let points: Vec<(usize, usize)> = (0..n).map(|i| (i, i)).collect();
        prop_assert_eq!(purged_kfold_label_spans(&points, k, embargo), purged_kfold_indices(n, k, embargo));
    }

    #[test]
    fn a_combinatorial_purged_split_never_trains_on_a_label_overlapping_a_test_group(
        spans in spans(),
        n_groups in 2usize..6,
        n_test_groups in 1usize..3,
        embargo in 0usize..4,
    ) {
        prop_assume!(n_test_groups < n_groups && n_groups <= spans.len());
        let bounds = vike_analytics::validation::group_bounds(spans.len(), n_groups);
        for (train, test) in combinatorial_purged_label_spans(&spans, n_groups, n_test_groups, embargo) {
            for &(t0, t1) in bounds.iter().filter(|(t0, t1)| t0 < t1 && test.contains(t0)) {
                let window_start = spans[t0..t1].iter().map(|s| s.0).min().unwrap();
                let window_end = spans[t0..t1].iter().map(|s| s.1).max().unwrap();
                for &i in &train {
                    prop_assert!(!test.contains(&i));
                    let (start, end) = spans[i];
                    prop_assert!(start >= window_end || end <= window_start, "{i} overlaps");
                }
            }
        }
    }

    #[test]
    fn point_labels_reproduce_the_combinatorial_point_splits(
        n in 2usize..40,
        n_groups in 2usize..6,
        n_test_groups in 1usize..3,
        embargo in 0usize..5,
    ) {
        prop_assume!(n_test_groups < n_groups && n_groups <= n);
        let points: Vec<(usize, usize)> = (0..n).map(|i| (i, i)).collect();
        prop_assert_eq!(
            combinatorial_purged_label_spans(&points, n_groups, n_test_groups, embargo),
            combinatorial_purged_splits(n, n_groups, n_test_groups, embargo)
        );
    }

    #[test]
    fn triple_barrier_labels_end_inside_the_barriers(
        close in prices(),
        width in 0.001f64..0.05,
        profit_taking_multiple in 0.0f64..3.0,
        stop_loss_multiple in 0.0f64..3.0,
        holding in 1usize..20,
        meta in any::<bool>(),
    ) {
        let n = close.len();
        let events: Vec<usize> = (0..n).collect();
        let side: Vec<i8> = (0..n).map(|i| if i % 3 == 0 { -1 } else { 1 }).collect();
        let barriers = TripleBarrierLabelling {
            profit_taking_multiple,
            stop_loss_multiple,
            max_holding_bars: Some(holding),
            minimum_target: 0.0,
        };
        let labels = triple_barrier_labels(
            &close,
            &vec![width; n],
            &events,
            barriers,
            meta.then_some(side.as_slice()),
        );
        prop_assert_eq!(labels.len(), n);
        for l in labels {
            prop_assert!(l.start <= l.end && l.end <= (l.start + holding).min(n - 1));
            let direction = if meta { f64::from(side[l.start]) } else { 1.0 };
            prop_assert_eq!(l.realized_return, (close[l.end] / close[l.start] - 1.0) * direction);
            if meta {
                prop_assert_eq!(l.label, i8::from(l.realized_return > 0.0));
            } else {
                prop_assert!(l.label == -1 || l.label == 0 || l.label == 1);
            }
        }
    }

    #[test]
    fn cusum_events_are_increasing_bars(series in prices(), threshold in 0.0f64..5.0) {
        let events = vike_math::change_detection::cusum_events_of_series(&series, threshold);
        prop_assert!(events.windows(2).all(|pair| pair[0] < pair[1]));
        prop_assert!(events.iter().all(|&i| i >= 1 && i < series.len()));
    }

    #[test]
    fn trend_scanning_labels_are_the_sign_of_a_window_in_range(
        series in prices(),
        min_horizon in 3usize..6,
        extra in 0usize..6,
    ) {
        let max_horizon = min_horizon + extra;
        let events: Vec<usize> = (0..series.len()).collect();
        for l in trend_scanning_labels(&series, &events, min_horizon, max_horizon) {
            let length = l.trend_end - l.start + 1;
            prop_assert!(length >= min_horizon && length <= max_horizon);
            prop_assert_eq!(l.end, l.start + max_horizon - 1);
            let sign = if l.t_value > 0.0 { 1 } else if l.t_value < 0.0 { -1 } else { 0 };
            prop_assert_eq!(l.label, sign);
        }
    }

    #[test]
    fn no_trend_scanning_training_label_read_a_bar_of_its_test_fold(
        series in prices(),
        min_horizon in 3usize..6,
        extra in 0usize..10,
        k in 2usize..5,
        embargo in 0usize..3,
    ) {
        let max_horizon = min_horizon + extra;
        let events: Vec<usize> = (0..series.len()).collect();
        let labels = trend_scanning_labels(&series, &events, min_horizon, max_horizon);
        let spans: Vec<(usize, usize)> = labels.iter().map(|l| (l.start, l.end)).collect();
        for (train, test) in purged_kfold_label_spans(&spans, k, embargo) {
            if test.is_empty() {
                continue;
            }
            // The bars the test labels read; a training label read every window up to the longest.
            let window_start = test.iter().map(|&i| spans[i].0).min().unwrap();
            let window_end = test.iter().map(|&i| spans[i].1).max().unwrap();
            for &i in &train {
                let (start, read_to) = (labels[i].start, labels[i].start + max_horizon - 1);
                // Sharing one boundary price is no shared return (snippet 7.3 keeps it).
                prop_assert!(read_to <= window_start || start >= window_end, "{i} read the test fold");
            }
        }
    }

    #[test]
    fn rank_ic_is_bounded_and_blind_to_a_monotone_rescaling(
        panel in prop::collection::vec(prop::collection::vec((-1000i32..1000, -1.0f64..1.0), 0..12), 1..8),
    ) {
        let factor: Vec<Vec<f64>> =
            panel.iter().map(|row| row.iter().map(|&(f, _)| f64::from(f)).collect()).collect();
        let returns: Vec<Vec<f64>> =
            panel.iter().map(|row| row.iter().map(|&(_, r)| r).collect()).collect();
        // 3x + 7 is exact on these integers and strictly increasing, so the ranks do not move.
        let rescaled: Vec<Vec<f64>> =
            factor.iter().map(|row| row.iter().map(|f| 3.0 * f + 7.0).collect()).collect();
        let ic = rank_information_coefficient(&factor, &returns);
        let ic_rescaled = rank_information_coefficient(&rescaled, &returns);
        for (a, b) in ic.iter().zip(&ic_rescaled) {
            prop_assert!(a.to_bits() == b.to_bits(), "{a} vs {b}");
            prop_assert!(a.is_nan() || (-1.0 - 1e-12..=1.0 + 1e-12).contains(a));
        }
    }

    #[test]
    fn a_factor_equal_to_its_return_ranks_perfectly_and_sorts_its_quantiles(
        rows in prop::collection::vec(prop::collection::vec(-1.0f64..1.0, 2..20), 1..6),
        quantiles in 1usize..6,
    ) {
        let ic = rank_information_coefficient(&rows, &rows);
        for value in ic {
            prop_assert!(value.is_nan() || (value - 1.0).abs() < 1e-12, "{value}");
        }
        for row in quantile_returns(&rows, &rows, quantiles) {
            let filled: Vec<f64> = row.into_iter().filter(|v| !v.is_nan()).collect();
            prop_assert!(filled.windows(2).all(|pair| pair[0] <= pair[1]), "{filled:?}");
        }
    }
}
