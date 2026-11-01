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

use crate::streamers::{check_all, rng_series};

fn check(name: &str, param_sets: &[&[f64]]) {
    check_all(&rng_series(0x9E37_79B9_7F4A_7C15, 0, None), name, param_sets);
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
