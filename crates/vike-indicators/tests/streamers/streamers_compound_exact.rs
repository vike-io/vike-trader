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

use crate::streamers::{check_all, rng_series};

fn check(name: &str, param_sets: &[&[f64]]) {
    check_all(&rng_series(0x9E37_79B9_7F4A_7C15, 25, Some(1_000)), name, param_sets);
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

/// The `(fast, slow)` sets `ppo` and `apo` share: the two rows take the same pair of periods.
const FAST_SLOW_SETS: &[&[f64]] = &[
    &[],             // the defaults
    &[2.0, 2.0],     // both at the smallest period: the lines are equal
    &[2.0, 3.0],     // small
    &[9.0, 4.0],     // fast above slow
    &[50.0, 120.0],  // medium
    &[200.0, 400.0], // the largest periods
];

#[test]
fn ppo_streams_bit_equal_to_the_batch() {
    check("ppo", FAST_SLOW_SETS);
}

#[test]
fn apo_streams_bit_equal_to_the_batch() {
    check("apo", FAST_SLOW_SETS);
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
