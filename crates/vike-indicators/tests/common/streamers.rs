//! Helpers the `streamers_*_exact` suites share: the two deterministic generators, the awkward
//! series each builds and the stream-equals-batch assertions. Each suite carried its own copy; the
//! copies differed only in a seed, a stretch or a threshold, which are parameters here, so every
//! suite still folds over exactly the series it did.
//!
//! The `streamers` binary (`tests/streamers.rs`) declares it ONCE,
//! `#[path = "common/streamers.rs"] mod streamers;`, and each suite reads it as `crate::streamers`;
//! the `dead_code` allowance stays for a helper a suite edit leaves unread.
#![allow(dead_code)]

use vike_indicators::{compute, make_with};
use vike_marketdata::Bar;

pub const BARS: usize = 1_500;

// ------------------------------------------------------------------ the xorshift64* half
// (`streamers_{ema,compound,directional,multi,wilder}_exact`)

/// A small deterministic generator (xorshift64*): no dependency, the same series on every box.
pub struct Rng(pub u64);
impl Rng {
    pub fn next_unit(&mut self) -> f64 {
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        let x = self.0.wrapping_mul(0x2545_F491_4F6C_DD1D);
        (x >> 11) as f64 / (1u64 << 53) as f64
    }
}

/// [`BARS`] one-minute bars seeded with `seed`, built to hurt a chained or windowed kernel: the
/// opening `flat_head` bars and a stretch at 300..340 do not move at all (zero changes, zero ranges),
/// `point_bar` (if any) moves the price but has no range, a zero-volume stretch, a large gap up and a
/// larger gap down, and one bar that closes at exactly zero.
pub fn rng_series(seed: u64, flat_head: usize, point_bar: Option<usize>) -> Vec<Bar> {
    let mut rng = Rng(seed);
    let mut bars = Vec::with_capacity(BARS);
    let mut price = 100.0_f64;
    for i in 0..BARS {
        let ts = 1_700_000_000_000 + i as i64 * 60_000;
        // A large gap up and a larger gap down.
        if i == 900 {
            price *= 2.5;
        }
        if i == 1_250 {
            price *= 0.3;
        }
        let flat = i < flat_head || (300..340).contains(&i);
        let no_range = point_bar == Some(i);
        let zero_close = i == 1_100;
        if !flat {
            let drift = (i as f64 * 0.05).sin() * 0.6;
            price = (price + drift + (rng.next_unit() - 0.5) * 2.0).max(1.0);
        }
        let (open, high, low, close) = if flat || no_range {
            (price, price, price, price)
        } else {
            let open = price + (rng.next_unit() - 0.5) * 0.8;
            let spread = rng.next_unit() * 1.5;
            let close = if zero_close { 0.0 } else { price };
            (open, open.max(close) + spread, open.min(close) - spread * 0.5, close)
        };
        let volume = if (600..650).contains(&i) { 0.0 } else { 500.0 + rng.next_unit() * 1_000.0 };
        bars.push(Bar::ohlcv(ts, open, high, low, close, volume));
    }
    bars
}

/// Fold `on_bar` over `bars` and compare each output line of each bar with the batch columns.
///
/// `skip_line` names a line that is not compared (`ichimoku`'s `chikou`) and must be `NaN` instead.
/// `dead` is the one case where the batch itself never leaves its warm-up (an `autocorr` whose
/// `period` is smaller than `lag + 2`): then every line must be `NaN` in the batch, which keeps the
/// "comes alive" check below from being skipped silently.
pub fn assert_stream_equals_batch(
    name: &str,
    raw: &[f64],
    bars: &[Bar],
    skip_line: Option<usize>,
    dead: bool,
) {
    let batch = compute(name, raw, bars).unwrap_or_else(|| panic!("{name} is not a registry row"));
    let mut ind = make_with(name, raw).unwrap_or_else(|| panic!("{name} is not a registry row"));
    // Folded twice: the second pass starts from `reset()`, which must rebuild all state.
    for pass in 0..2 {
        if pass == 1 {
            ind.reset();
        }
        for (i, bar) in bars.iter().enumerate() {
            let got = ind.on_bar(bar);
            assert_eq!(
                got.len(),
                batch.len(),
                "{name} {raw:?}: pass {pass}, bar {i}: {} output lines streamed, {} in the batch",
                got.len(),
                batch.len(),
            );
            for (line, (line_name, column)) in batch.iter().enumerate() {
                let (want, have) = (column[i], got[line]);
                if skip_line == Some(line) {
                    assert!(
                        have.is_nan(),
                        "{name} {raw:?}: pass {pass}, bar {i}, line {line} ({line_name}): the \
                         best-effort tip value should be NaN but is {have:?}",
                    );
                    continue;
                }
                let same = (want.is_nan() && have.is_nan()) || want.to_bits() == have.to_bits();
                assert!(
                    same,
                    "{name} {raw:?}: pass {pass}, bar {i}, line {line} ({line_name}): \
                     streamed {have:?} ({:#018x}) but the batch holds {want:?} ({:#018x})",
                    have.to_bits(),
                    want.to_bits(),
                );
            }
            assert_eq!(
                ind.value().iter().map(|v| v.to_bits()).collect::<Vec<_>>(),
                got.iter().map(|v| v.to_bits()).collect::<Vec<_>>(),
                "{name} {raw:?}: pass {pass}, bar {i}: value() is not the last on_bar output",
            );
        }
    }
    // A comparison of two all-NaN columns is exact and proves nothing: every line must come alive.
    for (line_name, column) in &batch {
        if dead {
            assert!(
                column.iter().all(|v| v.is_nan()),
                "{name} {raw:?}: line {line_name} is meant to be all NaN but holds a number",
            );
        } else {
            assert!(
                column.iter().any(|v| v.is_finite()),
                "{name} {raw:?}: line {line_name} never leaves its warm-up on {} bars",
                bars.len(),
            );
        }
    }
}

/// [`assert_stream_equals_batch`] (every line compared, every line alive) for each parameter set.
pub fn check_all(bars: &[Bar], name: &str, param_sets: &[&[f64]]) {
    for raw in param_sets {
        assert_stream_equals_batch(name, raw, bars, None, false);
    }
}

// ------------------------------------------------------------------ the PCG half
// (`streamers_{bands,stats,window}_exact`)

/// [`BARS`] one-minute bars of a random walk with the awkward stretches the kernels branch on. With
/// `runs`, also 40 bars compounding at a fixed 1% a bar (log returns that repeat) and a run of
/// strictly rising closes (a window a line fits almost perfectly, an up-streak that climbs past the
/// periods).
pub fn lcg_series(runs: bool) -> Vec<Bar> {
    // The crate's one fixture LCG (PCG-style multiplier), so the series is the same on every box.
    let mut rng = vike_indicators::test_support::Lcg(0x2545_F491_4F6C_DD1D);
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
            1200..=1239 if runs => {
                let open = px;
                px *= 1.01;
                Bar::ohlcv(ts, open, px, open, px, 1_000.0)
            }
            // A run of strictly rising closes.
            500..=529 if runs => {
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

pub fn same_bits(a: f64, b: f64) -> bool {
    a.to_bits() == b.to_bits() || (a.is_nan() && b.is_nan())
}

/// [`assert_exact`] for each of `sets` over [`lcg_series`]`(runs)`, with that series' `share`.
pub fn exact_all(runs: bool, name: &str, sets: &[&[f64]], share: usize) {
    let bars = lcg_series(runs);
    for raw in sets {
        assert_exact(name, raw, &bars, share);
    }
}

/// Fold `name` at `raw` over `bars` one bar at a time and require every value on every line to be
/// the batch kernel's, bit for bit, and every line to be finite on more than `bars.len() / share`
/// bars (the proof must not be vacuous).
pub fn assert_exact(name: &str, raw: &[f64], bars: &[Bar], share: usize) {
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
    for (line_name, series) in &batch {
        let finite = series.iter().filter(|v| v.is_finite()).count();
        assert!(
            finite > bars.len() / share,
            "{name} params {raw:?}: line {line_name} is finite on only {finite} of {} bars",
            bars.len()
        );
    }
}
