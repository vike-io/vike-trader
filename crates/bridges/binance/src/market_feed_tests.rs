use super::{FeedCtx, Feeds};
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::Duration;
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
    assert_eq!(feeds.registry.len(), 1, "only the unsubscribed stream is removed");
    assert!(feeds.registry.contains(id2), "the other subscription keeps running");

    feeds.shutdown();
    assert!(feeds.registry.is_empty());
}

#[test]
fn unsubscribe_of_an_unknown_id_is_a_no_op() {
    let sink = Arc::new(RecordingSink::default());
    let mut feeds = Feeds::new(sink, || {});
    let id = feeds.spawn_with("BTCUSDT", "1m", fake_feed_body).expect("spawn ok");
    feeds.unsubscribe(vike_data::SubscriptionId(id.0 + 100)); // never issued
    assert_eq!(feeds.registry.len(), 1, "unknown id must not disturb the real subscription");
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
    assert!(feeds.registry.is_empty());
}

#[test]
fn quotes_and_book_are_unsupported() {
    // subscribe_trades is deliberately NOT asserted here anymore — Task B2 wired it to a real
    // feed thread (real WS connect + REST warmup), so exercising it from a unit test would
    // attempt a live network connection. Its pure decode/splice logic is covered by
    // trades.rs's own tests; the loop itself is covered by the venue's live-smoke
    // conventions (see the crate's `tests/binance_market_data_smoke.rs` for the pattern).
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

/// Rung-2 guard: the kline + depth WS URLs the shared feed builds for Binance must be exactly
/// the hosts this venue used before the `UrlTable` extraction — spot on `stream.binance.com`,
/// perp on `fstream.binance.com`, depth spot-only.
#[test]
fn binance_kline_and_depth_urls_are_unchanged() {
    use crate::family::market_feed::{depth_ws_url, kline_ws_url};
    let urls = super::BINANCE_URLS;
    assert_eq!(
        kline_ws_url(urls.ws(false), "BTCUSDT", "1m"),
        "wss://stream.binance.com:9443/ws/btcusdt@kline_1m"
    );
    assert_eq!(
        kline_ws_url(urls.ws(true), "BTCUSDT", "1m"),
        "wss://fstream.binance.com/ws/btcusdt@kline_1m"
    );
    assert_eq!(
        depth_ws_url(urls.ws(false), "BTCUSDT"),
        "wss://stream.binance.com:9443/ws/btcusdt@depth@100ms"
    );
}

/// **The depth URL a `.P` symbol produces** — the bug this test exists to prevent from
/// returning. `depth_main` splits the core symbol and picks the host from the flag, so a perp
/// reaches the FUTURES stream with the suffix stripped. Before, both halves were wrong at once:
/// spot host + raw symbol = `stream.binance.com:9443/ws/btcusdt.p@depth@100ms`, a stream no
/// venue resolves, which connects and then silently streams nothing.
#[test]
fn a_perp_depth_subscription_reaches_the_futures_stream() {
    use crate::family::market_feed::depth_ws_url;
    let urls = super::BINANCE_URLS;
    let (sym, is_perp) = vike_catalog::split_perp("BTCUSDT.P");
    assert!(is_perp);
    assert_eq!(
        depth_ws_url(urls.ws(is_perp), sym),
        "wss://fstream.binance.com/ws/btcusdt@depth@100ms",
        "measured live: this stream pushes ~139 frames/15s, the broken one pushed 0"
    );
    // …and a spot symbol is byte-identical to before.
    let (sym, is_perp) = vike_catalog::split_perp("BTCUSDT");
    assert!(!is_perp);
    assert_eq!(
        depth_ws_url(urls.ws(is_perp), sym),
        "wss://stream.binance.com:9443/ws/btcusdt@depth@100ms"
    );
}

/// The REST seed must follow the WS host: a perp seed hits `fapi` + the `v1` futures path.
/// Pairing a spot host with a futures path (or the reverse) 404s, and the DOM lane treats a
/// failed seed as transient and retries it forever — which is exactly how the live bug hid.
#[test]
fn the_depth_seed_url_follows_the_instrument_class() {
    use crate::market_data::{MAINNET_REST, PERP_REST, depth_snapshot_url_for};
    assert_eq!(
        depth_snapshot_url_for(true, "BTCUSDT"),
        format!("{PERP_REST}/fapi/v1/depth?symbol=BTCUSDT&limit=1000")
    );
    assert_eq!(
        depth_snapshot_url_for(false, "BTCUSDT"),
        format!("{MAINNET_REST}/api/v3/depth?symbol=BTCUSDT&limit=1000")
    );
}

/// The pairing PREDICATE (mark-slot semantics, W2-T4): only a `.P` perp pairs a mark stream,
/// and only while the knob is on. Spot has no venue mark price at all.
#[test]
fn only_perps_pair_a_mark_stream_and_only_while_the_knob_is_on() {
    assert!(super::should_pair_mark("BTCUSDT.P", true));
    assert!(!super::should_pair_mark("BTCUSDT", true), "spot has no mark price");
    assert!(!super::should_pair_mark("BTCUSDT.P", false), "a `mark_streams = 0` row suppresses");
    assert!(!super::should_pair_mark("BTCUSDT", false));
}

/// SPAWN side, network-free: `pair_mark_stream` is the production path `try_spawn` calls (only
/// `body` differs here). A perp spawns a second, companion stream; unsubscribing the bars id
/// stops BOTH, while unrelated subscriptions keep running.
#[test]
fn a_perp_bars_subscription_spawns_and_then_stops_its_mark_stream() {
    let sink = Arc::new(RecordingSink::default());
    let mut feeds = Feeds::new(sink, || {});
    let bars_id = feeds.spawn_with("BTCUSDT.P", "1m", fake_feed_body).expect("spawn ok");
    let other_id = feeds.spawn_with("ETHUSDT", "1m", fake_feed_body).expect("spawn ok");
    feeds.pair_mark_stream(bars_id, "BTCUSDT.P", fake_feed_body);
    assert_eq!(feeds.registry.len(), 3, "bars + mark + the unrelated feed");
    assert!(feeds.mark_pairings.mark_id_of("BTCUSDT.P").is_some());

    feeds.unsubscribe(bars_id);
    assert_eq!(feeds.registry.len(), 1, "bars AND mark stopped together");
    assert!(feeds.registry.contains(other_id), "unrelated subscriptions keep running");
    assert!(feeds.mark_pairings.is_empty(), "the pairing is consumed");
    feeds.shutdown();
}

/// A SPOT bars subscription pairs nothing — the mark spawn branch never runs.
#[test]
fn a_spot_bars_subscription_spawns_no_mark_stream() {
    let sink = Arc::new(RecordingSink::default());
    let mut feeds = Feeds::new(sink, || {});
    let bars_id = feeds.spawn_with("BTCUSDT", "1m", fake_feed_body).expect("spawn ok");
    feeds.pair_mark_stream(bars_id, "BTCUSDT", fake_feed_body);
    assert_eq!(feeds.registry.len(), 1, "no companion stream for spot");
    assert!(feeds.mark_pairings.is_empty());
    feeds.shutdown();
}

/// A `mark_streams = 0` row (resolved into `Feeds.mark_streams` by `Feeds::with_mark_streams`)
/// suppresses the spawn even for a perp — the knob's whole job, pinned. Set on the struct directly,
/// so the test exercises the spawn gate alone.
#[test]
fn the_mark_streams_knob_off_suppresses_the_perp_spawn() {
    let sink = Arc::new(RecordingSink::default());
    let mut feeds = Feeds::new(sink, || {});
    feeds.mark_streams = false;
    let bars_id = feeds.spawn_with("BTCUSDT.P", "1m", fake_feed_body).expect("spawn ok");
    feeds.pair_mark_stream(bars_id, "BTCUSDT.P", fake_feed_body);
    assert_eq!(feeds.registry.len(), 1, "knob off -> no mark stream even for a perp");
    assert!(feeds.mark_pairings.is_empty());
    feeds.shutdown();
}

/// The default this feed runs and the one `config show` prints are ONE fact: the charter constant
/// is `venue.binance.mark_streams`'s declared default.
#[test]
fn the_charter_default_is_the_declared_fields() {
    let declared = vike_model::venues::venue_fields::venue_field("binance", "mark_streams")
        .expect("declared")
        .default;
    assert_eq!(super::MARK_STREAM_DEFAULT_ON, declared == "1", "declared {declared:?}");
}

/// Decision 0095: the `mark_streams` row decides, and no row keeps the charter default (ON here).
#[test]
fn the_mark_streams_row_decides_and_no_row_keeps_the_charter_default() {
    let fresh = || Feeds::new(Arc::new(RecordingSink::default()), || {});
    assert!(fresh().mark_streams, "charter default: ON");
    assert!(!fresh().with_mark_streams(Some("0")).mark_streams);
    assert!(fresh().with_mark_streams(Some("1")).mark_streams);
    assert!(fresh().with_mark_streams(None).mark_streams);
}

/// Per-symbol dedupe: a 1m AND a 5m chart on the same perp share ONE mark socket, released
/// only when the last bars subscription goes.
#[test]
fn two_intervals_on_one_perp_share_a_single_mark_stream() {
    let sink = Arc::new(RecordingSink::default());
    let mut feeds = Feeds::new(sink, || {});
    let bars_1m = feeds.spawn_with("BTCUSDT.P", "1m", fake_feed_body).expect("spawn ok");
    feeds.pair_mark_stream(bars_1m, "BTCUSDT.P", fake_feed_body);
    let bars_5m = feeds.spawn_with("BTCUSDT.P", "5m", fake_feed_body).expect("spawn ok");
    feeds.pair_mark_stream(bars_5m, "BTCUSDT.P", fake_feed_body);
    assert_eq!(feeds.registry.len(), 3, "two bars feeds but only ONE mark stream");

    feeds.unsubscribe(bars_1m);
    assert_eq!(feeds.registry.len(), 2, "the mark stream the 5m chart still needs stays up");
    feeds.unsubscribe(bars_5m);
    assert!(feeds.registry.is_empty(), "the last release stops the mark stream too");
    feeds.shutdown();
}
