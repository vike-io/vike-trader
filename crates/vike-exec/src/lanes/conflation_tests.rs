//! Conflation of the market-data lane: real-mark and bar-close ticks keep separate slots.

use super::*;

fn tick(px: f64, ts: i64) -> MarketTick {
    MarketTick { venue: "sim".into(), symbol: "BTCUSDT".into(), px, ts }
}

/// Mark-slot semantics: real-mark (`publish`) and candle-close (`publish_bar_close`)
/// ticks conflate INDEPENDENTLY — a 1s-cadence real-mark stream can never overwrite a kline
/// feed's bar-close tick for the same (venue, symbol), and only a SAME-lane overwrite counts
/// as a conflation drop.
#[test]
fn bar_close_and_mark_ticks_conflate_in_separate_slots() {
    let (_bars, market, _rx) = market_data_channel(8);
    market.publish(tick(101.0, 1));
    market.publish_bar_close(tick(100.0, 2));
    market.publish_bar_close(tick(99.0, 3)); // conflates the previous BAR-CLOSE tick only
    {
        let st = market.inner.state.lock().unwrap();
        assert_eq!(st.slots.len(), 1, "one real-mark slot");
        assert_eq!(st.slots[0].px, 101.0, "the real mark survives bar-close publishes");
        assert_eq!(st.bar_close_slots.len(), 1, "one bar-close slot");
        assert_eq!(st.bar_close_slots[0].px, 99.0, "bar-close latest-wins in its own slot");
    }
    assert_eq!(
        market.inner.drops.load(Ordering::Relaxed),
        1,
        "only the same-lane overwrite counts as a conflation drop"
    );
}
