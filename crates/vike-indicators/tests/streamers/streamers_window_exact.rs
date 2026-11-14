//! Exactness proof for the four bounded-state window streamers: `hvol`, `relative_volatility`,
//! `chande_kroll_stop` and `connors_rsi`.
//!
//! Each is folded bar by bar through `make_with(name, raw).on_bar(..)` and compared, at EVERY bar
//! and on EVERY output line, against `compute(name, raw, &bars)` (the batch kernel) by bit pattern:
//! `to_bits()` equality, with two NaNs counting as equal. This uses the public API only and is
//! independent of `tests/parity`: it holds each type to its kernel on a series built to hit the
//! kernels' degenerate branches (a flat stretch, a zero-volume stretch, a compounding stretch whose
//! log returns repeat, a large gap each way, a bar with `high == low`, a bar closing at zero),
//! under several parameter sets each, small and large.

use vike_indicators::make_with;
use vike_marketdata::Bar;

use crate::streamers::{exact_all, lcg_series, same_bits};

/// After `reset` the same bars must give the same bits again.
fn assert_reset_restarts(name: &str, raw: &[f64], bars: &[Bar]) {
    let mut streamer = make_with(name, raw).expect("the indicator is registered");
    let first: Vec<Vec<f64>> = bars.iter().map(|b| streamer.on_bar(b)).collect();
    streamer.reset();
    for (i, bar) in bars.iter().enumerate() {
        let row = streamer.on_bar(bar);
        for (line, (a, b)) in row.iter().zip(&first[i]).enumerate() {
            assert!(
                same_bits(*a, *b),
                "{name} params {raw:?}: after reset, bar {i}, line {line}: {a:?} != first pass {b:?}"
            );
        }
    }
}

#[test]
fn hvol_streams_bit_equal_to_vectorize() {
    // period 2..=200, ann 1..=365.
    let sets: &[&[f64]] =
        &[&[], &[2.0, 1.0], &[5.0, 52.0], &[20.0, 252.0], &[60.0, 365.0], &[200.0, 365.0]];
    exact_all(true, "hvol", sets, 2);
}

#[test]
fn relative_volatility_streams_bit_equal_to_vectorize() {
    // period 2..=200.
    exact_all(true, "relative_volatility", &[&[], &[2.0], &[3.0], &[7.0], &[30.0], &[200.0]], 2);
}

#[test]
fn chande_kroll_stop_streams_bit_equal_to_vectorize() {
    // p 2..=200, x 1..=10, q 2..=200.
    let sets: &[&[f64]] = &[
        &[],
        &[2.0, 1.0, 2.0],
        &[5.0, 3.0, 4.0],
        &[10.0, 1.0, 20.0],
        &[50.0, 2.0, 30.0],
        &[20.0, 10.0, 200.0],
        &[200.0, 10.0, 200.0],
    ];
    exact_all(true, "chande_kroll_stop", sets, 2);
}

#[test]
fn connors_rsi_streams_bit_equal_to_vectorize() {
    // rsi_p 2..=100, streak_p 2..=100, rank_p 10..=500.
    let sets: &[&[f64]] = &[
        &[],
        &[2.0, 2.0, 10.0],
        &[5.0, 3.0, 50.0],
        &[14.0, 7.0, 250.0],
        &[100.0, 100.0, 500.0],
        &[3.0, 2.0, 500.0],
    ];
    exact_all(true, "connors_rsi", sets, 2);
}

#[test]
fn reset_restarts_the_fold_for_all_four() {
    let bars = lcg_series(true);
    for name in ["hvol", "relative_volatility", "chande_kroll_stop", "connors_rsi"] {
        assert_reset_restarts(name, &[], &bars);
    }
}
