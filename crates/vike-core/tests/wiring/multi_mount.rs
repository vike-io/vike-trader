//! Phase D gate: N strategy mounts over ONE engine/Account (multi-symbol slice).
//! Two mounts (BTCUSDT primary + ETHUSDT via `extra_symbols`/`extra_mounts`) each size
//! from the shared equity, fills route to THEIR mount only, and marks/positions for both
//! symbols surface in the one snapshot.

use std::sync::atomic::{AtomicI64, AtomicUsize, Ordering};
use std::sync::Arc;
use vike_core::{spawn_core, CoreConfig, LiveBroker, StrategyMount};
use vike_exec::testing::RecordingClient;
use vike_exec::{Account, BalanceMode, BarUpdate, ExecutionEngine, RiskGate, RiskLimits};
use vike_model::events::{Event, FillEvent};
use vike_model::{Bar, Fill, Strategy};

fn bar(ts: i64, px: f64) -> Bar {
    Bar {
        ts,
        open: px,
        high: px,
        low: px,
        close: px,
        volume: 1.0,
        funding: None,
        bid: None,
        ask: None,
        symbol: None,
    }
}

/// order_target_percent(0.5) on the first bar; counts on_fill deliveries.
struct HalfTarget {
    done: bool,
    fills: Arc<AtomicUsize>,
}
impl Strategy<LiveBroker> for HalfTarget {
    fn on_bar(&mut self, broker: &mut LiveBroker, _bar: &Bar) {
        if !self.done {
            self.done = true;
            broker.order_target_percent(0.5);
        }
    }
    fn on_fill(&mut self, _broker: &mut LiveBroker, _fill: &Fill) {
        self.fills.fetch_add(1, Ordering::Relaxed);
    }
}

fn mount(symbol: &str, fills: &Arc<AtomicUsize>) -> StrategyMount {
    StrategyMount {
        account: None,
        symbols: Vec::new(),
        controller_id: None,
        underlying_symbol: None,
        venue: "binance".into(),
        symbol: symbol.into(),
        interval: "1m".into(),
        strategy: Box::new(HalfTarget { done: false, fills: Arc::clone(fills) }),
    }
}

fn close(handle: &vike_core::CoreHandle, symbol: &str, b: Bar) {
    handle
        .bar_sender()
        .close(BarUpdate {
            venue: "binance".into(),
            symbol: symbol.into(),
            interval: "1m".into(),
            bar: b,
        })
        .unwrap();
}

fn eth_fill(qty: f64, px: f64) -> FillEvent {
    FillEvent {
        trade_id: "t-eth-1".into(),
        client_order_id: String::new(),
        venue: "binance".into(),
        symbol: "ETHUSDT".into(),
        side: 1,
        last_qty: qty,
        last_px: px,
        commission: 0.0,
        commission_asset: String::new().into(),
        liquidity_side: "taker".into(),
        ts: 10,
        mark_price: None,
        position_side: "BOTH".into(),
    }
}

#[test]
fn two_mounts_one_engine_route_independently() {
    let mut engine = ExecutionEngine::new(
        Account::new(1.0, "binance", None, BalanceMode::Delta),
        RiskGate::new(RiskLimits::new()),
        RecordingClient::default(),
        "binance",
        "BTCUSDT",
    );
    engine.extra_symbols = vec!["ETHUSDT".into()];

    let btc_fills = Arc::<AtomicUsize>::default();
    let eth_fills = Arc::<AtomicUsize>::default();
    let t = Arc::new(AtomicI64::new(0));
    let config = CoreConfig {
        seed_cash: 10_000.0,
        clock: Box::new(move || t.fetch_add(1, Ordering::Relaxed)),
        strategy: Some(mount("BTCUSDT", &btc_fills)),
        extra_mounts: vec![mount("ETHUSDT", &eth_fills)],
        ..CoreConfig::default()
    };
    let handle = spawn_core(engine, config);
    let cell = handle.snapshot_cell();

    // each mount sizes from the SHARED equity at its own price
    close(&handle, "BTCUSDT", bar(60_000, 100.0)); // 0.5·10k/100 = 50
    close(&handle, "ETHUSDT", bar(60_000, 50.0)); // 0.5·10k/50 = 100
                                                  // a venue fill for ETH folds through extra_symbols and reaches ONLY the ETH mount
    handle.event_sender().blocking_send(Event::Fill(eth_fill(1.0, 50.0))).unwrap();
    handle.shutdown_and_join();

    let snap = cell.load_full();
    // both rebalance orders registered, each under its own symbol
    assert_eq!(snap.orders.len(), 2, "orders: {:?}", snap.orders);
    let btc = snap.orders.iter().find(|o| o.symbol == "BTCUSDT").unwrap();
    let eth = snap.orders.iter().find(|o| o.symbol == "ETHUSDT").unwrap();
    assert!((btc.qty - 50.0).abs() < 1e-12, "btc qty {}", btc.qty);
    assert!((eth.qty - 100.0).abs() < 1e-12, "eth qty {}", eth.qty);
    // marks recorded per symbol in the one account
    assert!(snap.marks.iter().any(|(_, s, px)| s == "BTCUSDT" && *px == 100.0));
    assert!(snap.marks.iter().any(|(_, s, px)| s == "ETHUSDT" && *px == 50.0));
    // the ETH fill folded into the shared account...
    assert!(snap.positions.iter().any(|p| p.symbol == "ETHUSDT" && (p.size - 1.0).abs() < 1e-12));
    // ...and its on_fill reached only the ETH mount
    assert_eq!(eth_fills.load(Ordering::Relaxed), 1, "eth on_fill");
    assert_eq!(btc_fills.load(Ordering::Relaxed), 0, "btc on_fill");
}

/// With no extra_symbols, an off-symbol fill is still filtered — the single-symbol
/// default is byte-identical to the pre-Phase-D engine.
#[test]
fn default_engine_still_filters_foreign_symbols() {
    let engine = ExecutionEngine::new(
        Account::new(1.0, "binance", None, BalanceMode::Delta),
        RiskGate::new(RiskLimits::new()),
        RecordingClient::default(),
        "binance",
        "BTCUSDT",
    );
    let t = Arc::new(AtomicI64::new(0));
    let config = CoreConfig {
        seed_cash: 10_000.0,
        clock: Box::new(move || t.fetch_add(1, Ordering::Relaxed)),
        ..CoreConfig::default()
    };
    let handle = spawn_core(engine, config);
    let cell = handle.snapshot_cell();
    handle.event_sender().blocking_send(Event::Fill(eth_fill(1.0, 50.0))).unwrap();
    handle.shutdown_and_join();
    assert!(cell.load_full().positions.is_empty(), "foreign fill dropped");
}
