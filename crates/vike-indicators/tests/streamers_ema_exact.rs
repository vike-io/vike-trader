//! `t3`, `tema` and `kst` stream from bounded state (a chain of EMA/SMA stages), not from a
//! history recompute. This is the exactness proof for that, independent of `tests/parity`: fold
//! `on_bar` over a long, awkward series and require EVERY bar and EVERY output line to be bit-equal
//! to what the registry's batch path (`compute`, i.e. `vectorize`) holds at that index, `NaN`s at
//! the same warm-up positions included.
//!
//! The series is deterministic and built to hurt a chained smoother: a long flat stretch (zero
//! changes, zero ranges), a zero-volume stretch, a large price gap in each direction, and one bar
//! that closes at exactly zero (the rate-of-change `NaN` branch of `kst`, whose SMAs then run over
//! a COMPACTED series). Each indicator runs at its defaults, at a small-period set and at larger
//! ones, every value inside the registry's range.

use vike_indicators::{compute, make_with};
use vike_marketdata::Bar;

const BARS: usize = 1_500;

/// A small deterministic generator (xorshift64*): no dependency, the same series on every box.
struct Rng(u64);
impl Rng {
    fn next_unit(&mut self) -> f64 {
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        let x = self.0.wrapping_mul(0x2545_F491_4F6C_DD1D);
        (x >> 11) as f64 / (1u64 << 53) as f64
    }
}

fn series() -> Vec<Bar> {
    let mut rng = Rng(0x9E37_79B9_7F4A_7C15);
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
        let flat = (300..340).contains(&i);
        let zero_close = i == 1_100;
        if !flat {
            let drift = (i as f64 * 0.05).sin() * 0.6;
            price = (price + drift + (rng.next_unit() - 0.5) * 2.0).max(1.0);
        }
        let (open, high, low, close) = if flat {
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
fn assert_stream_equals_batch(name: &str, raw: &[f64], bars: &[Bar]) {
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
        assert!(
            column.iter().any(|v| v.is_finite()),
            "{name} {raw:?}: line {line_name} never leaves its warm-up on {} bars",
            bars.len(),
        );
    }
}

fn check(name: &str, param_sets: &[&[f64]]) {
    let bars = series();
    for raw in param_sets {
        assert_stream_equals_batch(name, raw, &bars);
    }
}

#[test]
fn tema_streams_bit_equal_to_the_batch() {
    check(
        "tema",
        &[
            &[],      // the defaults
            &[2.0],   // the smallest period
            &[3.0],   // small
            &[37.0],  // medium
            &[250.0], // large: warm-up near bar 750
        ],
    );
}

#[test]
fn t3_streams_bit_equal_to_the_batch() {
    check(
        "t3",
        &[
            &[],           // the defaults
            &[2.0, 0.0],   // smallest period, v at its floor
            &[5.0, 0.3],   // small
            &[9.0, 1.0],   // v at its ceiling
            &[60.0, 0.85], // larger
            &[200.0, 0.5], // large: warm-up near bar 1200
        ],
    );
}

#[test]
fn kst_streams_bit_equal_to_the_batch() {
    check(
        "kst",
        &[
            &[],                                                       // the defaults
            &[1.0, 2.0, 2.0, 2.0, 3.0, 2.0, 4.0, 3.0, 2.0],            // small periods
            &[5.0, 4.0, 8.0, 6.0, 12.0, 8.0, 20.0, 10.0, 5.0],         // medium
            &[50.0, 40.0, 70.0, 50.0, 100.0, 60.0, 150.0, 80.0, 40.0], // large
            &[40.0, 2.0, 3.0, 30.0, 25.0, 2.0, 1.0, 50.0, 17.0], // each component dominates in turn
        ],
    );
}
