//! LIVE Bybit trades-feed smoke (network, NO creds — public `publicTrade` stream):
//!     cargo test -p vike-bybit --test bybit_trades_feed_smoke -- --ignored --nocapture
//!
//! The DEFINITIVE resolution of the `VenueCaps.live_data.trades` drift: `vike_model::BYBIT`
//! declares `live_data.trades` while `market_feed::Feeds` implements a real
//! [`vike_data::DataClient::subscribe_trades`] (`spawn_with(.., trades_main)`). Either the cap is a
//! lie or the code is dead — a live subscription settles it. This exercises the EXACT path the cap
//! documents: [`Feeds::subscribe_trades`] → the shared `run_market_feed` driver → `publicTrade`
//! WS → [`LiveDataSink::trade`] — and asserts ≥1 real [`TradeTick`] lands.
//!
//! Distinct from `bybit_market_data_smoke.rs`, which proves the SEPARATE `market_data.rs` HFT
//! tick-track (`spawn_bybit_market_data` over a `TickSender`); the `VenueCaps.live_data` row is
//! about the `vike_data::DataClient` (`market_feed::Feeds`) seam specifically. Modeled on
//! `vike-aster`'s `aster_market_data_smoke.rs` (a `RecordingSink` counting callbacks straight off
//! `Feeds`, no mounted core). Keyless: `publicTrade` is a public stream, so this needs no demo
//! creds even though a spot-only key would still work.

use std::sync::Arc;
use std::time::{Duration, Instant};

use vike_data::{DataClient, LiveDataSink};

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

    let sink = Arc::new(RecordingSink::default());
    let mut feeds =
        vike_bybit::market_feed::Feeds::new(Arc::clone(&sink) as Arc<dyn LiveDataSink>, || {});
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
                eprintln!("bybit trade: px={} size={} ts={}", t.price, t.size, t.ts);
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
