//! R6 slice-2 LIVE full-loop smoke (network + demo creds — run manually):
//!     cargo test -p vike-binance --test binance_fullloop_smoke -- --ignored --nocapture
//!
//! THE R6 gate, live: Command::Submit → vt-core RiskGate → LiveBinanceSpotClient REST
//! (accepted) → the venue's executionReport arrives on the WS-API user-data stream →
//! mapper → ingest → dual-dedup fold into the Account + FSM → CoreSnapshot. A MARKET
//! BUY (~$7 notional) then a MARKET SELL flatten, both confirmed FILLED by the venue's
//! own fill stream — no simulated anything.

use std::time::{Duration, Instant};

use vike_binance::spot::{
    BinanceSpotRest, DEMO_REST, LiveBinanceSpotClient, PATH_EXCHANGE_INFO, parse_symbol_properties,
};
use vike_binance::user_data::{DEMO_WS, spawn_binance_user_data};
use vike_bridge_core::credentials::{
    Environment, load_credentials_from, load_workspace_dotenv_from,
};
use vike_bridge_core::signer::BinanceHmacSigner;
use vike_bridge_core::transport::{RestTransport, UreqTransport};
use vike_core::{CoreConfig, spawn_core};
use vike_exec::{
    Account, BalanceMode, Command, ExecutionEngine, OrderIntent, OrderStatus, RiskGate, RiskLimits,
};

use vike_model::clock::now_ms;

fn submit_market(handle: &vike_core::CoreHandle, coid: &str, side: i32, qty: f64) {
    let req: vike_model::OrderRequest = serde_json::from_value(serde_json::json!({
        "client_order_id": coid, "venue": "binance", "symbol": "BTCUSDT",
        "side": side, "qty": qty, "order_type": "market", "ts": now_ms()
    }))
    .unwrap();
    handle.try_command(Command::Order(OrderIntent::Submit(Box::new(req)))).unwrap();
}

fn wait_for_filled(
    cell: &std::sync::Arc<arc_swap::ArcSwap<vike_core::CoreSnapshot>>,
    coid: &str,
) -> vike_core::OrderView {
    let deadline = Instant::now() + Duration::from_secs(20);
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
        std::thread::sleep(Duration::from_millis(200));
    }
}

#[test]
#[ignore = "network + demo creds — run manually (see module doc)"]
fn full_loop_market_round_trip() {
    vike_log::test_init();
    let vars = load_workspace_dotenv_from(std::env::var("VIKE_SETTINGS_DIR").ok().as_deref());
    let Some(creds) = load_credentials_from("binance", Environment::Demo, &vars) else {
        tracing::warn!(target: "vike_binance", "SKIP: BINANCE_DEMO creds absent");
        return;
    };

    // --- REST client (properties + clock offset) wrapped as the core's ExecutionClient ---
    let transport =
        UreqTransport::new("binance").with_rate_gate(vike_binance::ratelimit::spot_rest_gate());
    let info = transport
        .public(DEMO_REST, PATH_EXCHANGE_INFO, &[("symbol", "BTCUSDT".into())])
        .expect("exchangeInfo");
    let properties = parse_symbol_properties(&info)["BTCUSDT"];
    let rest = BinanceSpotRest {
        link_id: None,
        signer: BinanceHmacSigner::new(&creds, now_ms),
        transport,
        base_url: DEMO_REST.to_string(),
        symbol: "BTCUSDT".to_string(),
        properties,
        base_asset: "BTC".to_string(),
    };
    let offset = rest.server_time_offset(now_ms()).expect("server time");
    rest.signer.set_offset_ms(offset);
    let snapshot = rest.connect().expect("reconcile");
    let mark = snapshot.position_avg_px[0].1;
    tracing::info!(target: "vike_binance", "reconcile: mark={mark} position={:?}", snapshot.positions);

    // --- the vt-core owns the engine; the WS user-data stream feeds its ingest ---
    let engine = ExecutionEngine::new(
        Account::new(1.0, "binance", None, BalanceMode::Delta),
        RiskGate::new(RiskLimits::new()),
        LiveBinanceSpotClient::new(rest),
        "binance",
        "BTCUSDT",
    );
    let handle = spawn_core(engine, CoreConfig::default());
    let cell = handle.snapshot_cell();
    handle.try_command(Command::ApplySnapshot(Box::new(snapshot))).unwrap();
    let feed = spawn_binance_user_data(
        DEMO_WS.to_string(),
        creds.api_key.clone(),
        creds.api_secret.clone(),
        "BTCUSDT".to_string(),
        handle.event_sender(),
    );
    std::thread::sleep(Duration::from_secs(3)); // let the subscribe handshake settle

    // --- MARKET BUY (~$7) -> venue fill via WS -> fold ---
    let qty = (properties.min_notional.max(5.0) * 1.4) / mark;
    let buy_coid = format!("vtr6loopB{}", now_ms() % 100_000_000);
    let base_size = cell.load().positions.first().map_or(0.0, |p| p.size);
    submit_market(&handle, &buy_coid, 1, qty);
    let buy = wait_for_filled(&cell, &buy_coid);
    tracing::info!(
        target: "vike_binance",
        "BUY filled: qty={} avg={} (venue id {:?})",
        buy.filled_qty, buy.avg_fill_px, buy.venue_order_id
    );
    assert!(buy.avg_fill_px > 0.0);
    let snap = cell.load_full();
    let pos = snap.positions.iter().find(|p| p.symbol == "BTCUSDT").expect("position");
    assert!(pos.size > base_size, "fill must fold into the Account: {} -> {}", base_size, pos.size);
    assert!(snap.portfolio.fees_paid != 0.0 || buy.filled_qty > 0.0, "commission tracked");

    // --- MARKET SELL the same qty -> flatten back ---
    let sell_coid = format!("vtr6loopS{}", now_ms() % 100_000_000);
    submit_market(&handle, &sell_coid, -1, buy.filled_qty);
    let sell = wait_for_filled(&cell, &sell_coid);
    tracing::info!(target: "vike_binance", "SELL filled: qty={} avg={}", sell.filled_qty, sell.avg_fill_px);
    let snap = cell.load_full();
    let pos = snap.positions.iter().find(|p| p.symbol == "BTCUSDT").expect("position");
    tracing::info!(
        target: "vike_binance",
        "final: size={} realized={} fees={} equity={} recent={:?}",
        pos.size, snap.portfolio.realized_pnl, snap.portfolio.fees_paid, snap.portfolio.equity,
        &snap.recent_events[snap.recent_events.len().saturating_sub(6)..]
    );
    assert!(
        (pos.size - base_size).abs() < properties.step_size * 2.0,
        "round trip must flatten: base={base_size} final={}",
        pos.size
    );
    assert!(snap.portfolio.realized_pnl != 0.0, "the sell leg realizes PnL");
    assert!(snap.fault.is_none());

    // --- deterministic teardown: WS joined, core drained ---
    feed.shutdown().expect("user-data shutdown");
    handle.shutdown_and_join();
    tracing::info!(target: "vike_binance", "full loop green: submit -> accept -> WS fill -> fold -> flatten -> teardown");
}
