use super::{FeedCtx, Feeds};
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::Duration;
use vike_bridge_core::Environment;
use vike_data::DataClient;

// The shared capturing sink (testing-arch Phase 4d) — replaces the inline formatted-string
// RecordingSink copy that used to live here (same canonical `calls()` line forms).
use vike_data::RecordingSink;

/// A network-free stand-in for the shared `feed_main`: just polls its own stop flag, same
/// cadence as the real feed's WS loop, without ever touching a socket. Lets the per-key
/// lifecycle (spawn/unsubscribe/shutdown) be exercised deterministically in `cargo test`.
fn fake_feed_body(_symbol: String, _interval: String, ctx: FeedCtx) {
    while !ctx.stop.load(Ordering::Relaxed) {
        std::thread::sleep(Duration::from_millis(10));
    }
}

#[test]
fn subscribe_returns_distinct_ids() {
    let sink = Arc::new(RecordingSink::default());
    let mut feeds = Feeds::new(sink, || {});
    let id1 = feeds.spawn_with("BTCUSDT", "1m", fake_feed_body).expect("spawn ok");
    let id2 = feeds.spawn_with("ETHUSDT", "1m", fake_feed_body).expect("spawn ok");
    assert_ne!(id1, id2, "distinct ids per subscribe");
    feeds.shutdown();
}

#[test]
fn unsubscribe_stops_only_that_one_feed_others_keep_running() {
    let sink = Arc::new(RecordingSink::default());
    let mut feeds = Feeds::new(sink, || {});
    let id1 = feeds.spawn_with("BTCUSDT", "1m", fake_feed_body).expect("spawn ok");
    let id2 = feeds.spawn_with("ETHUSDT", "1m", fake_feed_body).expect("spawn ok");

    feeds.unsubscribe(id1);
    assert_eq!(feeds.subs.len(), 1, "only the unsubscribed stream is removed");
    assert!(feeds.subs.contains_key(&id2), "the other subscription keeps running");

    feeds.shutdown();
    assert!(feeds.subs.is_empty());
}

#[test]
fn unsubscribe_of_an_unknown_id_is_a_no_op() {
    let sink = Arc::new(RecordingSink::default());
    let mut feeds = Feeds::new(sink, || {});
    let id = feeds.spawn_with("BTCUSDT", "1m", fake_feed_body).expect("spawn ok");
    feeds.unsubscribe(vike_data::SubscriptionId(id.0 + 100)); // never issued
    assert_eq!(feeds.subs.len(), 1, "unknown id must not disturb the real subscription");
    feeds.shutdown();
}

#[test]
fn shutdown_joins_every_feed() {
    let sink = Arc::new(RecordingSink::default());
    let mut feeds = Feeds::new(sink, || {});
    feeds.spawn_with("BTCUSDT", "1m", fake_feed_body).expect("spawn ok");
    feeds.spawn_with("ETHUSDT", "1m", fake_feed_body).expect("spawn ok");
    feeds.spawn_with("SOLUSDT", "1m", fake_feed_body).expect("spawn ok");
    feeds.shutdown(); // must return only once every thread has actually joined
    assert!(feeds.subs.is_empty());
}

#[test]
fn quotes_and_book_are_unsupported() {
    // subscribe_trades is deliberately NOT asserted here — it wires a real feed thread (real
    // WS connect + REST warmup), so exercising it from a unit test would attempt a live
    // network connection. Its pure decode/splice logic is covered by trades.rs's own tests;
    // the loop itself is covered by the crate's live-smoke conventions (see
    // `tests/aster_market_data_smoke.rs`).
    let sink = Arc::new(RecordingSink::default());
    let mut feeds = Feeds::new(sink, || {});
    assert!(matches!(
        feeds.subscribe_quotes("BTCUSDT"),
        Err(vike_data::LiveDataError::Unsupported(_))
    ));
    assert!(matches!(
        feeds.subscribe_book("BTCUSDT"),
        Err(vike_data::LiveDataError::Unsupported(_))
    ));
}

/// `Feeds::new` defaults to testnet (`Environment::Demo`); `with_env` takes an explicit tier —
/// the one behavioral divergence this port adds over the binance template (which has no
/// concept of testnet/mainnet: it's hardcoded to one host per instrument class).
#[test]
fn new_defaults_to_demo_env_with_env_takes_explicit_tier() {
    let sink = Arc::new(RecordingSink::default());
    let demo = Feeds::new(sink.clone(), || {});
    assert_eq!(demo.env, Environment::Demo);
    let live = Feeds::with_env(sink, || {}, Environment::Live);
    assert_eq!(live.env, Environment::Live);
}

/// Rung-2 guard: the kline + depth WS URLs the shared feed builds for Aster must be the
/// `env`-resolved sapi/fapi stream hosts — depth spot-only (the futures book is
/// `market_data.rs`'s track).
#[test]
fn aster_kline_and_depth_urls_are_env_resolved() {
    use vike_binance::family::market_feed::{depth_ws_url, kline_ws_url};
    let urls = crate::trades::spec(Environment::Demo).urls;
    assert_eq!(
        kline_ws_url(urls.ws(false), "BTCUSDT", "1m"),
        "wss://sstream.asterdex-testnet.com/ws/btcusdt@kline_1m"
    );
    assert_eq!(
        kline_ws_url(urls.ws(true), "BTCUSDT", "1m"),
        "wss://fstream.asterdex-testnet.com/ws/btcusdt@kline_1m"
    );
    assert_eq!(
        depth_ws_url(urls.ws(false), "BTCUSDT"),
        "wss://sstream.asterdex-testnet.com/ws/btcusdt@depth@100ms"
    );
    let live = crate::trades::spec(Environment::Live).urls;
    assert_eq!(
        kline_ws_url(live.ws(true), "BTCUSDT", "1m"),
        "wss://fstream.asterdex.com/ws/btcusdt@kline_1m"
    );
}

/// The pairing PREDICATE (mark-slot semantics, W2-T4): only a `.P` perp pairs a mark stream,
/// and only while the knob is on. Spot has no venue mark price at all.
#[test]
fn only_perps_pair_a_mark_stream_and_only_while_the_knob_is_on() {
    assert!(super::should_pair_mark("BTCUSDT.P", true));
    assert!(!super::should_pair_mark("BTCUSDT", true), "spot has no mark price");
    assert!(!super::should_pair_mark("BTCUSDT.P", false), "a `mark_streams = 0` row suppresses");
}

/// SPAWN side, network-free: `pair_mark_stream` is the production path `try_spawn` calls (only
/// `body` differs here). A perp spawns a companion `@markPrice@1s` stream (Binance-grammar
/// `mark_main`, reused from the family); unsubscribing the bars id stops BOTH. Aster's mark
/// stream is default-OFF, so this OPTS IN explicitly (the production opt-in is
/// a `venue.aster.mark_streams = 1` row) to exercise the spawn mechanics.
#[test]
fn an_opted_in_perp_bars_subscription_spawns_and_then_stops_its_mark_stream() {
    let sink = Arc::new(RecordingSink::default());
    let mut feeds = Feeds::new(sink, || {});
    feeds.mark_streams = true; // opt in (Aster ships default-OFF)
    let bars_id = feeds.spawn_with("BTCUSDT.P", "1m", fake_feed_body).expect("spawn ok");
    let other_id = feeds.spawn_with("ETHUSDT", "1m", fake_feed_body).expect("spawn ok");
    feeds.pair_mark_stream(bars_id, "BTCUSDT.P", fake_feed_body);
    assert_eq!(feeds.subs.len(), 3, "bars + mark + the unrelated feed");

    feeds.unsubscribe(bars_id);
    assert_eq!(feeds.subs.len(), 1, "bars AND mark stopped together");
    assert!(feeds.subs.contains_key(&other_id), "unrelated subscriptions keep running");
    assert!(feeds.mark_pairings.is_empty(), "the pairing is consumed");
    feeds.shutdown();
}

#[test]
fn a_spot_bars_subscription_spawns_no_mark_stream() {
    let sink = Arc::new(RecordingSink::default());
    let mut feeds = Feeds::new(sink, || {});
    feeds.mark_streams = true; // even opted in, spot has no mark price
    let bars_id = feeds.spawn_with("BTCUSDT", "1m", fake_feed_body).expect("spawn ok");
    feeds.pair_mark_stream(bars_id, "BTCUSDT", fake_feed_body);
    assert_eq!(feeds.subs.len(), 1, "no companion stream for spot");
    feeds.shutdown();
}

/// Aster ships default-OFF: a freshly constructed `Feeds` (no `venue.aster.mark_streams = 1` row)
/// spawns NO mark stream even for a perp — the unverified-wire safety default, pinned.
#[test]
fn aster_defaults_to_no_mark_stream_until_opted_in() {
    let sink = Arc::new(RecordingSink::default());
    let mut feeds = Feeds::new(sink, || {});
    assert!(!feeds.mark_streams, "Aster's mark stream is OFF by default");
    let bars_id = feeds.spawn_with("BTCUSDT.P", "1m", fake_feed_body).expect("spawn ok");
    feeds.pair_mark_stream(bars_id, "BTCUSDT.P", fake_feed_body);
    assert_eq!(feeds.subs.len(), 1, "default-off -> no mark stream even for a perp");
    feeds.shutdown();
}

/// The default this feed runs and the one `config show` prints are ONE fact: the charter constant
/// is `venue.aster.mark_streams`'s declared default.
#[test]
fn the_charter_default_is_the_declared_fields() {
    let declared = vike_model::venues::venue_fields::venue_field("aster", "mark_streams")
        .expect("declared")
        .default;
    assert_eq!(super::MARK_STREAM_DEFAULT_ON, declared == "1", "declared {declared:?}");
}

/// Decision 0095: the `mark_streams` row decides, and no row keeps the charter default — OFF here,
/// because Aster's mark wire is unverified.
#[test]
fn the_mark_streams_row_decides_and_no_row_keeps_the_charter_default() {
    let fresh = || Feeds::new(Arc::new(RecordingSink::default()), || {});
    assert!(!fresh().mark_streams, "charter default: OFF");
    assert!(fresh().with_mark_streams(Some("1")).mark_streams);
    assert!(!fresh().with_mark_streams(Some("0")).mark_streams);
    assert!(!fresh().with_mark_streams(None).mark_streams);
}

/// The knob off (a `mark_streams = 0` row, or the default-off state) suppresses the spawn even for
/// a perp — the knob's whole job, pinned independently of the resolved default.
#[test]
fn the_mark_streams_knob_off_suppresses_the_perp_spawn() {
    let sink = Arc::new(RecordingSink::default());
    let mut feeds = Feeds::new(sink, || {});
    feeds.mark_streams = false;
    let bars_id = feeds.spawn_with("BTCUSDT.P", "1m", fake_feed_body).expect("spawn ok");
    feeds.pair_mark_stream(bars_id, "BTCUSDT.P", fake_feed_body);
    assert_eq!(feeds.subs.len(), 1, "knob off -> no mark stream even for a perp");
    feeds.shutdown();
}

/// Per-symbol dedupe: a 1m AND a 5m chart on the same perp share ONE mark socket (opted in).
#[test]
fn two_intervals_on_one_perp_share_a_single_mark_stream() {
    let sink = Arc::new(RecordingSink::default());
    let mut feeds = Feeds::new(sink, || {});
    feeds.mark_streams = true; // opt in (Aster ships default-OFF)
    let bars_1m = feeds.spawn_with("BTCUSDT.P", "1m", fake_feed_body).expect("spawn ok");
    feeds.pair_mark_stream(bars_1m, "BTCUSDT.P", fake_feed_body);
    let bars_5m = feeds.spawn_with("BTCUSDT.P", "5m", fake_feed_body).expect("spawn ok");
    feeds.pair_mark_stream(bars_5m, "BTCUSDT.P", fake_feed_body);
    assert_eq!(feeds.subs.len(), 3, "two bars feeds but only ONE mark stream");

    feeds.unsubscribe(bars_1m);
    assert_eq!(feeds.subs.len(), 2, "the mark stream the 5m chart still needs stays up");
    feeds.unsubscribe(bars_5m);
    assert!(feeds.subs.is_empty(), "the last release stops the mark stream too");
    feeds.shutdown();
}
