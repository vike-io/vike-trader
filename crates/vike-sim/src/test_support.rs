//! The bar, tick and calendar builders this crate's test binaries share, compiled ONCE here instead of
//! once per binary that `#[path]`-included a `tests/common/` copy (today the `integration` binary,
//! `controller_harness` and `r7_gate`), plus the unified benchmark schedule ([`target_at`]). Behind
//! `test-support`, which only dev edges turn on (this crate's own self dev-dependency, and vike-backtest's
//! for its `engines` bench), so a default build compiles none of it; the `vike_marketdata::test_support` model.
//!
//! These are NOT `vike_marketdata::test_support`'s `quote`/`trade`: those pin both sizes to `1.0`
//! and an empty symbol, these take the symbol and the sizes, and wrap the tick in this crate's
//! [`crate::Tick`]. A member reaches one as `vike_sim::test_support::quote`.

use vike_marketdata::{Bar, QuoteTick, TradeTick};
use vike_util::calendar::days_from_civil;

use crate::Tick;

/// One OHLC bar at `ts` (ms) with an explicit `volume`; no funding, no quote, no symbol stamp.
pub fn bar(ts: i64, open: f64, high: f64, low: f64, close: f64, volume: f64) -> Bar {
    Bar { ts, open, high, low, close, volume, funding: None, bid: None, ask: None, symbol: None }
}

/// `n` flat one-minute [`bar`]s at `px` — the first at minute 1 (60 000 ms) — each trading `volume`.
pub fn flat_minute_bars(n: usize, px: f64, volume: f64) -> Vec<Bar> {
    (0..n).map(|i| bar(60_000 * (i as i64 + 1), px, px, px, px, volume)).collect()
}

/// Epoch-ms for a UTC instant, via the workspace's one civil-calendar math (`vike_util::calendar`).
pub fn ms(y: i64, mo: u32, d: u32, h: i64) -> i64 {
    days_from_civil(y, mo, d) * 86_400_000 + h * 3_600_000
}

/// One L1 quote for `symbol` at `ts` (ms), with no local receive time (`local_ts: 0`).
pub fn quote(symbol: &str, ts: i64, bid: f64, ask: f64, bid_size: f64, ask_size: f64) -> Tick {
    Tick::Quote(QuoteTick {
        ts,
        local_ts: 0,
        bid,
        ask,
        bid_size,
        ask_size,
        symbol: symbol.to_string(),
    })
}

/// One trade print for `symbol` at `ts` (ms): a taker BUY (`is_buyer_maker: false`), no local
/// receive time (`local_ts: 0`).
pub fn trade(symbol: &str, ts: i64, price: f64, size: f64) -> Tick {
    Tick::Trade(TradeTick {
        ts,
        local_ts: 0,
        price,
        size,
        is_buyer_maker: false,
        symbol: symbol.to_string(),
    })
}

/// A deterministic bar walk (an LCG: no clock, no `rand`, and `+ - * /` only, so the same series on
/// every platform): `n` one-minute bars from `ts` 60 000, each stamped with `symbol`. `r7_gate` and
/// `controller_harness` replay it through both runtimes.
pub fn gen_bars(n: usize, symbol: Option<&str>) -> Vec<Bar> {
    let mut state: u64 = 0x9E3779B97F4A7C15;
    let mut px = 100.0f64;
    let mut bars = Vec::with_capacity(n);
    for i in 0..n {
        let mut r = || {
            state = state.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
            (state >> 32) as f64 / (1u64 << 32) as f64 * 2.0 - 1.0 // [-1, 1)
        };
        let drift = r() * 0.8;
        let open = px;
        let close = (px + drift).max(1.0);
        let high = open.max(close) + r().abs() * 0.3;
        let low = (open.min(close) - r().abs() * 0.3).max(0.5);
        bars.push(Bar {
            ts: 60_000 * (i as i64 + 1),
            open,
            high,
            low,
            close,
            volume: 10.0 + r().abs() * 5.0,
            funding: None,
            bid: None,
            ask: None,
            symbol: symbol.map(str::to_string),
        });
        px = close;
    }
    bars
}

/// The UNIFIED benchmark strategy's schedule — long/flat at [`W_PCT`] of equity, entering every
/// [`W_ON`] bars and closing [`W_OFF`] bars later — which `crates/vike-backtest/benches/engines.rs`
/// times and `crates/vike-sim/tests/parity/engine_kernel_parity.rs` holds both kernels equal over (each
/// carried a copy until 2026-10-31).
pub const W_ON: usize = 20;
/// Bars after an entry that the position closes (see [`W_ON`]).
pub const W_OFF: usize = 10;
/// The equity fraction an entry targets (see [`W_ON`]).
pub const W_PCT: f64 = 0.5;

/// The target weight at bar `i` under the [`W_ON`] schedule: [`W_PCT`] on an entry bar, `0.0` on its
/// exit bar, `None` (hold) otherwise.
#[inline]
pub fn target_at(i: usize) -> Option<f64> {
    if i.is_multiple_of(W_ON) {
        Some(W_PCT)
    } else if i % W_ON == W_OFF {
        Some(0.0)
    } else {
        None
    }
}
