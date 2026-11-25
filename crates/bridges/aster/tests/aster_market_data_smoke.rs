//! LIVE Aster market-data smoke (network, NO creds — public streams):
//!     cargo test -p vike-aster --test integration -- --ignored --nocapture aster_market_data_smoke::
//!
//! Proves aster's data client (`market_feed::DataClientSpecAster` on the one engine) — the
//! [`vike_data::DataClient`] seam this crate ships in Task 10
//! — end to end against the REAL Aster public WS the gatewayhub's feed dials: the release's
//! `venue_url` rows (`crates/vike-venues/src/venue_urls.rs`'s `VENUE_URL_SEED`) read at `live`
//! `data`, i.e. MAINNET public streams, keyless. (Until those rows existed this smoke read the
//! testnet's public streams; the seed holds no aster `demo` data row, so a `demo` data ask reads the
//! live rows too.) `subscribe_bars`/`subscribe_trades`/`subscribe_depth` each land at least one
//! callback on a `RecordingSink`.
//!
//! **Why this isn't shaped exactly like `binance_market_data_smoke.rs`:** that test exercises
//! binance's separate `feed/tick/mod.rs` HFT tick-track (`spawn_binance_feed_tick` over a
//! `TickSender`, proven through a mounted `Strategy`'s `on_quote_tick`/`on_trade_tick`/
//! `on_order_book`) — but this crate's `feed/tick/mod.rs` is a LATER SDD task (Task 11) and does not
//! exist yet. This smoke instead targets the seam Task 10 actually ships
//! (`vike_data::DataClient`), mirroring the shape of
//! `vike-polymarket`'s `tests/polymarket_smokes/market_ticks_live_smoke.rs::market_websocket_delivers_book_and_quote`: a
//! `RecordingSink` counting callbacks straight off the client, no mounted core needed.
//!
//! Self-skips (prints + returns, does not fail) if the Aster stream host is unreachable, so a
//! DNS/connect failure here reads as "environment can't reach it right now", not a real regression.

use std::net::{TcpStream, ToSocketAddrs};
use std::sync::Arc;
use std::time::{Duration, Instant};

use vike_aster::transport::urls::{AsterUrls, VENUE};
use vike_data::{DataClient, DataClientSink};
use vike_model::AccountTier;
use vike_venues::venue_urls::VenueUrls;

const SYMBOL: &str = "BTCUSDT";

/// The futures-stream host:port the feed below dials, for the reachability probe (bare TCP/DNS
/// connect only — no TLS/WS handshake): the seed's `live` `data` `ws` `usdm` row, port 443.
fn probe_host() -> String {
    let ws = AsterUrls::data(&VenueUrls::seed(VENUE), AccountTier::Live)
        .expect("the seed holds aster's data rows")
        .fapi_websocket;
    format!("{}:443", ws.trim_start_matches("wss://"))
}

// The shared capturing sink (testing-arch Phase 4d): lane-liveness counts come off its typed
// accessors — `bars` counts non-empty seeds + every close/forming delivery, `depth` counts
// non-empty `l2_snapshot`s (it has a no-op default in `DataClientSink`; the shared sink records
// it). The trade-positivity invariant the old inline sink asserted per callback is checked over
// the recorded trades after the wait loop.
use vike_data::RecordingSink;

/// Cheap DNS+TCP reachability probe (no TLS/WS handshake) — lets this `#[ignore]`d smoke self-skip
/// instead of hanging/failing when the stream host can't be resolved or refuses the connection.
fn stream_host_reachable(probe: &str) -> bool {
    match probe.to_socket_addrs() {
        Ok(mut addrs) => addrs
            .next()
            .map(|addr| TcpStream::connect_timeout(&addr, Duration::from_secs(5)).is_ok())
            .unwrap_or(false),
        Err(_) => false,
    }
}

#[test]
#[ignore = "network — public Aster streams (the live data rows), run manually (see module doc)"]
fn aster_live_ticks_reach_the_sink() {
    vike_log::test_init();
    let probe = probe_host();
    if !stream_host_reachable(&probe) {
        eprintln!(
            "aster stream host ({probe}) unreachable — skipping aster_live_ticks_reach_the_sink"
        );
        return;
    }

    let sink = Arc::new(RecordingSink::default());
    let spec =
        vike_aster::feed::DataClientSpecAster::new(&VenueUrls::seed(VENUE), AccountTier::Live)
            .expect("the seed holds aster's data rows");
    let mut feeds = vike_bridge_core::data_client::ws::DataClientWebsocket::new(
        spec,
        Arc::clone(&sink) as Arc<dyn DataClientSink>,
        || {},
    );

    let bars_id = feeds.subscribe_bars(SYMBOL, "1m").expect("subscribe_bars ok");
    let trades_id = feeds.subscribe_trades(SYMBOL).expect("subscribe_trades ok");
    let depth_id = feeds.subscribe_depth(SYMBOL).expect("subscribe_depth ok");

    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        let (bars, trades, depth) = (
            sink.seeded_bars().iter().filter(|(_, _, _, b)| !b.is_empty()).count()
                + sink.closed_bars().len()
                + sink.forming_bars().len(),
            sink.trades().len(),
            sink.l2_snapshots()
                .iter()
                .filter(|(_, _, _, b, a, _)| !b.is_empty() || !a.is_empty())
                .count(),
        );
        if bars > 0 && trades > 0 && depth > 0 {
            tracing::info!(target: "vike_aster", bars, trades, depth, "live ticks observed");
            break;
        }
        assert!(
            Instant::now() < deadline,
            "lanes idle after 30s (bars={bars}, trades={trades}, depth={depth}) — market closed or stream down?"
        );
        std::thread::sleep(Duration::from_millis(200));
    }

    for (_, _, t) in sink.trades() {
        assert!(t.price > 0.0 && t.size > 0.0, "non-positive trade price/size");
    }

    feeds.unsubscribe(bars_id);
    feeds.unsubscribe(trades_id);
    feeds.unsubscribe(depth_id);
    feeds.shutdown();
    tracing::info!(target: "vike_aster", "aster market-data smoke: clean shutdown");
}
