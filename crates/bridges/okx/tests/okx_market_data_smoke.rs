//! LIVE OKX market-data smoke (network, NO creds — public streams):
//!     cargo test -p vike-okx --test okx_market_data_smoke -- --ignored --nocapture
//!
//! Same proof as the Binance market-data smoke, for OKX: real public WS → decoders → TickSender →
//! core → a mounted strategy's tick handlers firing. Also verifies the OKX WS message formats
//! against the live stream. Moved into crates/bridges/okx (crate-reorg Phase 3, PR G) as the sole
//! resident of this file — Bybit's twin already moved out to `vike-bybit`'s own
//! `bybit_market_data_smoke.rs` when bybit split out earlier (PR E).

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use vike_core::{spawn_core, CoreConfig, CoreHandle, LiveBroker, StrategyMount};
use vike_exec::testing::RecordingClient;
use vike_exec::{Account, BalanceMode, ExecutionEngine, RiskGate, RiskLimits};
use vike_model::{L2Book, QuoteTick, Strategy, TradeTick};

#[derive(Default)]
struct TickCounter {
    quotes: Arc<AtomicUsize>,
    trades: Arc<AtomicUsize>,
    books: Arc<AtomicUsize>,
}

impl Strategy<LiveBroker> for TickCounter {
    fn on_quote_tick(&mut self, _b: &mut LiveBroker, q: &QuoteTick) {
        assert!(q.ask >= q.bid, "crossed quote {}/{}", q.bid, q.ask);
        self.quotes.fetch_add(1, Ordering::Relaxed);
    }
    fn on_trade_tick(&mut self, _b: &mut LiveBroker, t: &TradeTick) {
        assert!(t.price > 0.0 && t.size > 0.0);
        self.trades.fetch_add(1, Ordering::Relaxed);
    }
    fn on_order_book(&mut self, _b: &mut LiveBroker, book: &L2Book) {
        if let (Some((bid, _)), Some((ask, _))) = (book.best_bid(), book.best_ask()) {
            assert!(bid < ask, "crossed L2 top {bid}/{ask}");
        }
        self.books.fetch_add(1, Ordering::Relaxed);
    }
}

/// Mount a TickCounter on (venue, symbol) and return the core handle + the three counters.
fn core_with_counter(
    venue: &str,
    symbol: &str,
) -> (CoreHandle, Arc<AtomicUsize>, Arc<AtomicUsize>, Arc<AtomicUsize>) {
    let (q, t, b) = (
        Arc::<AtomicUsize>::default(),
        Arc::<AtomicUsize>::default(),
        Arc::<AtomicUsize>::default(),
    );
    let engine = ExecutionEngine::new(
        Account::new(1.0, venue, None, BalanceMode::Delta),
        RiskGate::new(RiskLimits::new()),
        RecordingClient::default(),
        venue,
        symbol,
    );
    let config = CoreConfig {
        strategy: Some(StrategyMount {
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
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        let (qn, tn, bn) =
            (q.load(Ordering::Relaxed), t.load(Ordering::Relaxed), b.load(Ordering::Relaxed));
        if qn > 0 && tn > 0 && bn > 0 {
            tracing::info!(target: "vike_okx", "live ticks: {qn} quotes, {tn} trades, {bn} book updates");
            return;
        }
        assert!(
            Instant::now() < deadline,
            "lanes idle after 20s (q={qn}, t={tn}, b={bn}) — market/stream down?"
        );
        std::thread::sleep(Duration::from_millis(200));
    }
}

#[test]
#[ignore = "network — public OKX streams, run manually"]
fn okx_live_ticks_reach_the_strategy() {
    vike_log::test_init();
    use vike_okx::market_data::spawn_okx_market_data;
    let (handle, q, t, b) = core_with_counter("okx", "BTC-USDT-SWAP");
    let feed = spawn_okx_market_data(handle.tick_sender(), "BTC-USDT-SWAP", 0.1);
    wait_all_lanes(&q, &t, &b);
    feed.shutdown();
    handle.shutdown_and_join();
    tracing::info!(target: "vike_okx", "okx market-data green: bbo-tbt/trades/books5 → the tick/L2 handlers");
}
