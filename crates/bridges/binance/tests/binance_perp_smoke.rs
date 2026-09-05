//! R6 slice-3 LIVE perp smoke (network + demo creds — run manually):
//!     cargo test -p vike-binance --test binance_perp_smoke -- --ignored --nocapture
//!
//! The fapi ladder, live: set_leverage → reconcile → listenKey WS attach → MARKET BUY
//! (long) filled via ORDER_TRADE_UPDATE → MARKET SELL 2× (FLIPS TO A REAL SHORT — the
//! signed-negative reconcile gate, live) → reduce-only BUY back to flat → teardown.

use std::time::{Duration, Instant};

use vike_binance::perp::{
    BinancePerpRest, DEMO_FAPI_REST, DEMO_FAPI_WS, PATH_EXCHANGE_INFO,
    parse_binance_perp_instruments,
};
use vike_binance::perp_user_data::spawn_binance_perp_user_data;
use vike_bridge_core::credentials::{
    Environment, load_credentials_from, load_workspace_dotenv_from,
};
use vike_bridge_core::rest::LiveRestClient;
use vike_bridge_core::signer::BinanceHmacSigner;
use vike_bridge_core::transport::{RestTransport, UreqTransport};
use vike_core::{CoreConfig, spawn_core};
use vike_exec::{
    Account, BalanceMode, Command, ExecutionEngine, OrderIntent, OrderStatus, RiskGate, RiskLimits,
};

use vike_model::clock::now_ms;

fn submit_market(handle: &vike_core::CoreHandle, coid: &str, side: i32, qty: f64, reduce: bool) {
    let req: vike_model::OrderRequest = serde_json::from_value(serde_json::json!({
        "client_order_id": coid, "venue": "binance", "symbol": "BTCUSDT",
        "side": side, "qty": qty, "order_type": "market", "reduce_only": reduce,
        "ts": now_ms()
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
fn perp_ladder_long_short_flat() {
    vike_log::test_init();
    let vars = load_workspace_dotenv_from(std::env::var("VIKE_SETTINGS_DIR").ok().as_deref());
    let Some(creds) = load_credentials_from("binance", Environment::Demo, &vars) else {
        tracing::warn!(target: "vike_binance", "SKIP: BINANCE_DEMO creds absent");
        return;
    };

    let transport =
        UreqTransport::new("binance").with_rate_gate(vike_binance::ratelimit::perp_rest_gate());
    let info = transport
        .public(DEMO_FAPI_REST, PATH_EXCHANGE_INFO, &[("symbol", "BTCUSDT".into())])
        .expect("fapi exchangeInfo");
    let inst = parse_binance_perp_instruments(&info)["BTCUSDT"].clone();
    tracing::info!(
        target: "vike_binance",
        "fapi properties: tick={} step={} min_notional={}",
        inst.properties.tick_size, inst.properties.step_size, inst.properties.min_notional
    );

    let rest = BinancePerpRest {
        link_id: None,
        signer: BinanceHmacSigner::new(&creds, now_ms),
        transport,
        base_url: DEMO_FAPI_REST.to_string(),
        symbol: "BTCUSDT".to_string(),
        properties: inst.properties,
        leverage: 2.0,
    };
    rest.set_leverage().expect("set_leverage (idempotent 200)");
    let snap0 = rest.reconcile_positions().expect("reconcile");
    tracing::info!(target: "vike_binance", "reconcile: positions={:?} balance={}", snap0.positions, snap0.balance);
    assert!(snap0.balance > 0.0, "fapi USDT wallet balance must be live");
    let base = snap0.positions.first().map_or(0.0, |(_, q)| *q);

    // a second REST client for direct reconcile probes (the first moves into the core)
    let probe = BinancePerpRest {
        link_id: None,
        signer: BinanceHmacSigner::new(&creds, now_ms),
        transport: UreqTransport::new("binance")
            .with_rate_gate(vike_binance::ratelimit::perp_rest_gate()),
        base_url: DEMO_FAPI_REST.to_string(),
        symbol: "BTCUSDT".to_string(),
        properties: inst.properties,
        leverage: 2.0,
    };

    let engine = ExecutionEngine::new(
        Account::new(1.0, "binance", None, BalanceMode::Delta),
        RiskGate::new(RiskLimits::new()),
        LiveRestClient::new(rest),
        "binance",
        "BTCUSDT",
    );
    let handle = spawn_core(engine, CoreConfig::default());
    let cell = handle.snapshot_cell();
    handle.try_command(Command::ApplySnapshot(Box::new(snap0))).unwrap();
    let feed = spawn_binance_perp_user_data(
        DEMO_FAPI_REST.to_string(),
        DEMO_FAPI_WS.to_string(),
        creds.api_key.clone(),
        "BTCUSDT".to_string(),
        handle.event_sender(),
    );
    std::thread::sleep(Duration::from_secs(3)); // listenKey connect settles

    // min_notional (100 USDT) at ~62k with step 0.001 -> 0.002 BTC per leg
    let qty = 0.002;

    // LONG leg
    let b1 = format!("vtr6pB{}", now_ms() % 100_000_000);
    submit_market(&handle, &b1, 1, qty, false);
    let buy = wait_filled(&cell, &b1);
    tracing::info!(target: "vike_binance", "LONG filled: {} @ {}", buy.filled_qty, buy.avg_fill_px);
    assert!(position_size(&cell) > base, "long folded");

    // FLIP to SHORT: sell 2x
    let s1 = format!("vtr6pS{}", now_ms() % 100_000_000);
    submit_market(&handle, &s1, -1, qty * 2.0, false);
    let sell = wait_filled(&cell, &s1);
    tracing::info!(target: "vike_binance", "FLIP filled: {} @ {}", sell.filled_qty, sell.avg_fill_px);
    let flipped = position_size(&cell);
    assert!(
        flipped < base - qty * 0.5,
        "position must be net SHORT of base: base={base} now={flipped}"
    );

    // THE live SHORT-reconcile gate: venue positionAmt is signed-negative
    let live = probe.reconcile_positions().expect("live reconcile while short");
    tracing::info!(target: "vike_binance", "LIVE SHORT reconcile: {:?}", live.positions);
    assert!(
        live.positions.iter().any(|(_, q)| *q < 0.0),
        "venue reconcile must report the signed SHORT: {:?}",
        live.positions
    );

    // reduce-only BUY back to flat vs base
    let b2 = format!("vtr6pF{}", now_ms() % 100_000_000);
    submit_market(&handle, &b2, 1, qty, false);
    wait_filled(&cell, &b2);
    let fin = position_size(&cell);
    tracing::info!(
        target: "vike_binance",
        "final: size={fin} realized={} fees={} funding={}",
        cell.load().portfolio.realized_pnl,
        cell.load().portfolio.fees_paid,
        cell.load().portfolio.funding_paid
    );
    assert!(
        (fin - base).abs() < inst.properties.step_size * 1.5,
        "round trip must flatten: base={base} final={fin}"
    );
    assert!(cell.load().portfolio.realized_pnl != 0.0, "the flip realizes PnL");
    assert!(cell.load().fault.is_none());

    feed.shutdown().expect("perp user-data shutdown");
    handle.shutdown_and_join();
    tracing::info!(target: "vike_binance", "perp ladder green: leverage -> long -> SHORT (live signed reconcile) -> flat -> teardown");
}
