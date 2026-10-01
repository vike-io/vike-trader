//! R6 slice-1 LIVE demo smoke (network + creds — run manually):
//!     cargo test -p vike-binance --test binance_demo_smoke -- --ignored --nocapture
//!
//! The ladder: server-time offset → signed account → reconcile connect() → place a
//! far-from-market LIMIT (formatted to tick/step) → verify it RESTS in openOrders →
//! cancel → verify gone → cancel again (idempotent: -2011 swallowed, the "unknown ≠
//! rejection" gate). Credentials come from the Python app's gitignored .env
//! (BINANCE_DEMO_API_KEY/SECRET) — never printed, never committed.

use vike_binance::spot::{
    BinanceSpotRest, DEMO_REST, PATH_EXCHANGE_INFO, PATH_OPEN_ORDERS, parse_symbol_properties,
};
use vike_bridge_core::credentials::{
    Environment, load_credentials_from, load_workspace_dotenv_from,
};
use vike_bridge_core::signer::BinanceHmacSigner;
use vike_bridge_core::transport::{RestTransport, UreqTransport};
use vike_model::events::Event;

use vike_model::clock::now_ms;

#[test]
#[ignore = "network + demo creds — run manually (see module doc)"]
fn binance_spot_demo_ladder() {
    vike_log::test_init();
    let vars = load_workspace_dotenv_from(std::env::var("VIKE_SETTINGS_DIR").ok().as_deref());
    let Some(creds) = load_credentials_from("binance", Environment::Demo, &vars) else {
        tracing::warn!(target: "vike_binance", "SKIP: BINANCE_DEMO creds absent");
        return;
    };

    let transport =
        UreqTransport::new("binance").with_rate_gate(vike_binance::ratelimit::spot_rest_gate());
    let signer = BinanceHmacSigner::new(&creds, now_ms);

    // exchangeInfo -> real symbol properties
    let info = transport
        .public(DEMO_REST, PATH_EXCHANGE_INFO, &[("symbol", "BTCUSDT".into())])
        .expect("exchangeInfo");
    let properties = parse_symbol_properties(&info)["BTCUSDT"];
    tracing::info!(
        target: "vike_binance",
        "properties: tick={} step={} min_notional={}",
        properties.tick_size, properties.step_size, properties.min_notional
    );

    let client = BinanceSpotRest {
        link_id: None,
        signer,
        transport,
        base_url: DEMO_REST.to_string(),
        symbol: "BTCUSDT".to_string(),
        properties,
        base_asset: "BTC".to_string(),
    };

    // clock skew -> signer offset (the -1021 killer)
    let offset = client.server_time_offset(now_ms()).expect("server time");
    client.signer.set_offset_ms(offset);
    tracing::info!(target: "vike_binance", "server-time offset: {offset} ms");

    // reconcile: balances + open orders + mark
    let snap = client.connect().expect("connect/reconcile");
    tracing::info!(
        target: "vike_binance",
        "reconcile: position={:?} open_orders={} avg_px={:?}",
        snap.positions, snap.open_orders.len(), snap.position_avg_px
    );
    let mark = snap.position_avg_px[0].1;
    assert!(mark > 0.0, "ticker mark must be live");

    // far-from-market LIMIT BUY that must REST: half the mark, notional >= min
    let limit_px = mark * 0.5;
    let qty = (properties.min_notional.max(5.0) * 1.6) / limit_px;
    let coid = format!("vtr6smoke{}", now_ms() % 100_000_000);
    let request: vike_model::OrderRequest = serde_json::from_value(serde_json::json!({
        "client_order_id": coid, "venue": "binance", "symbol": "BTCUSDT",
        "side": 1, "qty": qty, "order_type": "limit", "price": limit_px,
        "ts": now_ms()
    }))
    .unwrap();
    let events = client.submit_order(&request);
    tracing::debug!(target: "vike_binance", "submit events: {events:?}");
    assert!(
        matches!(events.last(), Some(Event::OrderAccepted(_))),
        "demo order must be ACCEPTED: {events:?}"
    );

    // it rests: openOrders contains our coid
    let open = client
        .transport
        .signed(DEMO_REST, PATH_OPEN_ORDERS, "GET", &[("symbol", "BTCUSDT".into())], &client.signer)
        .expect("openOrders");
    let resting = open
        .as_array()
        .unwrap()
        .iter()
        .any(|o| o.get("clientOrderId").and_then(|c| c.as_str()) == Some(coid.as_str()));
    assert!(resting, "order {coid} must rest in openOrders");
    tracing::info!(target: "vike_binance", "order {coid} is resting");

    // cancel + verify gone + idempotent re-cancel (-2011 swallowed)
    client.cancel_order(&coid).expect("cancel");
    let open = client
        .transport
        .signed(DEMO_REST, PATH_OPEN_ORDERS, "GET", &[("symbol", "BTCUSDT".into())], &client.signer)
        .expect("openOrders after cancel");
    let still = open
        .as_array()
        .unwrap()
        .iter()
        .any(|o| o.get("clientOrderId").and_then(|c| c.as_str()) == Some(coid.as_str()));
    assert!(!still, "order {coid} must be gone after cancel");
    client.cancel_order(&coid).expect("re-cancel must be idempotent (unknown != rejection)");
    tracing::info!(target: "vike_binance", "cancel + idempotent re-cancel OK — demo ladder green");
}
