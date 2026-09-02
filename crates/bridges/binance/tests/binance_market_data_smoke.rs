//! LIVE Binance-spot market-data smoke (network, NO creds — public streams):
//!     cargo test -p vike-binance --test binance_market_data_smoke -- --ignored --nocapture
//!
//! Proves the whole R8 tick/L2 chain end-to-end: real Binance bookTicker/trade/depth WS →
//! decoders → TickSender → the single-writer core → a mounted strategy's on_quote_tick /
//! on_trade_tick / on_order_book actually firing on live data.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use vike_binance::market_data::spawn_binance_market_data;
use vike_core::{spawn_core, CoreConfig, LiveBroker, StrategyMount};
use vike_exec::testing::RecordingClient;
use vike_exec::{Account, BalanceMode, ExecutionEngine, RiskGate, RiskLimits};
use vike_model::{L2Book, QuoteTick, Strategy, TradeTick};

const SYMBOL: &str = "BTCUSDT";

#[derive(Default)]
struct TickCounter {
    quotes: Arc<AtomicUsize>,
    trades: Arc<AtomicUsize>,
    books: Arc<AtomicUsize>,
}

impl Strategy<LiveBroker> for TickCounter {
    fn on_quote_tick(&mut self, _b: &mut LiveBroker, q: &QuoteTick) {
        assert!(q.ask >= q.bid, "crossed book: {} / {}", q.bid, q.ask);
        self.quotes.fetch_add(1, Ordering::Relaxed);
    }
    fn on_trade_tick(&mut self, _b: &mut LiveBroker, t: &TradeTick) {
        assert!(t.price > 0.0 && t.size > 0.0);
        self.trades.fetch_add(1, Ordering::Relaxed);
    }
    fn on_order_book(&mut self, _b: &mut LiveBroker, book: &L2Book) {
        // a synced book has a two-sided top with bid < ask
        if let (Some((bid, _)), Some((ask, _))) = (book.best_bid(), book.best_ask()) {
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
    let engine = ExecutionEngine::new(
        Account::new(1.0, "binance", None, BalanceMode::Delta),
        RiskGate::new(RiskLimits::new()),
        RecordingClient::default(),
        "binance",
        SYMBOL,
    );
    let config = CoreConfig {
        strategy: Some(StrategyMount {
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

    let feed = spawn_binance_market_data(handle.tick_sender(), SYMBOL, 0.01);

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
