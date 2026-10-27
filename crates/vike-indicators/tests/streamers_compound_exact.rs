//! `trix`, `dema`, `ppo`, `apo`, `zlema`, `coppock`, `tsi`, `smi_ergodic`, `kvo` and `mass` stream from
//! bounded state (a chain of EMA stages and small rings), not from a history recompute. This is the
//! exactness proof for that, independent of `tests/parity`: fold `on_bar` over a long, awkward series
//! and require EVERY bar and EVERY output line to be bit-equal to what the registry's batch path
//! (`compute`, i.e. `vectorize`) holds at that index, `NaN`s at the same warm-up positions included.
//!
//! The series is deterministic and built to hurt a chained smoother: a flat opening (so the first
//! smoothed absolute changes and ranges are exactly zero), a 40-bar flat stretch (zero changes, zero
//! ranges), a zero-volume stretch, a large price gap in each direction, a bar with `high == low` that
//! still moves the price, and one bar that closes at exactly zero (the rate-of-change `NaN` branch of
//! `coppock`, whose weighted average then runs over a COMPACTED series). Each indicator runs at its
//! defaults, at the smallest periods, at small, medium and large ones, every value inside the
//! registry's range.

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
        // The opening 25 bars and a stretch at 300..340 do not move at all.
        let flat = i < 25 || (300..340).contains(&i);
        // One bar that moves the price but has no range.
        let no_range = i == 1_000;
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
fn trix_streams_bit_equal_to_the_batch() {
    check(
        "trix",
        &[
            &[],      // the defaults
            &[2.0],   // the smallest period
            &[5.0],   // small
            &[60.0],  // medium
            &[200.0], // the largest period: warm-up near bar 600
        ],
    );
}

#[test]
fn dema_streams_bit_equal_to_the_batch() {
    check(
        "dema",
        &[
            &[],      // the defaults
            &[2.0],   // the smallest period
            &[5.0],   // small
            &[40.0],  // medium
            &[400.0], // the largest period: warm-up near bar 800
        ],
    );
}

#[test]
fn ppo_streams_bit_equal_to_the_batch() {
    check(
        "ppo",
        &[
            &[],             // the defaults
            &[2.0, 2.0],     // both at the smallest period: the lines are equal
            &[2.0, 3.0],     // small
            &[9.0, 4.0],     // fast above slow
            &[50.0, 120.0],  // medium
            &[200.0, 400.0], // the largest periods
        ],
    );
}

#[test]
fn apo_streams_bit_equal_to_the_batch() {
    check(
        "apo",
        &[
            &[],             // the defaults
            &[2.0, 2.0],     // both at the smallest period: the lines are equal
            &[2.0, 3.0],     // small
            &[9.0, 4.0],     // fast above slow
            &[50.0, 120.0],  // medium
            &[200.0, 400.0], // the largest periods
        ],
    );
}

#[test]
fn zlema_streams_bit_equal_to_the_batch() {
    check(
        "zlema",
        &[
            &[],      // the defaults
            &[2.0],   // the smallest period: no de-lagging at all (lag 0)
            &[3.0],   // lag 1
            &[8.0],   // lag 3
            &[61.0],  // medium, odd
            &[400.0], // the largest period
        ],
    );
}

#[test]
fn coppock_streams_bit_equal_to_the_batch() {
    check(
        "coppock",
        &[
            &[],                    // the defaults
            &[2.0, 2.0, 2.0],       // the smallest periods
            &[3.0, 5.0, 2.0],       // small
            &[7.0, 3.0, 9.0],       // the "short" rate longer than the "long" one
            &[30.0, 40.0, 25.0],    // medium
            &[100.0, 200.0, 150.0], // the largest periods
        ],
    );
}

#[test]
fn tsi_streams_bit_equal_to_the_batch() {
    check(
        "tsi",
        &[
            &[],             // the defaults
            &[2.0, 2.0],     // the smallest periods
            &[5.0, 3.0],     // small
            &[40.0, 20.0],   // medium
            &[100.0, 50.0],  // larger
            &[400.0, 200.0], // the largest periods: warm-up near bar 600
        ],
    );
}

#[test]
fn smi_ergodic_streams_bit_equal_to_the_batch() {
    check(
        "smi_ergodic",
        &[
            &[],                    // the defaults
            &[2.0, 2.0, 2.0],       // the smallest periods
            &[5.0, 3.0, 4.0],       // small
            &[40.0, 20.0, 10.0],    // medium
            &[100.0, 50.0, 25.0],   // larger
            &[300.0, 150.0, 100.0], // large: the signal first lands near bar 550
        ],
    );
}

#[test]
fn kvo_streams_bit_equal_to_the_batch() {
    check(
        "kvo",
        &[
            &[],                    // the defaults
            &[2.0, 2.0, 2.0],       // the smallest periods
            &[5.0, 8.0, 3.0],       // small
            &[10.0, 4.0, 5.0],      // fast above slow
            &[20.0, 40.0, 10.0],    // medium
            &[200.0, 500.0, 100.0], // the largest periods: the signal first lands near bar 600
        ],
    );
}

#[test]
fn mass_streams_bit_equal_to_the_batch() {
    check(
        "mass",
        &[
            &[],            // the defaults
            &[2.0, 2.0],    // the smallest periods
            &[5.0, 3.0],    // small
            &[50.0, 20.0],  // medium
            &[200.0, 50.0], // the largest window, a long smoothing
        ],
    );
}
