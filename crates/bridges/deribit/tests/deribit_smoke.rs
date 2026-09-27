//! R6 slice-6/7 LIVE Deribit smoke (network + testnet creds — run manually):
//!     cargo test -p vike-deribit --test deribit_smoke -- --ignored --nocapture
//!
//! The options ladder on test.deribit.com: pick the most liquid BTC option (book
//! summary: highest open interest with a live ask) → authed order-WS connect →
//! reconcile → user.trades feed attach → far-below LIMIT rest + cancel + idempotent
//! re-cancel → MARKET BUY min amount filled via the WS (post_only:false trap live) →
//! MARKET SELL reduce-only close → teardown.

use std::time::{Duration, Instant};

use serde_json::json;
use vike_bridge_core::credentials::{
    Environment, load_credentials_from, load_workspace_dotenv_from,
};
use vike_bridge_core::rest::LiveRestClient;
use vike_bridge_core::transport::{RestTransport, UreqTransport};
use vike_core::{CoreConfig, spawn_core};
use vike_deribit::client::{DeribitRest, parse_deribit_option_instruments};
use vike_deribit::transport::{DeribitOrderTransport, TESTNET_REST, TESTNET_WS};
use vike_deribit::user_data::spawn_deribit_user_data;
use vike_exec::{
    Account, BalanceMode, Command, ExecutionEngine, OrderIntent, OrderStatus, RiskGate, RiskLimits,
};

use vike_model::clock::now_ms;

fn wait_status(
    cell: &std::sync::Arc<arc_swap::ArcSwap<vike_core::CoreSnapshot>>,
    coid: &str,
    accept: &[OrderStatus],
    secs: u64,
) -> vike_core::OrderView {
    let deadline = Instant::now() + Duration::from_secs(secs);
    loop {
        let snap = cell.load_full();
        if let Some(o) = snap.orders.iter().find(|o| o.client_order_id == coid) {
            if accept.contains(&o.status) {
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
            "{coid} never reached {accept:?}; recent: {:?}",
            cell.load().recent_events
        );
        std::thread::sleep(Duration::from_millis(250));
    }
}

#[test]
#[ignore = "network + testnet creds — run manually (see module doc)"]
fn deribit_options_ladder() {
    vike_log::test_init();
    let vars = load_workspace_dotenv_from(std::env::var("VIKE_SETTINGS_DIR").ok().as_deref());
    let Some(creds) = load_credentials_from("deribit", Environment::Demo, &vars) else {
        tracing::warn!(target: "vike_deribit", "SKIP: DERIBIT_DEMO creds absent");
        return;
    };

    // most liquid BTC option: highest open interest with a live ask (one REST call)
    let public = UreqTransport::new("deribit");
    let book = public
        .public(
            TESTNET_REST,
            "/api/v2/public/get_book_summary_by_currency",
            &[("currency", "BTC".into()), ("kind", "option".into())],
        )
        .expect("book summary");
    let mut best: Option<(String, f64, f64)> = None; // (name, oi, ask)
    for row in book["result"].as_array().unwrap_or(&vec![]) {
        let Some(ask) = row.get("ask_price").and_then(|a| a.as_f64()) else { continue };
        let oi = row.get("open_interest").and_then(|o| o.as_f64()).unwrap_or(0.0);
        let name = row.get("instrument_name").and_then(|n| n.as_str()).unwrap_or("");
        if ask > 0.0 && !name.is_empty() && best.as_ref().is_none_or(|(_, b_oi, _)| oi > *b_oi) {
            best = Some((name.to_string(), oi, ask));
        }
    }
    let (symbol, oi, ask) = best.expect("a live-ask BTC option on testnet");
    tracing::info!(target: "vike_deribit", "instrument: {symbol} (oi={oi}, ask={ask})");

    let info = public
        .public(
            TESTNET_REST,
            "/api/v2/public/get_instruments",
            &[("currency", "BTC".into()), ("kind", "option".into())],
        )
        .expect("instruments");
    let inst = parse_deribit_option_instruments(&info)[&symbol].clone();
    tracing::info!(target: "vike_deribit", "properties: tick={} step={}", inst.properties.tick_size, inst.properties.step_size);

    // authed order transport (JSON-RPC over WS — order entry rides the socket)
    let mut transport =
        DeribitOrderTransport::new(TESTNET_WS, &creds.api_key, &creds.api_secret, None);
    transport.connect().expect("order-WS auth");
    let rest = DeribitRest::new(transport, &symbol, inst.properties, "BTC");
    let snap0 = rest.connect().expect("reconcile");
    tracing::info!(target: "vike_deribit", "reconcile: positions={:?} open_orders={}", snap0.positions, snap0.open_orders.len());
    let base = snap0.positions.first().map_or(0.0, |(_, q)| *q);

    let engine = ExecutionEngine::new(
        Account::new(1.0, "deribit", None, BalanceMode::Delta),
        RiskGate::new(RiskLimits::new()),
        LiveRestClient::new(rest),
        "deribit",
        &symbol,
    );
    let handle = spawn_core(engine, CoreConfig::default());
    let cell = handle.snapshot_cell();
    handle.try_command(Command::ApplySnapshot(Box::new(snap0))).unwrap();
    let feed = spawn_deribit_user_data(
        TESTNET_WS.to_string(),
        creds.api_key.clone(),
        creds.api_secret.clone(),
        symbol.clone(),
        handle.event_sender(),
    );
    std::thread::sleep(Duration::from_secs(3)); // auth+subscribe settles

    let qty = inst.properties.min_qty; // 0.1 BTC-amount for BTC options
    let submit = |coid: &str, side: i32, order_type: &str, price: Option<f64>, reduce: bool| {
        let req: vike_model::OrderRequest = serde_json::from_value(json!({
            "client_order_id": coid, "venue": "deribit", "symbol": symbol,
            "side": side, "qty": qty, "order_type": order_type, "price": price,
            "reduce_only": reduce, "ts": now_ms()
        }))
        .unwrap();
        handle.try_command(Command::Order(OrderIntent::Submit(Box::new(req)))).unwrap();
    };

    // resting LIMIT far below the ask, then cancel + idempotent re-cancel.
    // NOTE: option ticks are TIERED (tick_size_steps: 0.0005 above 0.005) — rest below
    // the tier boundary so the base tick_size applies (the instruments parser carries
    // only the base tick, matching Python).
    let rest_coid = format!("vtr6dbR{}", now_ms() % 100_000_000);
    let low_px = (ask * 0.25).min(0.004).max(inst.properties.tick_size);
    submit(&rest_coid, 1, "limit", Some(low_px), false);
    wait_status(&cell, &rest_coid, &[OrderStatus::Accepted], 15);
    tracing::info!(target: "vike_deribit", "limit resting @ {low_px}");
    handle.try_command(Command::Order(OrderIntent::Cancel(rest_coid.clone()))).unwrap();
    std::thread::sleep(Duration::from_secs(2));
    handle.try_command(Command::Order(OrderIntent::Cancel(rest_coid.clone()))).unwrap(); // idempotent
    std::thread::sleep(Duration::from_secs(1));
    assert!(cell.load().fault.is_none(), "re-cancel must be swallowed");

    // MARKET BUY -> fill arrives on user.trades (post_only:false live)
    let buy_coid = format!("vtr6dbB{}", now_ms() % 100_000_000);
    submit(&buy_coid, 1, "market", None, false);
    let buy = wait_status(&cell, &buy_coid, &[OrderStatus::Filled], 25);
    tracing::info!(target: "vike_deribit", "BUY filled: {} @ {} (fee tracked)", buy.filled_qty, buy.avg_fill_px);
    assert!(buy.avg_fill_px > 0.0);
    let pos = cell.load().positions.first().map(|p| p.size).unwrap_or(0.0);
    assert!(pos > base, "fill folded: {base} -> {pos}");

    // MARKET SELL reduce-only close
    let sell_coid = format!("vtr6dbS{}", now_ms() % 100_000_000);
    submit(&sell_coid, -1, "market", None, true);
    wait_status(&cell, &sell_coid, &[OrderStatus::Filled], 25);
    let fin = cell.load().positions.first().map(|p| p.size).unwrap_or(0.0);
    tracing::info!(
        target: "vike_deribit",
        "final: size={fin} realized={} fees={}",
        cell.load().portfolio.realized_pnl,
        cell.load().portfolio.fees_paid
    );
    assert!(
        (fin - base).abs() < inst.properties.step_size * 0.5,
        "round trip must flatten: {base} -> {fin}"
    );
    assert!(cell.load().fault.is_none());

    feed.shutdown().expect("deribit user-data shutdown");
    handle.shutdown_and_join();
    tracing::info!(target: "vike_deribit", "deribit ladder green: auth -> reconcile -> rest/cancel -> market fill via user.trades -> close -> teardown");
}
