//! The render-model bar: [`ChartBar`] (an integer bar-index x) and the one mapping from the
//! wire `vike_marketdata::Bar` into it.

/// One OHLC bar. `t` is the bar index, NOT a unix timestamp.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ChartBar {
    pub t: f64,  // bar index (x)
    pub ot: i64, // open time (ms) — for the time x-axis
    pub o: f64,
    pub h: f64,
    pub l: f64,
    pub c: f64,
    pub v: f64, // base-asset volume
}

/// Map one wire-format `vike_marketdata::Bar` (venue/core-side OHLCV) to the render-model
/// `ChartBar` at bar-index `i`. The single source of truth for that mapping — both
/// `ChartState::sync`'s fast paths and its full-rebuild fallback go through this.
pub(super) fn render_bar(i: usize, b: &vike_marketdata::Bar) -> ChartBar {
    ChartBar {
        t: i as f64,
        ot: b.timestamp,
        o: b.open,
        h: b.high,
        l: b.low,
        c: b.close,
        v: b.volume,
    }
}
