//! Cleaved from `binance/data.rs` — the venue-neutral kline→[`Bar`] constructor every kline-based
//! venue bridge (binance/bybit/okx) builds its `Bar`s through, so bar shape stays identical across
//! venues (Phase 3 PR A). Promoted from `pub(crate)` to `pub` — cross-crate callers after the split.

use vike_model::Bar;

/// One kline → a [`Bar`]. The series key carries venue/symbol/interval, so those stay `None` here.
pub fn kline_to_bar(t: i64, o: f64, h: f64, l: f64, c: f64, v: f64) -> Bar {
    Bar {
        ts: t,
        open: o,
        high: h,
        low: l,
        close: c,
        volume: v,
        funding: None,
        bid: None,
        ask: None,
        symbol: None,
    }
}
