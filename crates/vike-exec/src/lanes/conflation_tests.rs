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

/// "Marked" is honest: when the ingest queue is FULL the marker is not queued and `marker_in_flight`
/// stays false, the tick safe in its slot; the next publish queues the marker once there is room. A
/// producer never holds a marker "in flight" that the core was not handed (`MarketSender::arm_marker`
/// sets the flag only after `try_send` accepted it).
#[test]
fn a_full_ingest_queue_leaves_the_marker_unsent_and_the_next_publish_retries_it() {
    let (bars, market, mut rx) = market_data_channel(1);
    bars.ingest.try_send(Ingest::Watchdog).expect("the queue has room for one message");
    market.publish(tick(101.0, 1));
    {
        let st = market.inner.state.lock().unwrap();
        assert!(!st.marker_in_flight, "the marker was refused, so nothing is in flight");
        assert_eq!(st.slots[0].px, 101.0, "the tick waits in its slot");
    }
    assert!(matches!(rx.try_recv(), Ok(Ingest::Watchdog)));
    market.publish(tick(102.0, 2));
    assert!(market.inner.state.lock().unwrap().marker_in_flight, "queued once there is room");
    assert!(matches!(rx.try_recv(), Ok(Ingest::Market)));
}
