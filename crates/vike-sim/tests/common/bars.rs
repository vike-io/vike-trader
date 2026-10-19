//! The OHLC bar builder shared by this crate's test binaries.

use vike_marketdata::Bar;

/// One OHLC bar at `ts` (ms) with an explicit `volume`; no funding, no quote, no symbol stamp.
pub fn bar(ts: i64, open: f64, high: f64, low: f64, close: f64, volume: f64) -> Bar {
    Bar { ts, open, high, low, close, volume, funding: None, bid: None, ask: None, symbol: None }
}
