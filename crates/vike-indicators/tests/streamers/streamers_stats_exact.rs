//! Exactness proof for the four bounded-state rolling-window streamers: `hurst`, `linreg_channel`,
//! `r_squared` and `median_ma`.
//!
//! Each is folded bar by bar through `make_with(name, raw).on_bar(..)` and compared, at EVERY bar
//! and on EVERY output line, against `compute(name, raw, &bars)` (the batch kernel) by bit pattern:
//! `to_bits()` equality, with two NaNs counting as equal. This uses the public API only and is
//! independent of `tests/parity`: it holds each type to its kernel on a series built to hit the
//! kernels' degenerate branches (a flat stretch, a zero-volume stretch, a compounding stretch whose
//! log returns repeat, a large gap each way, a bar with `high == low`, a bar closing at zero),
//! under several parameter sets each, small and large, odd and even.

use vike_indicators::make_with;
use vike_marketdata::Bar;

use crate::streamers::{exact_all, lcg_series, same_bits};

/// Fold twice with a `reset` between: the second pass must give the first pass's bits at every bar
/// and line, `value()` must equal the last output after each pass, and a freshly reset streamer
/// must read all `NaN`.
fn assert_reset_and_value(name: &str, raw: &[f64], bars: &[Bar]) {
    let mut streamer = make_with(name, raw).expect("the indicator is registered");
    let first: Vec<Vec<f64>> = bars.iter().map(|b| streamer.on_bar(b)).collect();
    let last_first = first.last().expect("bars is not empty");
    let value = streamer.value();
    assert_eq!(value.len(), last_first.len(), "{name} params {raw:?}: value() has a wrong arity");
    for (line, (a, b)) in value.iter().zip(last_first).enumerate() {
        assert!(
            same_bits(*a, *b),
            "{name} params {raw:?}: after the first pass value() line {line} is {a:?}, the last \
             output was {b:?}"
        );
    }
    streamer.reset();
    for (line, v) in streamer.value().iter().enumerate() {
        assert!(v.is_nan(), "{name} params {raw:?}: after reset value() line {line} is {v:?}");
    }
    for (i, bar) in bars.iter().enumerate() {
        let row = streamer.on_bar(bar);
        for (line, (a, b)) in row.iter().zip(&first[i]).enumerate() {
            assert!(
                same_bits(*a, *b),
                "{name} params {raw:?}: after reset, bar {i}, line {line}: {a:?} != first pass {b:?}"
            );
        }
    }
    let value = streamer.value();
    for (line, (a, b)) in value.iter().zip(last_first).enumerate() {
        assert!(
            same_bits(*a, *b),
            "{name} params {raw:?}: after the second pass value() line {line} is {a:?}, the last \
             output was {b:?}"
        );
    }
}

#[test]
fn hurst_streams_bit_equal_to_vectorize() {
    // period 17..=400: the registry minimum, a block-size boundary either side of 33, a power of
    // two, and the maximum.
    exact_all(true, "hurst", &[&[], &[17.0], &[18.0], &[33.0], &[64.0], &[250.0], &[400.0]], 4);
}

#[test]
fn linreg_channel_streams_bit_equal_to_vectorize() {
    // period 2..=400, mult 0.5..=10.
    let sets: &[&[f64]] =
        &[&[], &[2.0, 0.5], &[3.0, 1.0], &[20.0, 2.0], &[50.0, 5.0], &[150.0, 3.5], &[400.0, 10.0]];
    exact_all(true, "linreg_channel", sets, 4);
}

#[test]
fn r_squared_streams_bit_equal_to_vectorize() {
    // period 2..=200.
    exact_all(true, "r_squared", &[&[], &[2.0], &[3.0], &[14.0], &[50.0], &[200.0]], 4);
}

#[test]
fn median_ma_streams_bit_equal_to_vectorize() {
    // period 2..=400, odd and even.
    let sets: &[&[f64]] = &[&[], &[2.0], &[3.0], &[7.0], &[8.0], &[60.0], &[61.0], &[400.0]];
    exact_all(true, "median_ma", sets, 4);
}

#[test]
fn reset_restarts_the_fold_and_value_is_the_last_output() {
    let bars = lcg_series(true);
    let hurst: &[&[f64]] = &[&[], &[17.0], &[64.0]];
    let linreg_channel: &[&[f64]] = &[&[], &[2.0, 0.5], &[50.0, 5.0]];
    let r_squared: &[&[f64]] = &[&[], &[2.0], &[50.0]];
    let median_ma: &[&[f64]] = &[&[], &[3.0], &[8.0], &[60.0]];
    for (name, sets) in [
        ("hurst", hurst),
        ("linreg_channel", linreg_channel),
        ("r_squared", r_squared),
        ("median_ma", median_ma),
    ] {
        for raw in sets {
            assert_reset_and_value(name, raw, &bars);
        }
    }
}
