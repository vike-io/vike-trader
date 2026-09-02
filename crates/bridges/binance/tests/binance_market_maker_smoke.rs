//! LIVE end-to-end market-maker smoke (network, NO real orders — a LOCAL accepting client):
//!     cargo test -p vike-binance --test binance_market_maker_smoke -- --ignored --nocapture
//!
//! The full payoff of the HFT track: the live Binance-spot feed drives `SpreadMaker`, which rests a
//! tagged bid + ask and RE-QUOTES them in place via modify as the real mid moves. Orders go to a
//! local accepting client (no venue creds, no real orders), so this proves the composition —
//! live market data → on_quote_tick → tagged submit → modify — end to end.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use vike_binance::market_data::spawn_binance_market_data;
use vike_core::{spawn_core, CoreConfig, StrategyMount};
use vike_exec::{Account, BalanceMode, ExecutionClient, ExecutionEngine, RiskGate, RiskLimits};
use vike_mm::SpreadMaker; // the maker moved to the vike-mm crate (the vike-mm extraction)
use vike_model::events::{Event, OrderAccepted, OrderModified, OrderSubmitted};
use vike_model::OrderRequest;

const SYMBOL: &str = "BTCUSDT";

/// Accepts every order (so it rests, modifiable) and counts modifications — no real orders leave the box.
struct MakerClient {
    events: VecDeque<Event>,
    modifications: Arc<AtomicUsize>,
}

impl ExecutionClient for MakerClient {
    fn submit(&mut self, request: &OrderRequest) {
        self.events.push_back(Event::OrderSubmitted(OrderSubmitted {
            client_order_id: request.client_order_id.clone(),
            ts: request.ts,
        }));
        self.events.push_back(Event::OrderAccepted(OrderAccepted {
            client_order_id: request.client_order_id.clone(),
            venue_order_id: None,
            ts: request.ts,
        }));
    }
    fn cancel(&mut self, _client_order_id: &str) {}
    fn modify(&mut self, order: &OrderRequest, new_qty: Option<f64>, new_price: Option<f64>) {
        self.modifications.fetch_add(1, Ordering::Relaxed);
        self.events.push_back(Event::OrderModified(OrderModified {
            client_order_id: order.client_order_id.clone(),
            venue_order_id: None,
            new_qty,
            new_price,
            ts: 0,
        }));
    }
    fn poll_events(&mut self) -> Option<Event> {
        self.events.pop_front()
    }
}

#[test]
#[ignore = "network — public Binance streams, run manually (see module doc)"]
fn spread_maker_runs_on_live_data() {
    vike_log::test_init();
    let modifications = Arc::<AtomicUsize>::default();
    let engine = ExecutionEngine::new(
        Account::new(1.0, "binance", None, BalanceMode::Delta),
        RiskGate::new(RiskLimits::new()),
        MakerClient { events: VecDeque::new(), modifications: Arc::clone(&modifications) },
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
            strategy: Box::new(SpreadMaker::new(0.001, 5.0)), // 0.001 BTC, ±$5 off mid
        }),
        ..CoreConfig::default()
    };
    let handle = spawn_core(engine, config);
    let cell = handle.snapshot_cell();
    let feed = spawn_binance_market_data(handle.tick_sender(), SYMBOL, 0.01);

    // wait until the maker has re-quoted on live data at least a few times
    let deadline = Instant::now() + Duration::from_secs(25);
    loop {
        let n = modifications.load(Ordering::Relaxed);
        if n >= 4 {
            let snap = cell.load();
            tracing::info!(target: "vike_binance", "SpreadMaker live: {n} re-quotes, {} resting orders", snap.orders.len());
            // modify-in-place, NOT churn: exactly the two tagged orders regardless of re-quote count
            assert_eq!(snap.orders.len(), 2, "one bid + one ask, re-priced in place");
            assert!(snap.orders.iter().any(|o| o.side == 1), "resting bid");
            assert!(snap.orders.iter().any(|o| o.side == -1), "resting ask");
            break;
        }
        assert!(
            Instant::now() < deadline,
            "maker idle after 25s ({n} modifications) — market closed or stream down?"
        );
        std::thread::sleep(Duration::from_millis(200));
    }

    feed.shutdown();
    handle.shutdown_and_join();
    tracing::info!(target: "vike_binance", "market-maker green: live feed → tagged bid/ask → live modify re-quoting (2 orders, no churn)");
}
