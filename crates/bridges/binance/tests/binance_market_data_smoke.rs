//! LIVE Binance-spot market-data smoke (network, NO creds — public streams):
//!     cargo test -p vike-binance --test binance_market_data_smoke -- --ignored --nocapture
//!
//! Proves the whole R8 tick/L2 chain end-to-end: real Binance bookTicker/trade/depth WS →
//! decoders → TickSender → the single-writer core → a mounted strategy's on_quote_tick /
//! on_trade_tick / on_order_book actually firing on live data.

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use vike_binance::feed::tick::spawn_binance_feed_tick;
use vike_core::{CoreConfig, LiveBroker, Mount, spawn_core};
use vike_execution::testing::RecordingClient;
use vike_execution::{Account, BalanceMode, Engine, RiskGate};
use vike_marketdata::{L2Book, L2BookLevel, TickQuote, TickTrade};
use vike_model::RiskLimits;
use vike_model::Strategy;

const SYMBOL: &str = "BTCUSDT";

#[derive(Default)]
struct TickCounter {
    quotes: Arc<AtomicUsize>,
    trades: Arc<AtomicUsize>,
    books: Arc<AtomicUsize>,
}

impl Strategy<LiveBroker> for TickCounter {
    fn on_quote_tick(&mut self, _b: &mut LiveBroker, q: &TickQuote) {
        assert!(q.ask >= q.bid, "crossed book: {} / {}", q.bid, q.ask);
        // A66: the pump stamps the machine RECEIVE time (`send_quote`); 0 is "not stamped", and a stamp far
        // from this machine's clock is the wrong clock. The spot `bookTicker` states no event time, so the pump
        // stamps `ts` with the arrival too (`crates/vike-bridge-core/src/depth.rs`'s `send_quote`).
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
        // a synced book has a two-sided top with bid < ask
        if let (Some(L2BookLevel { price: bid, .. }), Some(L2BookLevel { price: ask, .. })) =
            (book.best_bid(), book.best_ask())
        {
            assert!(bid < ask, "crossed L2 top: {bid} / {ask}");
        }
        self.books.fetch_add(1, Ordering::Relaxed);
    }
}

#[test]
#[ignore = "network — public Binance streams, run manually (see module doc)"]
fn binance_live_ticks_reach_the_strategy() {
    vike_log::test_init();
    let (quotes, trades, books) = (
        Arc::<AtomicUsize>::default(),
        Arc::<AtomicUsize>::default(),
        Arc::<AtomicUsize>::default(),
    );
    let engine = Engine::new(
        Account::new(1.0, "binance", None, BalanceMode::Delta),
        RiskGate::new(RiskLimits::new()),
        RecordingClient::default(),
        "binance",
        SYMBOL,
    );
    let config = CoreConfig {
        strategy: Some(Mount {
            order_client_id_code: None,
            account: None,
            symbols: Vec::new(),
            controller_id: None,
            underlying_symbol: None,
            venue: "binance".into(),
            symbol: SYMBOL.into(),
            interval: "1m".into(),
            strategy: Box::new(TickCounter {
                quotes: Arc::clone(&quotes),
                trades: Arc::clone(&trades),
                books: Arc::clone(&books),
            }),
        }),
        ..CoreConfig::default()
    };
    let handle = spawn_core(engine, config);

    let urls = vike_venues::venue_urls::VenueUrls::seed("binance");
    let feed = spawn_binance_feed_tick(&urls, handle.tick_sender(), SYMBOL, 0.01)
        .expect("the seed holds every binance row");

    // let live data flow, then require every lane to have fired at least once
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        let (q, t, b) = (
            quotes.load(Ordering::Relaxed),
            trades.load(Ordering::Relaxed),
            books.load(Ordering::Relaxed),
        );
        if q > 0 && t > 0 && b > 0 {
            tracing::info!(target: "vike_binance", "live ticks: {q} quotes, {t} trades, {b} book updates");
            break;
        }
        assert!(
            Instant::now() < deadline,
            "lanes idle after 20s (quotes={q}, trades={t}, books={b}) — market closed or stream down?"
        );
        std::thread::sleep(Duration::from_millis(200));
    }

    feed.shutdown();
    handle.shutdown_and_join();
    tracing::info!(target: "vike_binance", "binance market-data green: live bookTicker/trade/depth → on_quote_tick/on_trade_tick/on_order_book");
}
