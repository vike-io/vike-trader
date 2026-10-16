//! `gmma`, `alligator`, `relative_vigor`, `high_low_52w`, `stochrsi`, `fisher`, `ichimoku` and
//! `volume_profile_poc` stream from bounded state (EMA/Wilder recurrences and rings of exactly the
//! values a window folds), not from a history recompute. This is the exactness proof for that,
//! independent of `tests/parity`: fold `on_bar` over a long, awkward series and require EVERY bar and
//! EVERY output line to be bit-equal to what the registry's batch path (`compute`, i.e. `vectorize`)
//! holds at that index, `NaN`s at the same warm-up positions included.
//!
//! The series is deterministic and built to hurt a windowed or recursive kernel: a 40-bar flat
//! stretch (zero ranges, zero changes: the zero-range branches of `fisher`, `stochrsi`, `relative_vigor`
//! and the degenerate `volume_profile_poc` window), a zero-volume stretch, a large price gap in each
//! direction, a lone bar with `high == low`, and one bar that closes at exactly zero. Each indicator
//! runs at its defaults, at a small-period set and at larger ones, every value inside the registry's
//! range. The three paramless indicators run their defaults over three differently seeded series.
//!
//! `ichimoku` is the one exception to "every line": its `chikou` line plots the close `kijun` bars
//! BACK, i.e. it reads bars that do not exist yet at the tip, so the registry row is `batch_only`
//! and `on_bar` is documented best-effort for that line (always `NaN` at the tip). Lines 0..=3 are
//! compared exactly; `chikou` is skipped against the batch and only required to be `NaN`.

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

fn series(seed: u64) -> Vec<Bar> {
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
        let flat = (300..340).contains(&i);
        let zero_close = i == 1_100;
        let point_bar = i == 700; // high == low, but the neighbours move
        if !flat {
            let drift = (i as f64 * 0.05).sin() * 0.6;
            price = (price + drift + (rng.next_unit() - 0.5) * 2.0).max(1.0);
        }
        let (open, high, low, close) = if flat || point_bar {
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

const SEED: u64 = 0x9E37_79B9_7F4A_7C15;

/// Fold `on_bar` over `bars` and compare each output line of each bar with the batch columns.
/// `skip_line` names a line that is not compared (`ichimoku`'s `chikou`) and must be `NaN` instead.
fn assert_stream_equals_batch(name: &str, raw: &[f64], bars: &[Bar], skip_line: Option<usize>) {
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
        assert!(
            column.iter().any(|v| v.is_finite()),
            "{name} {raw:?}: line {line_name} never leaves its warm-up on {} bars",
            bars.len(),
        );
    }
}

fn check(name: &str, param_sets: &[&[f64]]) {
    let bars = series(SEED);
    for raw in param_sets {
        assert_stream_equals_batch(name, raw, &bars, None);
    }
}

/// A paramless indicator has one parameter set, so vary the series instead.
fn check_paramless(name: &str) {
    for seed in [SEED, 0x0123_4567_89AB_CDEF, 0xD1B5_4A32_D192_ED03] {
        assert_stream_equals_batch(name, &[], &series(seed), None);
    }
}

#[test]
fn gmma_streams_bit_equal_to_the_batch() {
    check_paramless("gmma");
}

#[test]
fn alligator_streams_bit_equal_to_the_batch() {
    check_paramless("alligator");
}

#[test]
fn relative_vigor_streams_bit_equal_to_the_batch() {
    check(
        "relative_vigor",
        &[
            &[],      // the defaults
            &[2.0],   // the smallest period
            &[5.0],   // small
            &[40.0],  // covers the whole flat stretch: rvgi's range sum is zero there
            &[200.0], // large
        ],
    );
}

#[test]
fn high_low_52w_streams_bit_equal_to_the_batch() {
    check(
        "high_low_52w",
        &[
            &[],       // the defaults
            &[2.0],    // the smallest period
            &[20.0],   // small
            &[500.0],  // larger
            &[1000.0], // the registry maximum: warm-up at bar 999
        ],
    );
}

#[test]
fn stochrsi_streams_bit_equal_to_the_batch() {
    check(
        "stochrsi",
        &[
            &[],                   // the defaults
            &[2.0, 2.0, 1.0],      // every parameter at its smallest
            &[5.0, 7.0, 3.0],      // small
            &[30.0, 50.0, 10.0],   // larger
            &[100.0, 100.0, 50.0], // every parameter at its largest
        ],
    );
}

#[test]
fn fisher_streams_bit_equal_to_the_batch() {
    check(
        "fisher",
        &[
            &[],      // the defaults
            &[2.0],   // the smallest period
            &[5.0],   // small
            &[50.0],  // larger
            &[100.0], // the registry maximum
        ],
    );
}

#[test]
fn ichimoku_streams_bit_equal_to_the_batch_except_chikou() {
    let bars = series(SEED);
    for raw in [
        &[][..],                    // the defaults
        &[2.0, 2.0, 2.0][..],       // every parameter at its smallest
        &[5.0, 10.0, 20.0][..],     // small
        &[20.0, 60.0, 120.0][..],   // larger
        &[100.0, 100.0, 200.0][..], // every parameter at its largest
    ] {
        // Line 4 is `chikou`, which reads future bars: see the module doc.
        assert_stream_equals_batch("ichimoku", raw, &bars, Some(4));
    }
}

#[test]
fn volume_profile_poc_streams_bit_equal_to_the_batch() {
    check(
        "volume_profile_poc",
        &[
            &[],             // the defaults
            &[5.0, 4.0],     // the smallest window and bin count
            &[30.0, 10.0],   // a window inside the flat stretch: the degenerate-range branch
            &[100.0, 50.0],  // larger
            &[500.0, 200.0], // the registry maximum of both
        ],
    );
}
