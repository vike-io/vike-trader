//! Cross-venue runtime gate: ONE core thread, TWO engines (binance primary + bybit extra,
//! heterogeneous clients via `Box<dyn ExecutionClient + Send>`). Venue-tagged events and
//! `Command::Submit` route by venue; order-lifecycle replies route back via the coid map;
//! the snapshot carries per-venue blocks and the py_sum aggregate equity
//! (CrossVenueDriver semantics, now structural in the runtime).

use std::sync::atomic::AtomicUsize;
use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::Arc;
use vike_core::{spawn_core_multi, CoreConfig, LiveBroker, StrategyMount};
use vike_exec::testing::{RecordingClient, TestExecutionClient};
use vike_exec::BarUpdate;
use vike_exec::{
    Account, BalanceMode, Command, ExecutionClient, ExecutionEngine, OrderIntent, OrderStatus,
    RiskGate, RiskLimits,
};
use vike_model::events::{AccountState, Event};
use vike_model::OrderRequest;
use vike_model::{Bar, Fill, Strategy};

type DynClient = Box<dyn ExecutionClient + Send>;

fn engine(venue: &str, symbol: &str, client: DynClient) -> ExecutionEngine<DynClient> {
    ExecutionEngine::new(
        Account::new(1.0, venue, None, BalanceMode::Delta),
        RiskGate::new(RiskLimits::new()),
        client,
        venue,
        symbol,
    )
}

#[test]
fn two_venues_route_and_aggregate() {
    let primary = engine("binance", "BTCUSDT", Box::new(RecordingClient::default()));
    // bybit's client synthesizes Submitted/Accepted/Fill/Filled at the request price
    let bybit = engine("bybit", "ETHUSDT", Box::new(TestExecutionClient::new("bybit", 50.0)));

    let t = Arc::new(AtomicI64::new(0));
    let config = CoreConfig {
        seed_cash: 10_000.0,
        clock: Box::new(move || t.fetch_add(1, Ordering::Relaxed)),
        ..CoreConfig::default()
    };
    let handle = spawn_core_multi(primary, vec![(500.0, bybit)], config);
    let cell = handle.snapshot_cell();

    // 1. a GUI submit for the bybit venue routes to the bybit engine + client
    let req = OrderRequest {
        client_order_id: "xv1".into(),
        venue: "bybit".into(),
        symbol: "ETHUSDT".into(),
        order_type: "market".into(),
        side: 1,
        qty: 2.0,
        ts: 1,
        ..Default::default()
    };
    handle.try_command(Command::Order(OrderIntent::Submit(Box::new(req)))).unwrap();
    // 2. a venue-tagged AccountState for bybit routes to the bybit account
    handle
        .event_sender()
        .blocking_send(Event::AccountState(AccountState {
            venue: "bybit".into(),
            balances: vec![("USDT".into(), 700.0)],
            ts: 2,
            route_key: None,
        }))
        .unwrap();
    handle.shutdown_and_join();

    let snap = cell.load_full();
    // per-venue blocks: primary first, untouched; bybit folded the fill + account state
    assert_eq!(snap.portfolio.venues.len(), 2);
    assert_eq!(snap.portfolio.venues[0].venue, "binance");
    assert_eq!(snap.portfolio.venues[0].balance, 0.0);
    assert!(snap.portfolio.venues[0].positions.is_empty());
    let vb = &snap.portfolio.venues[1];
    assert_eq!(vb.venue, "bybit");
    assert_eq!(vb.balance, 700.0); // authoritative AccountState routed by venue
    assert_eq!(vb.balance_mode, BalanceMode::Authoritative);
    assert!(vb.positions.iter().any(|p| p.symbol == "ETHUSDT" && (p.size - 2.0).abs() < 1e-12));
    // the ETH position marked at the fill price: equity = 700 + (50-50)*2 = 700
    assert_eq!(vb.equity, 700.0);
    // aggregate equity: py_sum(primary 10_000, bybit 700)
    assert_eq!(snap.portfolio.equity_total, 10_700.0);
    // scalar fields still mirror the primary (GUI back-compat)
    assert_eq!(snap.balance, 0.0);
    assert_eq!(snap.portfolio.equity, 10_000.0);
    // 3. lifecycle replies (no venue on the wire) routed via the coid map: the bybit
    // order reached Filled in the BYBIT registry, surfaced with its venue tag
    let o = snap
        .orders
        .iter()
        .find(|o| o.client_order_id == "xv1")
        .expect("bybit order visible in the unified view");
    assert_eq!(o.venue, "bybit");
    assert_eq!(o.status, OrderStatus::Filled);
    assert!((o.filled_qty - 2.0).abs() < 1e-12);
}

/// spawn_core (no extras) still produces a single venue block mirroring the scalars —
/// the single-venue path is unchanged in shape and value.
#[test]
fn single_venue_block_mirrors_scalars() {
    let primary = engine("binance", "BTCUSDT", Box::new(RecordingClient::default()));
    let t = Arc::new(AtomicI64::new(0));
    let config = CoreConfig {
        seed_cash: 1_000.0,
        clock: Box::new(move || t.fetch_add(1, Ordering::Relaxed)),
        ..CoreConfig::default()
    };
    let handle = vike_core::spawn_core(primary, config);
    let cell = handle.snapshot_cell();
    handle.shutdown_and_join();
    let snap = cell.load_full();
    assert_eq!(snap.portfolio.venues.len(), 1);
    assert_eq!(snap.portfolio.venues[0].venue, "binance");
    assert_eq!(snap.portfolio.equity_total, snap.portfolio.equity);
}

// ---- strategy mounts on EXTRA venues (the final increment) ------------------------------

/// order_target_percent(0.5) on the first bar of the EXTRA venue; counts on_fill.
struct ExtraVenueHalf {
    done: bool,
    fills: Arc<AtomicUsize>,
}
impl Strategy<LiveBroker> for ExtraVenueHalf {
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

#[test]
fn mount_on_extra_venue_trades_its_own_engine() {
    let primary = engine("binance", "BTCUSDT", Box::new(RecordingClient::default()));
    let bybit = engine("bybit", "ETHUSDT", Box::new(TestExecutionClient::new("bybit", 50.0)));
    let fills = Arc::<AtomicUsize>::default();
    let t = Arc::new(AtomicI64::new(0));
    let config = CoreConfig {
        seed_cash: 10_000.0,
        clock: Box::new(move || t.fetch_add(1, Ordering::Relaxed)),
        extra_mounts: vec![StrategyMount {
            account: None,
            symbols: Vec::new(),
            controller_id: None,
            underlying_symbol: None,
            venue: "bybit".into(),
            symbol: "ETHUSDT".into(),
            interval: "1m".into(),
            strategy: Box::new(ExtraVenueHalf { done: false, fills: Arc::clone(&fills) }),
        }],
        ..CoreConfig::default()
    };
    let handle = spawn_core_multi(primary, vec![(500.0, bybit)], config);
    let cell = handle.snapshot_cell();

    // a closed bar on the EXTRA venue drives ITS mount: sizing from the bybit seed
    // (0.5 * 500 / 50 = 5), the TestExecutionClient fills at 50, on_fill delivered
    handle
        .bar_sender()
        .close(BarUpdate {
            venue: "bybit".into(),
            symbol: "ETHUSDT".into(),
            interval: "1m".into(),
            bar: Bar {
                ts: 60_000,
                open: 50.0,
                high: 50.5,
                low: 49.5,
                close: 50.0,
                volume: 1.0,
                funding: None,
                bid: None,
                ask: None,
                symbol: None,
            },
        })
        .unwrap();
    handle.shutdown_and_join();

    let snap = cell.load_full();
    let o = snap
        .orders
        .iter()
        .find(|o| o.venue == "bybit")
        .expect("strategy order routed to the bybit engine");
    assert!((o.qty - 5.0).abs() < 1e-12, "qty {}", o.qty);
    assert_eq!(o.status, OrderStatus::Filled); // lifecycle replies routed via coid map
    let vb = snap.portfolio.venues.iter().find(|v| v.venue == "bybit").unwrap();
    assert!(vb.positions.iter().any(|p| p.symbol == "ETHUSDT" && (p.size - 5.0).abs() < 1e-12));
    assert_eq!(fills.load(Ordering::Relaxed), 1, "on_fill reached the extra-venue mount");
    // the primary engine saw none of it
    assert!(snap.portfolio.venues[0].positions.is_empty());
    assert!(snap.orders.iter().all(|o| o.venue != "binance"));
}
