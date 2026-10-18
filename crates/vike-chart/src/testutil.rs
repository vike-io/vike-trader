//! Shared fixtures for the `model` test modules: render and wire bars on a one-minute epoch.

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
    vike_marketdata::Bar {
        ts,
        open: base,
        high: base + 1.0 + jitter.abs(),
        low: base - 1.0,
        close: base + jitter,
        volume: 500.0 + i as f64,
        funding: None,
        bid: None,
        ask: None,
        symbol: None,
    }
}

/// A `vike_marketdata::Bar` (wire type) at an explicit open-time `ts` (ms) — for tz-mark tests
/// that need specific wall-clock instants rather than [`raw_bar`]'s index-derived
/// one-minute epoch.
pub(super) fn wire_bar_at(ts: i64) -> vike_marketdata::Bar {
    vike_marketdata::Bar {
        ts,
        open: 1.0,
        high: 2.0,
        low: 1.0,
        close: 1.5,
        volume: 1.0,
        funding: None,
        bid: None,
        ask: None,
        symbol: None,
    }
}
