//! Exactness proof for the four bounded-state band streamers: `ttm_squeeze`, `acceleration_bands`,
//! `rwi` and `qstick`.
//!
//! Each is folded bar by bar through `make_with(name, raw).on_bar(..)` and compared, at EVERY bar
//! and on EVERY output line, against `compute(name, raw, &bars)` (the batch kernel) by bit pattern:
//! `to_bits()` equality, with two NaNs counting as equal. This uses the public API only and is
//! independent of `tests/parity`: it holds each type to its kernel on a series built to hit the
//! kernels' degenerate branches (a flat stretch, a zero-volume stretch, a large gap each way, a bar
//! with `high == low`, a bar closing at zero), under several parameter sets each, small and large.

use vike_indicators::{compute, make_with};
use vike_marketdata::Bar;

use crate::streamers::{exact_all, lcg_series, same_bits};

/// Fold twice with `reset` between: both passes must give the batch kernel's bits, and `value()`
/// must be the last output after each pass.
fn assert_reset_restarts(name: &str, raw: &[f64], bars: &[Bar]) {
    let batch = compute(name, raw, bars).expect("the indicator is registered");
    let mut streamer = make_with(name, raw).expect("the indicator is registered");
    for pass in 0..2 {
        let mut last = Vec::new();
        for (i, bar) in bars.iter().enumerate() {
            let row = streamer.on_bar(bar);
            for (line, (line_name, series)) in batch.iter().enumerate() {
                assert!(
                    same_bits(row[line], series[i]),
                    "{name} params {raw:?}: pass {pass}, bar {i}, line {line} ({line_name}): \
                     {:?} != batch {:?}",
                    row[line],
                    series[i]
                );
            }
            last = row;
        }
        let value = streamer.value();
        assert_eq!(value.len(), last.len(), "{name} params {raw:?}: pass {pass}: value() width");
        for (line, (a, b)) in value.iter().zip(&last).enumerate() {
            assert!(
                same_bits(*a, *b),
                "{name} params {raw:?}: pass {pass}, line {line}: value() {a:?} != last output {b:?}"
            );
        }
        streamer.reset();
    }
}

/// `value()` before any bar is all-NaN, the same width as the output.
fn assert_fresh_value_is_nan(name: &str, raw: &[f64]) {
    let streamer = make_with(name, raw).expect("the indicator is registered");
    let fresh = streamer.value();
    assert!(
        !fresh.is_empty() && fresh.iter().all(|v| v.is_nan()),
        "{name}: fresh value() {fresh:?}"
    );
}

// length 2..=200, bb_mult 0.5..=5, kc_mult 0.5..=5.
const TTM_SETS: &[&[f64]] = &[
    &[],
    &[2.0, 0.5, 0.5],
    &[5.0, 1.0, 2.0],
    &[10.0, 3.0, 4.5],
    &[20.0, 2.0, 1.5],
    &[50.0, 0.5, 5.0],
    &[200.0, 2.0, 1.5],
];
// period 2..=400, factor 0.5..=10.
const ACCEL_SETS: &[&[f64]] =
    &[&[], &[2.0, 0.5], &[7.0, 4.0], &[20.0, 10.0], &[50.0, 1.0], &[400.0, 4.0]];
// period 2..=200.
const RWI_SETS: &[&[f64]] = &[&[], &[2.0], &[3.0], &[14.0], &[60.0], &[200.0]];
// period 2..=200.
const QSTICK_SETS: &[&[f64]] = &[&[], &[2.0], &[3.0], &[8.0], &[40.0], &[200.0]];

#[test]
fn ttm_squeeze_streams_bit_equal_to_vectorize() {
    exact_all(false, "ttm_squeeze", TTM_SETS, 3);
}

#[test]
fn acceleration_bands_streams_bit_equal_to_vectorize() {
    exact_all(false, "acceleration_bands", ACCEL_SETS, 3);
}

#[test]
fn rwi_streams_bit_equal_to_vectorize() {
    exact_all(false, "rwi", RWI_SETS, 3);
}

#[test]
fn qstick_streams_bit_equal_to_vectorize() {
    exact_all(false, "qstick", QSTICK_SETS, 3);
}

#[test]
fn reset_restarts_the_fold_and_value_is_the_last_output() {
    let bars = lcg_series(false);
    let all: [(&str, &[&[f64]]); 4] = [
        ("ttm_squeeze", TTM_SETS),
        ("acceleration_bands", ACCEL_SETS),
        ("rwi", RWI_SETS),
        ("qstick", QSTICK_SETS),
    ];
    for (name, sets) in all {
        for raw in sets {
            assert_reset_restarts(name, raw, &bars);
        }
    }
}

#[test]
fn a_fresh_streamer_has_a_nan_value() {
    for name in ["ttm_squeeze", "acceleration_bands", "rwi", "qstick"] {
        assert_fresh_value_is_nan(name, &[]);
    }
}
