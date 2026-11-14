//! Binance's half of the data-client engine: its rows, its plans and its perp naming. The lifecycle
//! every lane shares (spawn, unsubscribe, shutdown, the mark pairing, the refusal order, the thread
//! rule) is the engine's own suite (`crates/vike-bridge-core/src/data_client/websocket_tests.rs`).

use super::{DataClientSpecBinance, MARK_STREAM_DEFAULT_ON};
use std::sync::Arc;
use vike_bridge_core::data_client::ws::{
    DataClientLane, DataClientLanePlan, DataClientSpec, DataClientWebsocket,
};
use vike_bridge_core::market_feed_pins;

// The shared capturing sink (testing-arch Phase 4d) — replaces the inline formatted-string
// RecordingSink copy that used to live here (same canonical `calls()` line forms).
use vike_data::RecordingSink;

fn spec() -> DataClientSpecBinance {
    DataClientSpecBinance::new(&vike_venues::venue_urls::VenueUrls::seed("binance"))
        .expect("the seed holds every row")
}

/// The URL a `Session` or `Depth` plan dials (`None` for a lane that dials for itself).
fn plan_url(lane: DataClientLane, symbol: &str, interval: &str) -> Option<String> {
    match spec().plan(lane, symbol, interval).expect("binance plans this lane") {
        DataClientLanePlan::Session { url, .. } | DataClientLanePlan::Depth { url, .. } => {
            Some(url)
        }
        DataClientLanePlan::Own(_) | DataClientLanePlan::SessionOn { .. } => None,
    }
}

/// A feed over a table without binance's rows is refused at construction, naming the row: nothing is
/// dialled on a guessed host.
#[test]
fn a_feed_without_its_address_rows_is_refused() {
    let empty = vike_venues::venue_urls::VenueUrls::default();
    let why = DataClientSpecBinance::new(&empty).err().expect("no row, no feed").to_string();
    assert!(why.contains("live data ws product=spot"), "{why}");
}

#[test]
fn quotes_and_book_are_unsupported() {
    // subscribe_trades is deliberately NOT asserted here — it spawns a real feed thread (real WS
    // connect + REST warmup), so exercising it from a unit test would attempt a live network
    // connection. Its pure decode/splice logic is covered by feed/tick/trades.rs's own tests; the loop itself is
    // covered by the offline loopback tests and the venue's live smoke.
    let client = DataClientWebsocket::new(spec(), Arc::new(RecordingSink::default()), || {});
    market_feed_pins::refuses_quotes_and_book(client, "BTCUSDT");
    // …and a plan asked for one anyway says so in words, never a panic.
    for lane in [DataClientLane::Quotes, DataClientLane::Book] {
        let refused = spec().plan(lane, "BTCUSDT", lane.word("")).err().expect("no such lane");
        assert!(refused.starts_with("binance serves no "), "{refused}");
    }
}

/// Rung-2 guard: the kline + depth WS URLs the shared feed builds for Binance are the hosts this
/// venue used before the `UrlTable` extraction — spot on `stream.binance.com`, perp on
/// `fstream.binance.com`, depth spot-only — and since decision 0251 a perp kline rides the futures
/// MARKET path (the legacy one no longer delivers it).
#[test]
fn binance_kline_and_depth_urls_follow_the_routed_layout() {
    use crate::family::feed::{depth_websocket_url, kline_websocket_url};
    let urls = super::seeded_tables().linear;
    assert_eq!(
        kline_websocket_url(&urls.market_websocket(false), "BTCUSDT", "1m"),
        "wss://stream.binance.com:9443/ws/btcusdt@kline_1m"
    );
    assert_eq!(
        kline_websocket_url(&urls.market_websocket(true), "BTCUSDT", "1m"),
        "wss://fstream.binance.com/market/ws/btcusdt@kline_1m"
    );
    assert_eq!(
        depth_websocket_url(&urls.public_websocket(false), "BTCUSDT"),
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
    assert_eq!(
        plan_url(DataClientLane::Depth, "BTCUSDT.P", "depth").as_deref(),
        Some("wss://fstream.binance.com/public/ws/btcusdt@depth@100ms"),
        "measured live: this stream pushes ~139 frames/15s, the broken one pushed 0"
    );
    // …and a spot symbol is byte-identical to before.
    assert_eq!(
        plan_url(DataClientLane::Depth, "BTCUSDT", "depth").as_deref(),
        Some("wss://stream.binance.com:9443/ws/btcusdt@depth@100ms")
    );
}

/// The REST seed must follow the WS host: a perp seed hits `fapi` + the `v1` futures path.
/// Pairing a spot host with a futures path (or the reverse) 404s, and the DOM lane treats a
/// failed seed as transient and retries it forever — which is exactly how the live bug hid.
#[test]
fn the_depth_seed_url_follows_the_instrument_class() {
    use crate::feed::tick::depth_snapshot_url_for;
    let tables = super::seeded_tables();
    assert_eq!(
        depth_snapshot_url_for(&tables, true, "BTCUSDT"),
        "https://fapi.binance.com/fapi/v1/depth?symbol=BTCUSDT&limit=1000"
    );
    assert_eq!(
        depth_snapshot_url_for(&tables, false, "BTCUSDT"),
        "https://api.binance.com/api/v3/depth?symbol=BTCUSDT&limit=1000"
    );
}

/// **A COIN-M subscription's lanes dial `dstream` and seed from `dapi`; every other symbol keeps
/// [`super::BinanceTables::linear`].** The table is read off the family row every lane's plan is
/// built from (`DataClientSpecBinance::family`), and off the plans themselves. The hosts were measured
/// from the dev box 2026-11-08: `dstream.binance.com` answers every one of these streams for
/// `btcusd_perp`, and `fstream` lists no COIN-M symbol, so a COIN-M lane on the USDⓈ-M table connects
/// and hears nothing.
#[test]
fn a_coin_m_subscription_dials_dstream_and_seeds_from_dapi() {
    use crate::family::feed::tick::trades::{agg_trades_websocket_url, trades_rest_url};
    use crate::feed::tick::depth_snapshot_url_for;
    assert_eq!(
        plan_url(DataClientLane::Bars, "BTCUSD_PERP.P", "1m").as_deref(),
        Some("wss://dstream.binance.com/market/ws/btcusd_perp@kline_1m")
    );
    assert_eq!(
        plan_url(DataClientLane::Mark, "BTCUSD_PERP.P", "mark").as_deref(),
        Some("wss://dstream.binance.com/market/ws/btcusd_perp@markPrice@1s")
    );
    assert_eq!(
        plan_url(DataClientLane::Depth, "BTCUSD_PERP.P", "depth").as_deref(),
        Some("wss://dstream.binance.com/public/ws/btcusd_perp@depth@100ms")
    );
    // `@aggTrade` streams on dstream (fapi's was silent on its legacy path), so the trade lane is the
    // aggregate one and its warmup is dapi's aggTrades, in the same id space.
    let urls = spec().family("BTCUSD_PERP.P").urls;
    assert_eq!(
        agg_trades_websocket_url(&urls, "BTCUSD_PERP", true),
        "wss://dstream.binance.com/market/ws/btcusd_perp@aggTrade"
    );
    assert_eq!(
        trades_rest_url(&urls, "BTCUSD_PERP", true, 1000),
        "https://dapi.binance.com/dapi/v1/aggTrades?symbol=BTCUSD_PERP&limit=1000"
    );
    let tables = super::seeded_tables();
    assert_eq!(
        depth_snapshot_url_for(&tables, true, "BTCUSD_PERP"),
        "https://dapi.binance.com/dapi/v1/depth?symbol=BTCUSD_PERP&limit=1000"
    );
    // A COIN-M quarterly is the same book.
    assert_eq!(spec().family("BTCUSD_261225.P").urls, urls);
    // A USDⓈ-M perpetual, a USDⓈ-M quarterly and spot keep the table they had, byte for byte.
    for symbol in ["BTCUSDT.P", "BTCUSDT_261225.P", "BTCUSDT"] {
        assert_eq!(
            spec().family(symbol),
            crate::feed::tick::trades::spec(tables.linear),
            "{symbol}"
        );
    }
    assert_eq!(
        depth_snapshot_url_for(&tables, true, "BTCUSDT"),
        "https://fapi.binance.com/fapi/v1/depth?symbol=BTCUSDT&limit=1000"
    );
}

/// The pairing PREDICATE (mark-slot semantics, W2-T4): only a `.P` perp pairs a mark stream (the
/// engine adds the venue's `mark_streams` row). Spot has no venue mark price at all.
#[test]
fn only_perps_pair_a_mark_stream() {
    assert!(spec().pairs_mark("BTCUSDT.P"));
    assert!(spec().pairs_mark("BTCUSD_PERP.P"));
    assert!(!spec().pairs_mark("BTCUSDT"), "spot has no mark price");
}

/// The default this feed runs and the one `config show` prints are ONE fact: the charter constant
/// is `venue.binance.mark_streams`'s declared default, and the spec hands it to the engine.
#[test]
fn the_charter_default_is_the_declared_fields() {
    market_feed_pins::charter_default_is_declared("binance", MARK_STREAM_DEFAULT_ON);
    assert_eq!(
        <DataClientSpecBinance as DataClientSpec>::MARK_STREAMS_DEFAULT_ON,
        MARK_STREAM_DEFAULT_ON
    );
}
