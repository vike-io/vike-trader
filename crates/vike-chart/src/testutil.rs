//! Shared fixtures for the crate's unit-test modules: render and wire bars on a one-minute epoch.
//! `pub(crate)` items are the ones modules outside `model` (`chart`, `interact`, `indicators`) call;
//! each had its own copy. The fixtures the integration tests share too (`flat_bars`, the
//! fully-closed `make_state`) are `crate::test_support`'s; several here call platform `sin`/`cos`,
//! which only a `#[cfg(test)]` module may (that module's doc says why).

use super::*;

pub(super) fn bar(i: usize, l: f64, h: f64) -> Bar {
    Bar {
        t: i as f64,
        // one-minute bars starting at an arbitrary epoch minute
        ot: 1_700_000_040_000 + i as i64 * 60_000,
        o: (l + h) / 2.0,
        h,
        l,
        c: (l + h) / 2.0,
        v: 1.0,
    }
}

/// One `vike_marketdata::Bar` (the wire type `sync` reads FROM) at index `i`, on the
/// same one-minute-bar epoch as [`bar`] above. `jitter` perturbs `close`/`high` so
/// a "forming tick" mutation is distinguishable from the bar's prior values.
pub(super) fn raw_bar(i: usize, jitter: f64) -> vike_marketdata::Bar {
    let ts = 1_700_000_040_000 + i as i64 * 60_000;
    let base = 100.0 + (i as f64 * 0.31).sin() * 3.0;
    let (high, close) = (base + 1.0 + jitter.abs(), base + jitter);
    vike_marketdata::Bar::ohlcv(ts, base, high, base - 1.0, close, 500.0 + i as f64)
}

/// A `vike_marketdata::Bar` (wire type) at an explicit open-time `ts` (ms) — for tz-mark tests
/// that need specific wall-clock instants rather than [`raw_bar`]'s index-derived
/// one-minute epoch.
pub(super) fn wire_bar_at(ts: i64) -> vike_marketdata::Bar {
    vike_marketdata::Bar::ohlcv(ts, 1.0, 2.0, 1.0, 1.5, 1.0)
}

/// A gently-varying, all-closed OHLCV series (deterministic) — safe for every style incl. the
/// path-dependent transforms. Mirrors the characterization harness's `wave_bars`.
pub(crate) fn wave(n: usize) -> Vec<Bar> {
    (0..n)
        .map(|i| {
            let base = 100.0 + (i as f64 * 0.30).sin() * 6.0;
            Bar {
                t: i as f64,
                ot: 1_700_000_000_000 + i as i64 * 60_000,
                o: base,
                h: base + 2.5,
                l: base - 2.5,
                c: base + (i as f64 * 0.7).cos(),
                v: 10.0 + (i % 7) as f64,
            }
        })
        .collect()
}

/// A gently-varying bar at index `i`; `c_shift` moves the close (and widens the high) so a
/// forming-tick mutation is distinguishable from the closed value.
pub(crate) fn gb(i: usize, c_shift: f64) -> Bar {
    let base = 100.0 + (i as f64 * 0.31).sin() * 3.0;
    Bar {
        t: i as f64,
        ot: 1_700_000_000_000 + i as i64 * 60_000,
        o: base,
        h: base + 1.0 + c_shift.abs(),
        l: base - 1.0,
        c: base + c_shift,
        v: 500.0 + i as f64,
    }
}

/// `n` closed [`gb`] bars, each with a 0.3 close shift.
pub(crate) fn closed_bars(n: usize) -> Vec<Bar> {
    (0..n).map(|i| gb(i, 0.3)).collect()
}
