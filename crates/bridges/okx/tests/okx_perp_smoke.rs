//! R6 slice-5 LIVE OKX smoke (network + demo creds — run manually):
//!     cargo test -p vike-okx --test okx_perp_smoke -- --ignored --nocapture
//!
//! The V5 SWAP ladder, live on the simulated-trading demo: set_leverage → reconcile →
//! private-WS login+subscribe (browser UA, Cloudflare-gated endpoint) → MARKET BUY in
//! CONTRACTS filled via the orders channel → SELL 2× (FLIPS TO A REAL SHORT — live
//! signed reconcile in BASE units) → BUY back to flat → bills funding probe → teardown.

use std::time::{Duration, Instant};

use serde_json::json;
use vike_bridge_core::credentials::{
    load_credentials_from, load_workspace_dotenv_from, Environment,
};
use vike_bridge_core::rest::LiveRestClient;
use vike_bridge_core::signer::OkxV5Signer;
use vike_core::{spawn_core, CoreConfig};
use vike_exec::{
    Account, BalanceMode, Command, ExecutionEngine, OrderIntent, OrderStatus, RiskGate, RiskLimits,
};
use vike_okx::funding::OkxFundingPoller;
use vike_okx::perp::{parse_okx_perp_instruments, OkxPerpRest, DEMO_WS, PATH_INSTRUMENTS, REST};
use vike_okx::transport::{OkxTransport, UreqOkxTransport};
use vike_okx::user_data::spawn_okx_perp_user_data;

const SYMBOL: &str = "BTC-USDT-SWAP";

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

fn submit_market(handle: &vike_core::CoreHandle, coid: &str, side: i32, base_qty: f64) {
    let req: vike_model::OrderRequest = serde_json::from_value(json!({
        "client_order_id": coid, "venue": "okx", "symbol": SYMBOL,
        "side": side, "qty": base_qty, "order_type": "market", "ts": now_ms()
    }))
    .unwrap();
    handle.try_command(Command::Order(OrderIntent::Submit(Box::new(req)))).unwrap();
}

fn submit_limit(handle: &vike_core::CoreHandle, coid: &str, side: i32, base_qty: f64, price: f64) {
    let req: vike_model::OrderRequest = serde_json::from_value(json!({
        "client_order_id": coid, "venue": "okx", "symbol": SYMBOL,
        "side": side, "qty": base_qty, "order_type": "limit", "price": price, "ts": now_ms()
    }))
    .unwrap();
    handle.try_command(Command::Order(OrderIntent::Submit(Box::new(req)))).unwrap();
}

/// Poll the snapshot until `coid` reaches `want` (or a terminal reject), returning that view.
fn wait_status(
    cell: &std::sync::Arc<arc_swap::ArcSwap<vike_core::CoreSnapshot>>,
    coid: &str,
    want: OrderStatus,
) -> vike_core::OrderView {
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        let snap = cell.load_full();
        if let Some(o) = snap.orders.iter().find(|o| o.client_order_id == coid) {
            if o.status == want {
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
            "{coid} never reached {want:?}; recent: {:?}",
            cell.load().recent_events
        );
        std::thread::sleep(Duration::from_millis(250));
    }
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
    cell.load().positions.iter().find(|p| p.symbol == SYMBOL).map_or(0.0, |p| p.size)
}

#[test]
#[ignore = "network + demo creds — run manually (see module doc)"]
fn okx_ladder_long_short_flat() {
    vike_log::test_init();
    let vars = load_workspace_dotenv_from(std::env::var("VIKE_SETTINGS_DIR").ok().as_deref());
    let Some(creds) = load_credentials_from("okx", Environment::Demo, &vars) else {
        tracing::warn!(target: "vike_okx", "SKIP: OKX_DEMO creds absent");
        return;
    };
    assert!(creds.passphrase.is_some(), "OKX needs the API passphrase");

    let transport =
        UreqOkxTransport::new(true).with_rate_gate(vike_okx::ratelimit::rest_rate_gate()); // demo: x-simulated-trading: 1
    let info = transport
        .public(REST, PATH_INSTRUMENTS, &[("instType", "SWAP".into()), ("instId", SYMBOL.into())])
        .expect("public instruments");
    let inst = parse_okx_perp_instruments(
        &vike_okx::transport::unwrap_okx(info)
            .map(|d| json!({"data": d}))
            .expect("instruments data"),
    )[SYMBOL]
        .clone();
    tracing::info!(
        target: "vike_okx",
        "okx properties: tick={} lotSz={} minSz={} ctVal={}",
        inst.properties.tick_size, inst.properties.step_size, inst.properties.min_qty, inst.ct_val
    );

    let make_rest = || OkxPerpRest {
        signer: OkxV5Signer::new(&creds, now_ms),
        transport: UreqOkxTransport::new(true)
            .with_rate_gate(vike_okx::ratelimit::rest_rate_gate()),
        base_url: REST.to_string(),
        symbol: SYMBOL.to_string(),
        properties: inst.properties,
        ct_val: inst.ct_val,
        leverage: 2.0,
        broker_code: None,
    };
    let rest = make_rest();
    rest.set_leverage().expect("set_leverage");
    let mark = rest.last_price().expect("ticker");
    let snap0 = rest.reconcile_positions().expect("reconcile");
    tracing::info!(
        target: "vike_okx",
        "reconcile: positions={:?} balance={} mark={mark}",
        snap0.positions, snap0.balance
    );
    let base = snap0.positions.first().map_or(0.0, |(_, q)| *q);
    let probe = make_rest();

    let engine = ExecutionEngine::new(
        Account::new(1.0, "okx", None, BalanceMode::Delta),
        RiskGate::new(RiskLimits::new()),
        LiveRestClient::new(rest),
        "okx",
        SYMBOL,
    );
    let handle = spawn_core(engine, CoreConfig::default());
    let cell = handle.snapshot_cell();
    handle.try_command(Command::ApplySnapshot(Box::new(snap0))).unwrap();
    let ready = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let feed = spawn_okx_perp_user_data(
        DEMO_WS.to_string(),
        creds.api_key.clone(),
        creds.api_secret.clone(),
        creds.passphrase.clone().unwrap_or_default(),
        SYMBOL.to_string(),
        inst.ct_val,
        handle.event_sender(),
        ready.clone(),
    );
    wait_pump_ready(&ready); // condition-based settle: proceed only once the pump has subscribed

    // per-leg BASE qty = minSz contracts (BTC-USDT-SWAP: 0.01 ct × 0.01 BTC = 0.0001 BTC)
    let leg_base = inst.properties.min_qty * inst.ct_val;
    tracing::info!(
        target: "vike_okx",
        "leg: {} contracts = {leg_base} BTC (~${:.0})", inst.properties.min_qty,
        leg_base * mark
    );

    // LONG leg
    let b1 = format!("vtr6okB{}", now_ms() % 100_000_000);
    submit_market(&handle, &b1, 1, leg_base);
    let buy = wait_filled(&cell, &b1);
    tracing::info!(target: "vike_okx", "LONG filled: {} base @ {}", buy.filled_qty, buy.avg_fill_px);
    assert!(position_size(&cell) > base, "long folded (base units)");

    // FLIP to SHORT
    let s1 = format!("vtr6okS{}", now_ms() % 100_000_000);
    submit_market(&handle, &s1, -1, leg_base * 2.0);
    wait_filled(&cell, &s1);
    let flipped = position_size(&cell);
    assert!(flipped < base - leg_base * 0.5, "must be net SHORT: base={base} now={flipped}");

    // THE live signed-SHORT reconcile gate (venue pos in contracts -> base via ctVal)
    let live = probe.reconcile_positions().expect("live reconcile while short");
    tracing::info!(target: "vike_okx", "LIVE SHORT reconcile (base units): {:?}", live.positions);
    assert!(
        live.positions.iter().any(|(_, q)| *q < 0.0),
        "venue reconcile must report the signed SHORT: {:?}",
        live.positions
    );

    // back to flat
    let b2 = format!("vtr6okF{}", now_ms() % 100_000_000);
    submit_market(&handle, &b2, 1, leg_base);
    wait_filled(&cell, &b2);
    let fin = position_size(&cell);
    tracing::info!(
        target: "vike_okx",
        "final: size={fin} realized={} fees={}",
        cell.load().portfolio.realized_pnl,
        cell.load().portfolio.fees_paid
    );
    assert!(
        (fin - base).abs() < leg_base * 0.5,
        "round trip must flatten: base={base} final={fin}"
    );
    assert!(cell.load().portfolio.fees_paid != 0.0, "-fillFee commissions tracked");
    assert!(cell.load().fault.is_none());

    // bills funding probe: the signed call path works.
    // Floor 0 = no spawn floor, so the probe sees the venue's whole default window.
    let mut poller = OkxFundingPoller::new(&probe, SYMBOL, 0);
    let funding = poller.poll().expect("live funding poll");
    tracing::info!(target: "vike_okx", "funding bills poller: {} rows decoded", funding.len());

    feed.shutdown().expect("okx user-data shutdown");
    handle.shutdown_and_join();
    tracing::info!(target: "vike_okx", "okx ladder green: leverage -> long -> SHORT (live signed reconcile) -> flat -> teardown");
}

/// LIVE native-modify smoke (demo): rest a far-from-market limit so it stays ACCEPTED, modify its
/// qty+price via the native /api/v5/trade/amend-order endpoint, verify the OrderModified folds and
/// the resting terms change, then cancel. Never fills (price is ~50% below market).
#[test]
#[ignore = "network + demo creds — run manually (see module doc)"]
fn okx_native_modify_reprices_resting_limit() {
    vike_log::test_init();
    let vars = load_workspace_dotenv_from(std::env::var("VIKE_SETTINGS_DIR").ok().as_deref());
    let Some(creds) = load_credentials_from("okx", Environment::Demo, &vars) else {
        tracing::warn!(target: "vike_okx", "SKIP: OKX_DEMO creds absent");
        return;
    };
    assert!(creds.passphrase.is_some(), "OKX needs the API passphrase");

    let transport =
        UreqOkxTransport::new(true).with_rate_gate(vike_okx::ratelimit::rest_rate_gate());
    let info = transport
        .public(REST, PATH_INSTRUMENTS, &[("instType", "SWAP".into()), ("instId", SYMBOL.into())])
        .expect("public instruments");
    let inst = parse_okx_perp_instruments(
        &vike_okx::transport::unwrap_okx(info)
            .map(|d| json!({ "data": d }))
            .expect("instruments data"),
    )[SYMBOL]
        .clone();

    let rest = OkxPerpRest {
        signer: OkxV5Signer::new(&creds, now_ms),
        transport: UreqOkxTransport::new(true)
            .with_rate_gate(vike_okx::ratelimit::rest_rate_gate()),
        base_url: REST.to_string(),
        symbol: SYMBOL.to_string(),
        properties: inst.properties,
        ct_val: inst.ct_val,
        leverage: 2.0,
        broker_code: None,
    };
    rest.set_leverage().expect("set_leverage");
    let mark = rest.last_price().expect("ticker");
    let snap0 = rest.reconcile_positions().expect("reconcile");

    let engine = ExecutionEngine::new(
        Account::new(1.0, "okx", None, BalanceMode::Delta),
        RiskGate::new(RiskLimits::new()),
        LiveRestClient::new(rest),
        "okx",
        SYMBOL,
    );
    let handle = spawn_core(engine, CoreConfig::default());
    let cell = handle.snapshot_cell();
    handle.try_command(Command::ApplySnapshot(Box::new(snap0))).unwrap();
    let ready = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let feed = spawn_okx_perp_user_data(
        DEMO_WS.to_string(),
        creds.api_key.clone(),
        creds.api_secret.clone(),
        creds.passphrase.clone().unwrap_or_default(),
        SYMBOL.to_string(),
        inst.ct_val,
        handle.event_sender(),
        ready.clone(),
    );
    wait_pump_ready(&ready);

    let leg_base = inst.properties.min_qty * inst.ct_val; // one contract in BASE units
    let px0 = mark * 0.5; // far below market → the BUY rests, never fills
    let coid = format!("vtAmdOkx{}", now_ms() % 100_000_000);
    submit_limit(&handle, &coid, 1, leg_base, px0);
    let resting = wait_status(&cell, &coid, OrderStatus::Accepted);
    tracing::info!(target: "vike_okx", "resting limit: qty={} px={:?}", resting.qty, resting.price);

    // native modify: double the size, nudge the price up (still well below market)
    let new_qty = leg_base * 2.0;
    let new_px = mark * 0.6;
    handle
        .try_command(Command::Order(OrderIntent::Modify {
            client_order_id: coid.clone(),
            new_qty: Some(new_qty),
            new_price: Some(new_px),
        }))
        .unwrap();

    // wait for the OrderModified (native /amend-order ack) to fold: resting qty grows
    let deadline = Instant::now() + Duration::from_secs(15);
    loop {
        let snap = cell.load_full();
        let o = snap.orders.iter().find(|o| o.client_order_id == coid).expect("order present");
        if o.qty > resting.qty * 1.5 {
            tracing::info!(target: "vike_okx", "MODIFIED: qty {} -> {} px {:?} -> {:?}", resting.qty, o.qty, resting.price, o.price);
            assert!(o.price.unwrap_or(0.0) > px0, "price moved up on modify");
            break;
        }
        assert!(Instant::now() < deadline, "modify never folded; recent: {:?}", snap.recent_events);
        std::thread::sleep(Duration::from_millis(250));
    }

    handle.try_command(Command::Order(OrderIntent::Cancel(coid))).unwrap();
    std::thread::sleep(Duration::from_secs(1));
    assert!(cell.load().fault.is_none());
    feed.shutdown().expect("okx user-data shutdown");
    handle.shutdown_and_join();
    tracing::info!(target: "vike_okx", "okx native-modify green: rest -> modify (qty+px) -> cancel -> teardown");
}
