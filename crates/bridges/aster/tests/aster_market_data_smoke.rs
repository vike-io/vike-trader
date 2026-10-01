//! LIVE Aster market-data smoke (network, NO creds — public streams):
//!     cargo test -p vike-aster --test aster_market_data_smoke -- --ignored --nocapture
//!
//! Proves `market_feed::Feeds` — the [`vike_data::DataClient`] seam this crate ships in Task 10
//! — end to end against the REAL Aster testnet WS: `subscribe_bars`/`subscribe_trades`/
//! `subscribe_depth` each land at least one callback on a `RecordingSink`.
//!
//! **Why this isn't shaped exactly like `binance_market_data_smoke.rs`:** that test exercises
//! binance's separate `market_data.rs` HFT tick-track (`spawn_binance_market_data` over a
//! `TickSender`, proven through a mounted `Strategy`'s `on_quote_tick`/`on_trade_tick`/
//! `on_order_book`) — but this crate's `market_data.rs` is a LATER SDD task (Task 11) and does not
//! exist yet. This smoke instead targets the seam Task 10 actually ships
//! (`market_feed::Feeds`/`vike_data::DataClient`), mirroring the shape of
//! `vike-polymarket`'s `tests/market_ticks_live_smoke.rs::market_ws_delivers_book_and_quote`: a
//! `RecordingSink` counting callbacks straight off `Feeds`, no mounted core needed.
//!
//! Self-skips (prints + returns, does not fail) if the Aster testnet host is unreachable — the
//! testnet is a far less battle-tested endpoint than Binance's public infra, so a DNS/connect
//! failure here reads as "environment can't reach it right now", not a real regression.

use std::net::{TcpStream, ToSocketAddrs};
use std::sync::Arc;
use std::time::{Duration, Instant};

use vike_data::{DataClient, LiveDataSink};

const SYMBOL: &str = "BTCUSDT";
/// Aster futures-stream testnet host:port for the reachability probe (bare TCP/DNS connect only
/// — no TLS/WS handshake).
const PROBE_HOST: &str = "fstream.asterdex-testnet.com:443";

// The shared capturing sink (testing-arch Phase 4d): lane-liveness counts come off its typed
// accessors — `bars` counts non-empty seeds + every close/forming delivery, `depth` counts
// non-empty `l2_snapshot`s (it has a no-op default in `LiveDataSink`; the shared sink records
// it). The trade-positivity invariant the old inline sink asserted per callback is checked over
// the recorded trades after the wait loop.
use vike_data::RecordingSink;

/// Cheap DNS+TCP reachability probe (no TLS/WS handshake) — lets this `#[ignore]`d smoke self-skip
/// instead of hanging/failing when the testnet host can't be resolved or refuses the connection.
fn testnet_reachable() -> bool {
    match PROBE_HOST.to_socket_addrs() {
        Ok(mut addrs) => addrs
            .next()
            .map(|addr| TcpStream::connect_timeout(&addr, Duration::from_secs(5)).is_ok())
            .unwrap_or(false),
        Err(_) => false,
    }
}

#[test]
#[ignore = "network — public Aster testnet streams, run manually (see module doc)"]
fn aster_live_ticks_reach_the_sink() {
    vike_log::test_init();
    if !testnet_reachable() {
        eprintln!(
            "aster testnet ({PROBE_HOST}) unreachable — skipping aster_live_ticks_reach_the_sink"
        );
        return;
    }

    let sink = Arc::new(RecordingSink::default());
    let mut feeds =
        vike_aster::market_feed::Feeds::new(Arc::clone(&sink) as Arc<dyn LiveDataSink>, || {});

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
            "lanes idle after 30s (bars={bars}, trades={trades}, depth={depth}) — testnet market closed or stream down?"
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
