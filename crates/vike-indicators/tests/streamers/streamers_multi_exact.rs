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

use vike_marketdata::Bar;

use crate::streamers::{assert_stream_equals_batch, check_all, rng_series};

fn series(seed: u64) -> Vec<Bar> {
    rng_series(seed, 0, Some(700))
}

const SEED: u64 = 0x9E37_79B9_7F4A_7C15;

fn check(name: &str, param_sets: &[&[f64]]) {
    check_all(&series(SEED), name, param_sets);
}

/// A paramless indicator has one parameter set, so vary the series instead.
fn check_paramless(name: &str) {
    for seed in [SEED, 0x0123_4567_89AB_CDEF, 0xD1B5_4A32_D192_ED03] {
        assert_stream_equals_batch(name, &[], &series(seed), None, false);
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
        assert_stream_equals_batch("ichimoku", raw, &bars, Some(4), false);
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
