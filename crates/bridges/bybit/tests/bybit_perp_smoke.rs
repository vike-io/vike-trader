//! R6 slice-4 LIVE Bybit smoke (network + demo creds — run manually):
//!     cargo test -p vike-bybit --test bybit_perp_smoke -- --ignored --nocapture
//!
//! The V5 linear ladder, live: set_leverage (110043-safe) → reconcile → private-WS
//! auth+subscribe handshake → MARKET BUY filled via the execution topic → SELL 2×
//! (FLIPS TO A REAL SHORT — live signed reconcile gate) → BUY back to flat → funding
//! poller probe → teardown.

use std::time::{Duration, Instant};

use serde_json::json;
use vike_bridge_core::credentials::{
    load_credentials_from, load_workspace_dotenv_from, Environment,
};
use vike_bridge_core::rest::LiveRestClient;
use vike_bridge_core::signer::BybitV5Signer;
use vike_bybit::funding::BybitFundingPoller;
use vike_bybit::perp::{
    parse_bybit_perp_instruments, BybitPerpRest, DEMO_REST, DEMO_WS, PATH_INSTRUMENTS,
};
use vike_bybit::transport::{BybitTransport, UreqBybitTransport};
use vike_bybit::user_data::spawn_bybit_perp_user_data;
use vike_core::{spawn_core, CoreConfig};
use vike_exec::{
    Account, BalanceMode, Command, ExecutionEngine, OrderIntent, OrderStatus, RiskGate, RiskLimits,
};

use vike_model::clock::now_ms;

/// Condition-based settle: block until the user-data pump reports it has subscribed (or a 10s
/// deadline), so the order path never races a cold-start handshake that would drop a fill landing
/// before subscribe — the fixed `sleep(3s)` flake this replaces.
fn wait_pump_ready(ready: &std::sync::Arc<std::sync::atomic::AtomicBool>) {
    let deadline = Instant::now() + Duration::from_secs(10);
    while !ready.load(std::sync::atomic::Ordering::Acquire) && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(50));
    }
    assert!(ready.load(std::sync::atomic::Ordering::Acquire), "user-data pump never became ready");
}

fn submit_market(handle: &vike_core::CoreHandle, coid: &str, side: i32, qty: f64) {
    let req: vike_model::OrderRequest = serde_json::from_value(json!({
        "client_order_id": coid, "venue": "bybit", "symbol": "BTCUSDT",
        "side": side, "qty": qty, "order_type": "market", "ts": now_ms()
    }))
    .unwrap();
    handle.try_command(Command::Order(OrderIntent::Submit(Box::new(req)))).unwrap();
}

fn wait_filled(
    cell: &std::sync::Arc<arc_swap::ArcSwap<vike_core::CoreSnapshot>>,
    coid: &str,
) -> vike_core::OrderView {
    let deadline = Instant::now() + Duration::from_secs(25);
    loop {
        let snap = cell.load_full();
        if let Some(o) = snap.orders.iter().find(|o| o.client_order_id == coid) {
            if o.status == OrderStatus::Filled && o.filled_qty > 0.0 {
                return o.clone();
            }
            assert!(
                !matches!(o.status, OrderStatus::Rejected | OrderStatus::Denied),
                "{coid} {:?}: {:?}",
                o.status,
                snap.recent_events
            );
        }
        assert!(
            Instant::now() < deadline,
            "{coid} never FILLED; recent: {:?}",
            cell.load().recent_events
        );
        std::thread::sleep(Duration::from_millis(250));
    }
}

fn position_size(cell: &std::sync::Arc<arc_swap::ArcSwap<vike_core::CoreSnapshot>>) -> f64 {
    cell.load().positions.iter().find(|p| p.symbol == "BTCUSDT").map_or(0.0, |p| p.size)
}

#[test]
#[ignore = "network + demo creds — run manually (see module doc)"]
fn bybit_ladder_long_short_flat() {
    vike_log::test_init();
    let vars = load_workspace_dotenv_from(std::env::var("VIKE_SETTINGS_DIR").ok().as_deref());
    let Some(creds) = load_credentials_from("bybit", Environment::Demo, &vars) else {
        tracing::warn!(target: "vike_bybit", "SKIP: BYBIT_DEMO creds absent");
        return;
    };

    let transport =
        UreqBybitTransport::new().with_rate_gate(vike_bybit::ratelimit::rest_rate_gate());
    // instruments-info is public but the signed transport handles it fine
    let info = transport
        .signed(
            DEMO_REST,
            PATH_INSTRUMENTS,
            "GET",
            &[("category", json!("linear")), ("symbol", json!("BTCUSDT"))],
            &BybitV5Signer::new(&creds, now_ms),
        )
        .expect("instruments-info");
    let inst = parse_bybit_perp_instruments(&info)["BTCUSDT"].clone();
    tracing::info!(
        target: "vike_bybit",
        "bybit properties: tick={} step={} min_qty={} min_notional={}",
        inst.properties.tick_size, inst.properties.step_size, inst.properties.min_qty,
        inst.properties.min_notional
    );

    let make_rest = || BybitPerpRest {
        signer: BybitV5Signer::new(&creds, now_ms),
        transport: UreqBybitTransport::new()
            .with_rate_gate(vike_bybit::ratelimit::rest_rate_gate()),
        base_url: DEMO_REST.to_string(),
        symbol: "BTCUSDT".to_string(),
        properties: inst.properties,
        leverage: 2.0,
    };
    let rest = make_rest();
    rest.set_leverage().expect("set_leverage (110043-safe)");
    let mark = rest.last_price().expect("tickers");
    let snap0 = rest.reconcile_positions().expect("reconcile");
    tracing::info!(
        target: "vike_bybit",
        "reconcile: positions={:?} balance={} mark={mark}",
        snap0.positions, snap0.balance
    );
    assert!(snap0.balance > 0.0, "UNIFIED USDT wallet balance must be live");
    let base = snap0.positions.first().map_or(0.0, |(_, q)| *q);
    let probe = make_rest();

    let engine = ExecutionEngine::new(
        Account::new(1.0, "bybit", None, BalanceMode::Delta),
        RiskGate::new(RiskLimits::new()),
        LiveRestClient::new(rest),
        "bybit",
        "BTCUSDT",
    );
    let handle = spawn_core(engine, CoreConfig::default());
    let cell = handle.snapshot_cell();
    handle.try_command(Command::ApplySnapshot(Box::new(snap0))).unwrap();
    let ready = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let feed = spawn_bybit_perp_user_data(
        DEMO_WS.to_string(),
        creds.api_key.clone(),
        creds.api_secret.clone(),
        "BTCUSDT".to_string(),
        handle.event_sender(),
        ready.clone(),
    );
    wait_pump_ready(&ready); // condition-based settle: proceed only once the pump has subscribed

    // per-leg qty: satisfy min_qty AND min_notional at the live mark
    let qty = f64::max(inst.properties.min_qty, (inst.properties.min_notional * 1.4) / mark);
    let qty = (qty / inst.properties.step_size).ceil() * inst.properties.step_size;
    tracing::info!(target: "vike_bybit", "leg qty: {qty}");

    // LONG leg
    let b1 = format!("vtr6byB{}", now_ms() % 100_000_000);
    submit_market(&handle, &b1, 1, qty);
    let buy = wait_filled(&cell, &b1);
    tracing::info!(target: "vike_bybit", "LONG filled: {} @ {} (mark {:?})", buy.filled_qty, buy.avg_fill_px, mark);
    assert!(position_size(&cell) > base, "long folded");

    // FLIP to SHORT
    let s1 = format!("vtr6byS{}", now_ms() % 100_000_000);
    submit_market(&handle, &s1, -1, qty * 2.0);
    wait_filled(&cell, &s1);
    let flipped = position_size(&cell);
    assert!(flipped < base - qty * 0.5, "must be net SHORT: base={base} now={flipped}");

    // THE live signed-SHORT reconcile gate
    let live = probe.reconcile_positions().expect("live reconcile while short");
    tracing::info!(target: "vike_bybit", "LIVE SHORT reconcile: {:?}", live.positions);
    assert!(
        live.positions.iter().any(|(_, q)| *q < 0.0),
        "venue reconcile must report the signed SHORT: {:?}",
        live.positions
    );

    // back to flat
    let b2 = format!("vtr6byF{}", now_ms() % 100_000_000);
    submit_market(&handle, &b2, 1, qty);
    wait_filled(&cell, &b2);
    let fin = position_size(&cell);
    tracing::info!(
        target: "vike_bybit",
        "final: size={fin} realized={} fees={}",
        cell.load().portfolio.realized_pnl,
        cell.load().portfolio.fees_paid
    );
    assert!(
        (fin - base).abs() < inst.properties.step_size * 1.5,
        "round trip must flatten: base={base} final={fin}"
    );
    assert!(cell.load().portfolio.fees_paid != 0.0, "execFee commissions tracked");
    assert!(cell.load().fault.is_none());

    // funding poller probe: the call path works (settlements may be empty in the window).
    // Floor 0 = no spawn floor, so the probe sees the venue's whole default window.
    let mut poller = BybitFundingPoller::new(&probe, "BTCUSDT", 0);
    let funding = poller.poll().expect("live funding poll");
    tracing::info!(target: "vike_bybit", "funding poller: {} settlement rows decoded", funding.len());

    feed.shutdown().expect("bybit user-data shutdown");
    handle.shutdown_and_join();
    tracing::info!(target: "vike_bybit", "bybit ladder green: leverage -> long -> SHORT (live signed reconcile) -> flat -> teardown");
}
