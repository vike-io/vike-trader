use super::{BarFolder, BarRoll, FeedCtx, Feeds, KEEPALIVE_PING};
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::Duration;
use vike_bridge_core::klines::kline_to_bar;
use vike_data::{DataClient, RecordingSink};

/// A network-free stand-in for a lane main: polls its own stop flag at the real feeds' WS
/// cadence without ever touching a socket — the shared lifecycle-test idiom.
fn fake_feed_body(_symbol: String, _interval: String, ctx: FeedCtx) {
    while !ctx.stop.load(Ordering::Relaxed) {
        std::thread::sleep(Duration::from_millis(10));
    }
}

#[test]
fn subscribe_returns_distinct_ids() {
    let sink = Arc::new(RecordingSink::default());
    let mut feeds = Feeds::new(sink, || {});
    let id1 = feeds.spawn_with("BTC-PERPETUAL", "1m", fake_feed_body).expect("spawn ok");
    let id2 = feeds.spawn_with("ETH-PERPETUAL", "1m", fake_feed_body).expect("spawn ok");
    assert_ne!(id1, id2, "distinct ids per subscribe");
    feeds.shutdown();
}

#[test]
fn unsubscribe_stops_only_that_one_feed_others_keep_running() {
    let sink = Arc::new(RecordingSink::default());
    let mut feeds = Feeds::new(sink, || {});
    let id1 = feeds.spawn_with("BTC-PERPETUAL", "1m", fake_feed_body).expect("spawn ok");
    let id2 = feeds.spawn_with("ETH-PERPETUAL", "1m", fake_feed_body).expect("spawn ok");

    feeds.unsubscribe(id1);
    assert_eq!(feeds.registry.len(), 1, "only the unsubscribed stream is removed");
    assert!(feeds.registry.contains(id2), "the other subscription keeps running");

    feeds.shutdown();
    assert!(feeds.registry.is_empty());
}

#[test]
fn unsubscribe_of_an_unknown_id_is_a_no_op() {
    let sink = Arc::new(RecordingSink::default());
    let mut feeds = Feeds::new(sink, || {});
    let id = feeds.spawn_with("BTC-PERPETUAL", "1m", fake_feed_body).expect("spawn ok");
    feeds.unsubscribe(vike_data::SubscriptionId(id.0 + 100)); // never issued
    assert_eq!(feeds.registry.len(), 1, "unknown id must not disturb the real subscription");
    feeds.shutdown();
}

#[test]
fn shutdown_joins_every_feed() {
    let sink = Arc::new(RecordingSink::default());
    let mut feeds = Feeds::new(sink, || {});
    feeds.spawn_with("BTC-PERPETUAL", "1m", fake_feed_body).expect("spawn ok");
    feeds.spawn_with("ETH-PERPETUAL", "1m", fake_feed_body).expect("spawn ok");
    feeds.spawn_with("SOL_USDC-PERPETUAL", "1m", fake_feed_body).expect("spawn ok");
    feeds.shutdown(); // must return only once every thread has actually joined
    assert!(feeds.registry.is_empty());
}

/// The ONE refused verb, driven off the declared caps row (never a hand-rolled string).
#[test]
fn depth_is_unsupported_via_the_declared_row() {
    let sink = Arc::new(RecordingSink::default());
    let mut feeds = Feeds::new(sink, || {});
    assert!(matches!(
        feeds.subscribe_depth("BTC-PERPETUAL"),
        Err(vike_data::LiveDataError::Unsupported(_))
    ));
}

/// The keepalive payload is a well-formed `public/test` JSON-RPC request — the frame whose
/// reply resets the idle watchdog on a quiet channel (the row declares the cadence).
#[test]
fn keepalive_is_a_public_test_rpc() {
    let v: serde_json::Value = serde_json::from_str(KEEPALIVE_PING).unwrap();
    assert_eq!(v["method"], "public/test");
}

/// `pump_opts` CONSUMES the venue's pump_spec row (row ownership): the subscribe payload and
/// the keepalive text are the venue's; every timing knob is the row's.
#[test]
fn pump_opts_consume_the_declared_row() {
    let sub = super::public_subscribe_frame(&["book.BTC-PERPETUAL.100ms".to_string()]);
    let opts = super::pump_opts(&sub);
    assert_eq!(opts.subscribe, Some(sub.as_str()));
    let knobs = vike_bridge_core::pump_spec::market_pump_spec("deribit").knobs();
    let ka = opts.keepalive.expect("the row declares a keepalive cadence");
    assert_eq!(ka.payload, KEEPALIVE_PING);
    assert_eq!(Some(ka.every), knobs.keepalive_every);
    assert_eq!(opts.read_timeout, knobs.read_timeout);
    assert_eq!(opts.idle_threshold, knobs.idle_threshold);
    assert_eq!(opts.connect_timeout, knobs.connect_timeout);
}

// ── BarFolder: the closed-bar inference ─────────────────────────────────────────────────────

fn bar(ts: i64, close: f64) -> vike_model::Bar {
    kline_to_bar(ts, close, close, close, close, 1.0)
}

#[test]
fn the_first_push_forms_and_closes_nothing() {
    let mut f = BarFolder::new();
    let out = f.fold(bar(60_000, 100.0));
    assert_eq!(out, Some(BarRoll { closed: None, forming: bar(60_000, 100.0) }));
}

#[test]
fn a_same_bucket_push_updates_the_forming_picture() {
    let mut f = BarFolder::new();
    f.fold(bar(60_000, 100.0));
    let out = f.fold(bar(60_000, 101.0));
    assert_eq!(out, Some(BarRoll { closed: None, forming: bar(60_000, 101.0) }));
}

#[test]
fn a_newer_bucket_closes_the_held_one_at_its_last_state() {
    let mut f = BarFolder::new();
    f.fold(bar(60_000, 100.0));
    f.fold(bar(60_000, 101.0));
    let out = f.fold(bar(120_000, 102.0));
    assert_eq!(
        out,
        Some(BarRoll { closed: Some(bar(60_000, 101.0)), forming: bar(120_000, 102.0) }),
        "the close is the bucket's LAST observed state, not its first"
    );
}

/// A quiet instrument can skip buckets entirely (chart pushes are trade-driven): the held
/// bucket still closes; the skipped empties are NOT fabricated (module-doc honesty rule).
#[test]
fn a_bucket_jump_closes_the_held_bar_without_fabricating_empties() {
    let mut f = BarFolder::new();
    f.fold(bar(60_000, 100.0));
    let out = f.fold(bar(300_000, 105.0));
    assert_eq!(
        out,
        Some(BarRoll { closed: Some(bar(60_000, 100.0)), forming: bar(300_000, 105.0) })
    );
}

#[test]
fn an_older_bucket_is_stale_and_changes_nothing() {
    let mut f = BarFolder::new();
    f.fold(bar(120_000, 102.0));
    assert_eq!(f.fold(bar(60_000, 999.0)), None);
    // …and the forming picture is untouched.
    let out = f.fold(bar(120_000, 103.0));
    assert_eq!(out, Some(BarRoll { closed: None, forming: bar(120_000, 103.0) }));
}
