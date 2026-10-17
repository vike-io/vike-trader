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
            // One bar that closes at zero: no log return, a zero denominator for the next return.
            1300 => Bar::ohlcv(ts, px, px, 0.0, 0.0, 900.0),
            // 40 bars compounding at a fixed 1% a bar: log returns that repeat.
            1200..=1239 => {
                let open = px;
                px *= 1.01;
                Bar::ohlcv(ts, open, px, open, px, 1_000.0)
            }
            // A run of strictly rising closes, so the up-streak counter climbs past the periods.
            500..=529 => {
                let open = px;
                px += 0.25 + 0.1 * rng.unit();
                Bar::ohlcv(ts, open, px + 0.05, open - 0.05, px, 1_000.0)
            }
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
    // The proof must not be vacuous: every line is defined on most of the series.
    for (line_name, series) in &batch {
        let finite = series.iter().filter(|v| v.is_finite()).count();
        assert!(
            finite > bars.len() / 2,
            "{name} params {raw:?}: line {line_name} is finite on only {finite} of {} bars",
            bars.len()
        );
    }
}

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
    let bars = series();
    // period 2..=200, ann 1..=365.
    let sets: &[&[f64]] =
        &[&[], &[2.0, 1.0], &[5.0, 52.0], &[20.0, 252.0], &[60.0, 365.0], &[200.0, 365.0]];
    for raw in sets {
        assert_exact("hvol", raw, &bars);
    }
}

#[test]
fn relative_volatility_streams_bit_equal_to_vectorize() {
    let bars = series();
    // period 2..=200.
    let sets: &[&[f64]] = &[&[], &[2.0], &[3.0], &[7.0], &[30.0], &[200.0]];
    for raw in sets {
        assert_exact("relative_volatility", raw, &bars);
    }
}

#[test]
fn chande_kroll_stop_streams_bit_equal_to_vectorize() {
    let bars = series();
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
    for raw in sets {
        assert_exact("chande_kroll_stop", raw, &bars);
    }
}

#[test]
fn connors_rsi_streams_bit_equal_to_vectorize() {
    let bars = series();
    // rsi_p 2..=100, streak_p 2..=100, rank_p 10..=500.
    let sets: &[&[f64]] = &[
        &[],
        &[2.0, 2.0, 10.0],
        &[5.0, 3.0, 50.0],
        &[14.0, 7.0, 250.0],
        &[100.0, 100.0, 500.0],
        &[3.0, 2.0, 500.0],
    ];
    for raw in sets {
        assert_exact("connors_rsi", raw, &bars);
    }
}

#[test]
fn reset_restarts_the_fold_for_all_four() {
    let bars = series();
    for name in ["hvol", "relative_volatility", "chande_kroll_stop", "connors_rsi"] {
        assert_reset_restarts(name, &[], &bars);
    }
}
