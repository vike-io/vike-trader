use super::{FeedCtx, Feeds};
use std::sync::Arc;
use vike_bridge_core::market_feed_pins::{self, FeedPins, MarkPins};

// The shared capturing sink (testing-arch Phase 4d) — replaces the inline formatted-string
// RecordingSink copy that used to live here (same canonical `calls()` line forms).
use vike_data::RecordingSink;

/// A network-free stand-in for the shared `feed_main`.
fn fake_feed_body(_symbol: String, _interval: String, ctx: FeedCtx) {
    market_feed_pins::idle_until_stopped(&ctx.stop);
}

fn fresh() -> Feeds {
    Feeds::new(Arc::new(RecordingSink::default()), || {})
}

const PINS: FeedPins<Feeds> = FeedPins {
    spawn: |feeds, symbol, interval| {
        feeds.spawn_with(symbol, interval, fake_feed_body).expect("spawn ok")
    },
    registry: |feeds| &feeds.registry,
    intervals: ["1m", "5m"],
    mark: Some(MarkPins {
        pair: |feeds, bars_id, symbol| feeds.pair_mark_stream(bars_id, symbol, fake_feed_body),
        pairings: |feeds| &feeds.mark_pairings,
    }),
};

#[test]
fn subscribe_returns_distinct_ids() {
    PINS.distinct_ids(fresh(), ["BTCUSDT", "ETHUSDT"]);
}

#[test]
fn unsubscribe_stops_only_that_one_feed_others_keep_running() {
    PINS.unsubscribe_keeps_the_others(fresh(), ["BTCUSDT", "ETHUSDT"]);
}

#[test]
fn unsubscribe_of_an_unknown_id_is_a_no_op() {
    PINS.unknown_unsubscribe_is_a_no_op(fresh(), "BTCUSDT");
}

#[test]
fn shutdown_joins_every_feed() {
    PINS.shutdown_joins_every_feed(fresh(), &["BTCUSDT", "ETHUSDT", "SOLUSDT"]);
}

#[test]
fn quotes_and_book_are_unsupported() {
    // subscribe_trades is deliberately NOT asserted here anymore — Task B2 wired it to a real
    // feed thread (real WS connect + REST warmup), so exercising it from a unit test would
    // attempt a live network connection. Its pure decode/splice logic is covered by
    // trades.rs's own tests; the loop itself is covered by the venue's live-smoke
    // conventions (see the crate's `tests/binance_market_data_smoke.rs` for the pattern).
    market_feed_pins::refuses_quotes_and_book(fresh(), "BTCUSDT");
}

/// Rung-2 guard: the kline + depth WS URLs the shared feed builds for Binance are the hosts this
/// venue used before the `UrlTable` extraction — spot on `stream.binance.com`, perp on
/// `fstream.binance.com`, depth spot-only — and since decision 0251 a perp kline rides the futures
/// MARKET path (the legacy one no longer delivers it).
#[test]
fn binance_kline_and_depth_urls_follow_the_routed_layout() {
    use crate::family::market_feed::{depth_ws_url, kline_ws_url};
    let urls = super::BINANCE_URLS;
    assert_eq!(
        kline_ws_url(&urls.market_ws(false), "BTCUSDT", "1m"),
        "wss://stream.binance.com:9443/ws/btcusdt@kline_1m"
    );
    assert_eq!(
        kline_ws_url(&urls.market_ws(true), "BTCUSDT", "1m"),
        "wss://fstream.binance.com/market/ws/btcusdt@kline_1m"
    );
    assert_eq!(
        depth_ws_url(&urls.public_ws(false), "BTCUSDT"),
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
        depth_ws_url(&urls.public_ws(is_perp), sym),
        "wss://fstream.binance.com/public/ws/btcusdt@depth@100ms",
        "measured live: this stream pushes ~139 frames/15s, the broken one pushed 0"
    );
    // …and a spot symbol is byte-identical to before.
    let (sym, is_perp) = vike_catalog::split_perp("BTCUSDT");
    assert!(!is_perp);
    assert_eq!(
        depth_ws_url(&urls.public_ws(is_perp), sym),
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
    PINS.bars_spawn_and_stop_their_mark(fresh(), ["BTCUSDT.P", "ETHUSDT"]);
}

/// A SPOT bars subscription pairs nothing — the mark spawn branch never runs.
#[test]
fn a_spot_bars_subscription_spawns_no_mark_stream() {
    PINS.pairs_no_mark(fresh(), "BTCUSDT", "no companion stream for spot");
}

/// A `mark_streams = 0` row (resolved into `Feeds.mark_streams` by `Feeds::with_mark_streams`)
/// suppresses the spawn even for a perp — the knob's whole job, pinned. Set on the struct directly,
/// so the test exercises the spawn gate alone.
#[test]
fn the_mark_streams_knob_off_suppresses_the_perp_spawn() {
    let mut feeds = fresh();
    feeds.mark_streams = false;
    PINS.pairs_no_mark(feeds, "BTCUSDT.P", "knob off -> no mark stream even for a perp");
}

/// The default this feed runs and the one `config show` prints are ONE fact: the charter constant
/// is `venue.binance.mark_streams`'s declared default.
#[test]
fn the_charter_default_is_the_declared_fields() {
    market_feed_pins::charter_default_is_declared("binance", super::MARK_STREAM_DEFAULT_ON);
}

/// Decision 0095: the `mark_streams` row decides, and no row keeps the charter default (ON here).
#[test]
fn the_mark_streams_row_decides_and_no_row_keeps_the_charter_default() {
    market_feed_pins::mark_streams_row_decides(
        fresh,
        Feeds::with_mark_streams,
        |f| f.mark_streams,
        true,
    );
}

/// Per-symbol dedupe: a 1m AND a 5m chart on the same perp share ONE mark socket, released
/// only when the last bars subscription goes.
#[test]
fn two_intervals_on_one_perp_share_a_single_mark_stream() {
    PINS.two_intervals_share_one_mark(fresh(), "BTCUSDT.P");
}
