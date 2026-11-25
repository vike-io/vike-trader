//! LIVE Bybit market-data smoke (network, NO creds — public streams):
//!     cargo test -p vike-bybit --test bybit_market_data_smoke -- --ignored --nocapture
//!
//! Same proof as the Binance market-data smoke, for Bybit: real public WS → decoders →
//! TickSender → core → a mounted strategy's tick handlers firing. Also verifies the Bybit WS
//! message formats against the live stream. Split out of the old shared venue-adapter crate's
//! former `okx_bybit_market_data_smoke.rs` (shared file, one test fn per venue) when bybit moved
//! into its own crate — crate-reorg Phase 3, PR E; OKX's twin later moved to its own
//! `okx_market_data_smoke.rs` in `crates/bridges/okx` (PR G).

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use vike_core::{CoreConfig, CoreHandle, LiveBroker, Mount, spawn_core};
use vike_execution::testing::RecordingClient;
use vike_execution::{Account, BalanceMode, Engine, RiskGate};
use vike_marketdata::{L2Book, L2BookLevel, TickQuote, TickTrade};
use vike_model::RiskLimits;
use vike_model::Strategy;

#[derive(Default)]
struct TickCounter {
    quotes: Arc<AtomicUsize>,
    trades: Arc<AtomicUsize>,
    books: Arc<AtomicUsize>,
}

impl Strategy<LiveBroker> for TickCounter {
    fn on_quote_tick(&mut self, _b: &mut LiveBroker, q: &TickQuote) {
        assert!(q.ask >= q.bid, "crossed quote {}/{}", q.bid, q.ask);
        // A66: the pump stamps the machine RECEIVE time (`send_quote`); 0 is "not stamped", and a stamp far
        // from this machine's clock is the wrong clock. The quote is derived from the book, so `ts` is 0.
        assert!(
            q.local_timestamp > 0 && (vike_model::now_ms() - q.local_timestamp).abs() < 60_000,
            "a quote must leave the pump stamped with its receive time: {q:?}"
        );
        self.quotes.fetch_add(1, Ordering::Relaxed);
    }
    fn on_trade_tick(&mut self, _b: &mut LiveBroker, t: &TickTrade) {
        assert!(t.price > 0.0 && t.size > 0.0);
        self.trades.fetch_add(1, Ordering::Relaxed);
    }
    fn on_order_book(&mut self, _b: &mut LiveBroker, book: &L2Book) {
        if let (Some(L2BookLevel { price: bid, .. }), Some(L2BookLevel { price: ask, .. })) =
            (book.best_bid(), book.best_ask())
        {
            assert!(bid < ask, "crossed L2 top {bid}/{ask}");
        }
        self.books.fetch_add(1, Ordering::Relaxed);
    }
}

/// Start a TickCounter on (venue, symbol) and return the core handle + the three counters.
fn core_with_counter(
    venue: &str,
    symbol: &str,
) -> (CoreHandle, Arc<AtomicUsize>, Arc<AtomicUsize>, Arc<AtomicUsize>) {
    let (q, t, b) = (
        Arc::<AtomicUsize>::default(),
        Arc::<AtomicUsize>::default(),
        Arc::<AtomicUsize>::default(),
    );
    let engine = Engine::new(
        Account::new(1.0, venue, None, BalanceMode::Delta),
        RiskGate::new(RiskLimits::new()),
        RecordingClient::default(),
        venue,
        symbol,
    );
    let config = CoreConfig {
        strategy: Some(Mount {
            order_client_id_code: None,
            account: None,
            symbols: Vec::new(),
            controller_id: None,
            underlying_symbol: None,
            venue: venue.into(),
            symbol: symbol.into(),
            interval: "1m".into(),
            strategy: Box::new(TickCounter {
                quotes: Arc::clone(&q),
                trades: Arc::clone(&t),
                books: Arc::clone(&b),
            }),
        }),
        ..CoreConfig::default()
    };
    (spawn_core(engine, config), q, t, b)
}

fn wait_all_lanes(q: &Arc<AtomicUsize>, t: &Arc<AtomicUsize>, b: &Arc<AtomicUsize>) {
    let counts =
        || (q.load(Ordering::Relaxed), t.load(Ordering::Relaxed), b.load(Ordering::Relaxed));
    let flowing = vike_log::poll::wait_until(Duration::from_secs(20), || {
        let (qn, tn, bn) = counts();
        qn > 0 && tn > 0 && bn > 0
    });
    let (qn, tn, bn) = counts();
    assert!(flowing, "lanes idle after 20s (q={qn}, t={tn}, b={bn}) — market/stream down?");
    tracing::info!(target: "vike_bybit", "live ticks: {qn} quotes, {tn} trades, {bn} book updates");
}

#[test]
#[ignore = "network — public Bybit streams, run manually"]
fn bybit_live_ticks_reach_the_strategy() {
    vike_log::test_init();
    use vike_bybit::feed::tick::spawn_bybit_feed_tick;
    // The pump routes the USER name by its kept row (decision 0290): `BTCUSDT.P` is the linear socket's `BTCUSDT`.
    vike_bybit::history::kept_list_for_tests();
    let (handle, q, t, b) = core_with_counter("bybit", "BTCUSDT.P");
    let urls = vike_venues::venue_urls::VenueUrls::seed("bybit"); // the release seed's rows
    let feed = spawn_bybit_feed_tick(&urls, handle.tick_sender(), "BTCUSDT.P", 0.1)
        .expect("the seed holds bybit's public rows");
    wait_all_lanes(&q, &t, &b);
    feed.shutdown();
    handle.shutdown_and_join();
    tracing::info!(target: "vike_bybit", "bybit market-data green: orderbook.50/publicTrade → the tick/L2 handlers");
}
