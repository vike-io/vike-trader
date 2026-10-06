//! Helpers shared by this crate's integration tests (`parity`, `params`, `lookback`, `pairs_parity`).
//!
//! Each of those files carried its own copy of these functions; the copies were identical apart from
//! comments, so there is one now. NOT shared on purpose: `window_pin.rs` and `libm_platform_probe.rs` each
//! keep their own `synth_bars`, because their generators are DIFFERENT series whose outputs are pinned to
//! committed values — unifying them would move the pins. `params.rs` keeps its own `assert_lines_bit_eq`,
//! which words its failure for a `make_with`-vs-`make` comparison rather than stream-vs-batch.
#![allow(dead_code)] // each test binary compiles this file and uses a subset of it

use vike_indicators::Indicator;
use vike_marketdata::Bar;

/// Bitwise float equality, with both-NaN treated as equal.
pub fn bits_eq(a: f64, b: f64) -> bool {
    if a.is_nan() && b.is_nan() { true } else { a.to_bits() == b.to_bits() }
}

/// Deterministic, varied OHLCV. Hourly bars span multiple UTC days (exercises the
/// VWAP session reset); every 17th bar is flat vs the previous close (exercises the
/// OBV/RSI zero-delta branches); prices oscillate so PSAR flips direction.
pub fn synth_bars(n: usize) -> Vec<Bar> {
    let mut bars = Vec::with_capacity(n);
    let mut prev_close = 100.0f64;
    for i in 0..n {
        let t = i as f64;
        let mut close =
            100.0 + (t * 0.07).sin() * 8.0 + (t * 0.017).cos() * 4.0 + (t * 0.31).sin() * 1.5;
        if i > 0 && i % 17 == 0 {
            close = prev_close; // flat bar
        }
        let open = prev_close;
        let high = open.max(close) + (i % 5) as f64 * 0.3 + 0.5;
        let low = open.min(close) - (i % 7) as f64 * 0.25 - 0.5;
        let volume = 1000.0 + (i % 13) as f64 * 50.0;
        bars.push(Bar {
            ts: i as i64 * 3_600_000, // hourly
            open,
            high,
            low,
            close,
            volume,
            funding: None,
            bid: None,
            ask: None,
            symbol: None,
        });
        prev_close = close;
    }
    bars
}

/// Fold `on_bar` over the series into per-line columns (same shape as `vectorize`).
pub fn fold_stream(ind: &mut dyn Indicator, bars: &[Bar]) -> Vec<Vec<f64>> {
    let mut lines: Vec<Vec<f64>> = Vec::new();
    for bar in bars {
        let out = ind.on_bar(bar);
        if lines.is_empty() {
            lines = vec![Vec::with_capacity(bars.len()); out.len()];
        }
        assert_eq!(out.len(), lines.len(), "on_bar output arity must be stable");
        for (li, v) in out.iter().enumerate() {
            lines[li].push(*v);
        }
    }
    lines
}

pub fn assert_lines_bit_eq(name: &str, stream: &[Vec<f64>], batch: &[Vec<f64>]) {
    assert_eq!(stream.len(), batch.len(), "{name}: line count mismatch");
    for (li, (s, b)) in stream.iter().zip(batch.iter()).enumerate() {
        assert_eq!(s.len(), b.len(), "{name} line {li}: length mismatch");
        for (i, (&sv, &bv)) in s.iter().zip(b.iter()).enumerate() {
            assert!(
                bits_eq(sv, bv),
                "{name} line {li} idx {i}: stream {sv:?} ({:#018x}) != batch {bv:?} ({:#018x})",
                sv.to_bits(),
                bv.to_bits(),
            );
        }
    }
}
