//! LIVE OKX trades-feed smoke (network, NO creds — public `trades` stream):
//!     cargo test -p vike-okx --test integration -- --ignored --nocapture okx_trades_feed_smoke::
//!
//! The DEFINITIVE resolution of the `VenueCapabilities.live_data.trades` drift: `vike_model::OKX` declares
//! `live_data.trades` while the engine over `market_feed::DataClientSpecOkx` implements a real
//! [`vike_data::DataClient::subscribe_trades`] (the trades lane's `market_feed::trades::trades_plan`). Either the cap is a
//! lie or the code is dead — a live subscription settles it. This exercises the EXACT path the cap
//! documents: `DataClient::subscribe_trades` (the engine over `DataClientSpecOkx`) → the shared `run_market` driver → the `trades` WS
//! channel → [`DataClientSink::trade`] — and asserts ≥1 real [`TickTrade`] lands.
//!
//! Distinct from `okx_market_data_smoke.rs`, which proves the SEPARATE `feed/tick/mod.rs` HFT
//! tick-track (`spawn_okx_feed_tick` over a `TickSender`); the `VenueCapabilities.live_data` row is about
//! the `vike_data::DataClient` (the engine over `market_feed::DataClientSpecOkx`) seam specifically. Modeled on `vike-aster`'s
//! `aster_market_data_smoke.rs` (a `RecordingSink` counting callbacks straight off the client, no
//! mounted core). Keyless: OKX's `trades` is a public stream, so this needs no demo creds.

use std::sync::Arc;
use std::time::{Duration, Instant};

use vike_data::{DataClient, DataClientSink};

/// `BTCUSDT.P`, the USER name (decision 0290) of `BTC-USDT-SWAP` — the liquid keyless `trades` channel OKX's exec
/// client also trades, so a real print lands well inside the deadline. The lane subscribes its kept row's instId.
const SYMBOL: &str = "BTCUSDT.P";

// The shared capturing sink (testing-arch Phase 4d): the trade count and the first-few sample
// both come off its typed `trades()` accessor; the positivity invariant the old inline sink
// asserted per callback is checked over the recorded trades once delivery is proven.
use vike_data::RecordingSink;

#[test]
#[ignore = "network — public OKX trades stream, run manually (see module doc)"]
fn okx_trades_feed_delivers_ticks() {
    vike_log::test_init();
    vike_okx::instrument::kept_list_for_tests();

    let sink = Arc::new(RecordingSink::default());
    let spec =
        vike_okx::feed::DataClientSpecOkx::new(&vike_venues::venue_urls::VenueUrls::seed("okx"))
            .expect("the seed's okx data rows");
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
            tracing::info!(target: "vike_okx", count = n, "okx trades ticks observed");
            for t in &sample {
                eprintln!("okx trade: px={} size={} ts={}", t.price, t.size, t.timestamp);
            }
            break;
        }
        assert!(
            Instant::now() < deadline,
            "no trades tick for {SYMBOL} after 25s — DataClient::subscribe_trades dead, \
             or market/stream down?"
        );
        std::thread::sleep(Duration::from_millis(200));
    }

    feeds.unsubscribe(trades_id);
    feeds.shutdown();
    tracing::info!(target: "vike_okx", "okx trades-feed smoke: clean shutdown");
}
