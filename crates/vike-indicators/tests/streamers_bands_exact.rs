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

const BARS: usize = 1_500;

/// A tiny deterministic generator (PCG-style multiplier), so the series is the same on every box.
struct Lcg(u64);
impl Lcg {
    /// A value in `[0, 1)`.
    fn unit(&mut self) -> f64 {
        self.0 =
            self.0.wrapping_mul(6_364_136_223_846_793_005).wrapping_add(1_442_695_040_888_963_407);
        (self.0 >> 11) as f64 / (1u64 << 53) as f64
    }
    /// A value in `[-1, 1)`.
    fn signed(&mut self) -> f64 {
        2.0 * self.unit() - 1.0
    }
}

/// 1,500 one-minute bars of a random walk with the awkward stretches the kernels branch on.
fn series() -> Vec<Bar> {
    let mut rng = Lcg(0x2545_F491_4F6C_DD1D);
    let mut px = 100.3_f64;
    let mut bars = Vec::with_capacity(BARS);
    for i in 0..BARS {
        let ts = 1_700_000_000_000 + i as i64 * 60_000;
        let bar = match i {
            // 40 bars with no movement at all: open == high == low == close, a constant window.
            300..=339 => Bar::ohlcv(ts, px, px, px, px, 400.0),
            // One bar with a zero range, mid-series, with volume.
            1100 => Bar::ohlcv(ts, px, px, px, px, 750.0),
            // One bar that closes at zero.
            1300 => Bar::ohlcv(ts, px, px, 0.0, 0.0, 900.0),
            _ => {
                // A large gap up at 900 and down at 1400: the open jumps away from the last close.
                if i == 900 {
                    px *= 1.35;
                }
                if i == 1400 {
                    px *= 0.6;
                }
                let open = px;
                let close = px * (1.0 + 0.01 * rng.signed());
                let high = open.max(close) + px * 0.005 * rng.unit();
                let low = open.min(close) - px * 0.005 * rng.unit();
                // 40 bars with no volume.
                let volume =
                    if (600..640).contains(&i) { 0.0 } else { 1_000.0 + 500.0 * rng.unit() };
                px = close;
                Bar::ohlcv(ts, open, high, low, close, volume)
            }
        };
        bars.push(bar);
    }
    bars
}

fn same_bits(a: f64, b: f64) -> bool {
    a.to_bits() == b.to_bits() || (a.is_nan() && b.is_nan())
}

/// Fold `name` at `raw` over `bars` one bar at a time and require every value on every line to be
/// the batch kernel's, bit for bit.
fn assert_exact(name: &str, raw: &[f64], bars: &[Bar]) {
    let batch = compute(name, raw, bars).expect("the indicator is registered");
    let mut streamer = make_with(name, raw).expect("the indicator is registered");
    for (i, bar) in bars.iter().enumerate() {
        let row = streamer.on_bar(bar);
        assert_eq!(
            row.len(),
            batch.len(),
            "{name} params {raw:?}: bar {i}: on_bar returned {} lines, the kernel has {}",
            row.len(),
            batch.len()
        );
        for (line, (line_name, series)) in batch.iter().enumerate() {
            let (streamed, expected) = (row[line], series[i]);
            assert!(
                same_bits(streamed, expected),
                "{name} params {raw:?}: bar {i}, line {line} ({line_name}): on_bar gave \
                 {streamed:?} ({:#018x}) but the batch kernel has {expected:?} ({:#018x})",
                streamed.to_bits(),
                expected.to_bits()
            );
        }
    }
    // The proof must not be vacuous: every line is defined on a good part of the series.
    for (line_name, series) in &batch {
        let finite = series.iter().filter(|v| v.is_finite()).count();
        assert!(
            finite > bars.len() / 3,
            "{name} params {raw:?}: line {line_name} is finite on only {finite} of {} bars",
            bars.len()
        );
    }
}

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
    let bars = series();
    for raw in TTM_SETS {
        assert_exact("ttm_squeeze", raw, &bars);
    }
}

#[test]
fn acceleration_bands_streams_bit_equal_to_vectorize() {
    let bars = series();
    for raw in ACCEL_SETS {
        assert_exact("acceleration_bands", raw, &bars);
    }
}

#[test]
fn rwi_streams_bit_equal_to_vectorize() {
    let bars = series();
    for raw in RWI_SETS {
        assert_exact("rwi", raw, &bars);
    }
}

#[test]
fn qstick_streams_bit_equal_to_vectorize() {
    let bars = series();
    for raw in QSTICK_SETS {
        assert_exact("qstick", raw, &bars);
    }
}

#[test]
fn reset_restarts_the_fold_and_value_is_the_last_output() {
    let bars = series();
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
