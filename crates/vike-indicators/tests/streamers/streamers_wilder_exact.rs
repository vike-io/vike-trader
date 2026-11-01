//! `adx`, `adxr`, `natr`, `supertrend`, `smma` and `efi` stream from bounded state (a Wilder or EMA
//! recurrence), not from a history recompute. This is the exactness proof for that, independent of
//! `tests/parity`: fold `on_bar` over a long, awkward series and require EVERY bar and EVERY output
//! line to be bit-equal to what the registry's batch path (`compute`, i.e. `vectorize`) holds at
//! that index, `NaN`s at the same warm-up positions included.
//!
//! The series is deterministic and built to hurt a smoother: a long flat stretch (zero changes, zero
//! ranges, `high == low`), a zero-volume stretch (which makes `efi`'s force exactly `-0.0` whenever
//! the close falls), a large price gap in each direction, one isolated bar with `high == low` that
//! still moves the close, and one bar that closes at exactly zero (`natr`'s `NaN` branch). Each
//! indicator runs at its defaults, at small periods and at larger ones, every value inside the
//! registry's range.

use crate::streamers::{check_all, rng_series};

fn check(name: &str, param_sets: &[&[f64]]) {
    check_all(&rng_series(0xD1B5_4A32_D192_ED03, 0, Some(1_000)), name, param_sets);
}

#[test]
fn adx_streams_bit_equal_to_the_batch() {
    check(
        "adx",
        &[
            &[],      // the defaults
            &[2.0],   // the smallest period
            &[5.0],   // small
            &[30.0],  // medium
            &[100.0], // the largest: the ADX line starts at bar 199
        ],
    );
}

#[test]
fn adxr_streams_bit_equal_to_the_batch() {
    check(
        "adxr",
        &[
            &[],      // the defaults
            &[2.0],   // the smallest period
            &[6.0],   // small
            &[40.0],  // medium
            &[100.0], // the largest: the line starts at bar 299
        ],
    );
}

#[test]
fn natr_streams_bit_equal_to_the_batch() {
    check(
        "natr",
        &[
            &[],      // the defaults
            &[2.0],   // the smallest period
            &[7.0],   // small
            &[50.0],  // medium
            &[100.0], // the largest
        ],
    );
}

#[test]
fn supertrend_streams_bit_equal_to_the_batch() {
    check(
        "supertrend",
        &[
            &[],            // the defaults
            &[1.0, 0.5],    // the smallest period, the narrowest band
            &[3.0, 1.5],    // small
            &[20.0, 3.0],   // medium
            &[100.0, 10.0], // the largest period, the widest band
            &[7.0, 10.0],   // bands wide enough that the trend rarely flips
        ],
    );
}

#[test]
fn smma_streams_bit_equal_to_the_batch() {
    check(
        "smma",
        &[
            &[],      // the defaults
            &[2.0],   // the smallest period
            &[9.0],   // small
            &[100.0], // medium
            &[400.0], // the largest
        ],
    );
}

#[test]
fn efi_streams_bit_equal_to_the_batch() {
    check(
        "efi",
        &[
            &[],      // the defaults
            &[2.0],   // the smallest period: a window can sit wholly inside the zero-volume stretch
            &[3.0],   // small
            &[50.0],  // medium
            &[200.0], // the largest
        ],
    );
}
