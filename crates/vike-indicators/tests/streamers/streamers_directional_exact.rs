//! `dx`, `plus_di`, `minus_di`, `plus_dm`, `minus_dm`, `chaikin_volatility`, `autocorr`, `chandelier`
//! and `starc` stream from bounded state (Wilder/EMA recurrences and fixed rings), not from a history
//! recompute. This is the exactness proof for that, independent of `tests/parity`: fold `on_bar` over a
//! long, awkward series and require EVERY bar and EVERY output line to be bit-equal to what the
//! registry's batch path (`compute`, i.e. `vectorize`) holds at that index, `NaN`s at the same warm-up
//! positions included.
//!
//! The series is deterministic and built to hurt a smoother: a long flat stretch (zero changes, zero
//! ranges, `high == low`, so every `autocorr` window inside it is constant), a zero-volume stretch, a
//! large price gap in each direction, one isolated bar with `high == low` that still moves the close,
//! and one bar that closes at exactly zero. Each indicator runs at its defaults, at the smallest
//! values the registry allows and at larger ones, every value inside the registry's range (`make_with`
//! clamps, so an out-of-range value would silently test something else).

use vike_marketdata::Bar;

use crate::streamers::{assert_stream_equals_batch, check_all, rng_series};

fn series() -> Vec<Bar> {
    rng_series(0xD1B5_4A32_D192_ED03, 0, Some(1_000))
}

fn check(name: &str, param_sets: &[&[f64]]) {
    check_all(&series(), name, param_sets);
}

#[test]
fn dx_streams_bit_equal_to_the_batch() {
    check(
        "dx",
        &[
            &[],      // the defaults
            &[2.0],   // the smallest period
            &[5.0],   // small
            &[30.0],  // medium
            &[100.0], // the largest
        ],
    );
}

#[test]
fn plus_di_streams_bit_equal_to_the_batch() {
    check(
        "plus_di",
        &[
            &[],      // the defaults
            &[2.0],   // the smallest period
            &[6.0],   // small
            &[40.0],  // medium
            &[100.0], // the largest
        ],
    );
}

#[test]
fn minus_di_streams_bit_equal_to_the_batch() {
    check(
        "minus_di",
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
fn plus_dm_streams_bit_equal_to_the_batch() {
    check(
        "plus_dm",
        &[
            &[],      // the defaults
            &[2.0],   // the smallest period
            &[8.0],   // small
            &[35.0],  // medium
            &[100.0], // the largest
        ],
    );
}

#[test]
fn minus_dm_streams_bit_equal_to_the_batch() {
    check(
        "minus_dm",
        &[
            &[],      // the defaults
            &[2.0],   // the smallest period
            &[9.0],   // small
            &[45.0],  // medium
            &[100.0], // the largest
        ],
    );
}

#[test]
fn chaikin_volatility_streams_bit_equal_to_the_batch() {
    check(
        "chaikin_volatility",
        &[
            &[],             // the defaults
            &[2.0, 1.0],     // the smallest of both
            &[5.0, 3.0],     // small
            &[30.0, 20.0],   // medium
            &[100.0, 100.0], // the largest of both
            &[2.0, 100.0],   // a fast EMA compared against a far-back value
            &[100.0, 1.0],   // a slow EMA, one-bar change
        ],
    );
}

#[test]
fn autocorr_streams_bit_equal_to_the_batch() {
    check(
        "autocorr",
        &[
            &[],            // the defaults
            &[5.0, 1.0],    // the smallest period, lag 1
            &[5.0, 3.0],    // the smallest window that still has two pairs for lag 3
            &[30.0, 7.0],   // a lag well above 1
            &[12.0, 10.0],  // exactly `lag + 2`: two pairs
            &[200.0, 50.0], // the largest of both
        ],
    );
}

#[test]
fn autocorr_with_period_below_lag_plus_two_is_all_nan_in_both_paths() {
    let bars = series();
    // `period < lag + 2`: the correlation is undefined, so batch and stream are all NaN.
    for raw in [&[5.0, 4.0][..], &[5.0, 50.0], &[20.0, 19.0], &[30.0, 29.0]] {
        assert_stream_equals_batch("autocorr", raw, &bars, None, true);
    }
}

#[test]
fn chandelier_streams_bit_equal_to_the_batch() {
    check(
        "chandelier",
        &[
            &[],            // the defaults
            &[2.0, 0.5],    // the smallest of both
            &[10.0, 1.5],   // small
            &[60.0, 5.0],   // medium
            &[200.0, 10.0], // the largest of both
            &[22.0, 10.0],  // the default window, the widest multiple
        ],
    );
}

#[test]
fn starc_streams_bit_equal_to_the_batch() {
    check(
        "starc",
        &[
            &[],                   // the defaults
            &[1.0, 1.0, 0.5],      // the smallest of all three
            &[3.0, 2.0, 1.0],      // small
            &[50.0, 30.0, 5.0],    // medium
            &[200.0, 200.0, 10.0], // the largest of all three
            &[200.0, 1.0, 2.0],    // a long average, an ATR seeded by a single true range
            &[1.0, 200.0, 2.0],    // a one-bar average, a long ATR
        ],
    );
}
