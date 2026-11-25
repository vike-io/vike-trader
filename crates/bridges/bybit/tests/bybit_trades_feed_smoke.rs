//! LIVE Bybit trades-feed smoke (network, NO creds — public `publicTrade` stream):
//!     cargo test -p vike-bybit --test integration -- --ignored --nocapture bybit_trades_feed_smoke::
//!
//! The DEFINITIVE resolution of the `VenueCapabilities.live_data.trades` drift: `vike_model::BYBIT`
//! declares `live_data.trades` while the engine over `market_feed::DataClientSpecBybit` implements a real
//! [`vike_data::DataClient::subscribe_trades`] (its `trades_plan`). Either the cap is a
//! lie or the code is dead — a live subscription settles it. This exercises the EXACT path the cap
//! documents: `DataClient::subscribe_trades` → the shared `run_market` driver → `publicTrade`
//! WS → [`DataClientSink::trade`] — and asserts ≥1 real [`TickTrade`] lands.
//!
//! Distinct from `bybit_market_data_smoke.rs`, which proves the SEPARATE `feed/tick/mod.rs` HFT
//! tick-track (`spawn_bybit_feed_tick` over a `TickSender`); the `VenueCapabilities.live_data` row is
//! about the `vike_data::DataClient` (`market_feed::DataClientSpecBybit`) seam specifically. Modeled on
//! `vike-aster`'s `aster_market_data_smoke.rs` (a `RecordingSink` counting callbacks straight off
//! the client, no mounted core). Keyless: `publicTrade` is a public stream, so this needs no demo
//! creds even though a spot-only key would still work.

use std::sync::Arc;
use std::time::{Duration, Instant};

use vike_data::{DataClient, DataClientSink};

/// `BTCUSDT` spot — the most liquid keyless `publicTrade` book on Bybit, so a real print lands in
/// well under the deadline. `subscribe_trades("BTCUSDT")` takes the spot lane (`is_perp = false`),
/// the byte-identical-to-pre-perp trades path.
const SYMBOL: &str = "BTCUSDT";

// The shared capturing sink (testing-arch Phase 4d): the trade count and the first-few sample
// both come off its typed `trades()` accessor; the positivity invariant the old inline sink
// asserted per callback is checked over the recorded trades once delivery is proven.
use vike_data::RecordingSink;

#[test]
#[ignore = "network — public Bybit publicTrade stream, run manually (see module doc)"]
fn bybit_trades_feed_delivers_ticks() {
    vike_log::test_init();
    // The lane routes the USER name by its kept row, as a process that keeps the list does (decision 0290).
    vike_bybit::history::kept_list_for_tests();

    let sink = Arc::new(RecordingSink::default());
    let urls = vike_venues::venue_urls::VenueUrls::seed("bybit"); // the release seed's rows
    let spec = vike_bybit::feed::DataClientSpecBybit::new(&urls)
        .expect("the seed holds bybit's public rows");
    let mut feeds = vike_bridge_core::data_client::ws::DataClientWebsocket::new(
        spec,
        Arc::clone(&sink) as Arc<dyn DataClientSink>,
        || {},
    );
    let trades_id = feeds.subscribe_trades(SYMBOL).expect("subscribe_trades ok");

    let deadline = Instant::now() + Duration::from_secs(25);
    loop {
        let n = sink.trades().len();
        if n > 0 {
            let trades = sink.trades();
            for (_, _, t) in &trades {
                assert!(t.price > 0.0 && t.size > 0.0, "non-positive trade price/size");
            }
            let sample: Vec<_> = trades.into_iter().take(5).map(|(_, _, t)| t).collect();
            tracing::info!(target: "vike_bybit", count = n, "bybit publicTrade ticks observed");
            for t in &sample {
                eprintln!("bybit trade: px={} size={} ts={}", t.price, t.size, t.timestamp);
            }
            break;
        }
        assert!(
            Instant::now() < deadline,
            "no publicTrade tick for {SYMBOL} after 25s — DataClient::subscribe_trades dead, \
             or market/stream down?"
        );
        std::thread::sleep(Duration::from_millis(200));
    }

    feeds.unsubscribe(trades_id);
    feeds.shutdown();
    tracing::info!(target: "vike_bybit", "bybit trades-feed smoke: clean shutdown");
}
