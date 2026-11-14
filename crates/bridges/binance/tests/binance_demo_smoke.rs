//! R6 slice-1 LIVE demo smoke (network + creds — run manually):
//!     cargo test -p vike-binance --test binance_demo_smoke -- --ignored --nocapture
//!
//! The ladder: server-time offset → the reconcile read production runs (the spot `ReconcileClient`
//! from `vike_binance::reconcile`: the USDT balance and the open-order reports; a spot lane
//! reports no position) and the ticker mark → place a far-from-market LIMIT (formatted to
//! tick/step) → verify it RESTS in openOrders → cancel → verify gone → cancel again (idempotent:
//! -2011 swallowed, the "unknown ≠ rejection" gate). Credentials are the default DEMO
//! account's `API_KEY`/`API_SECRET` in the credential store — never printed, never committed.

use std::assert_matches;
use vike_binance::execution::BinanceUrls;
use vike_binance::execution::rest::spot::{
    PATH_EXCHANGE_INFO, PATH_OPEN_ORDERS, VenueRestBinanceSpot, parse_symbol_properties,
};
use vike_binance::family::execution::spot_fee::ticker_price;
use vike_binance::family::signer::BinanceHmacSigner;
use vike_bridge_core::transport::{RestTransport, UreqTransport};
use vike_model::AccountTier;
use vike_model::accounts::account_keys::AccountLabel;
use vike_model::events::Event;
use vike_secrets::credentials::api_credentials;
use vike_venues::venue_urls::VenueUrls;

use vike_model::now_ms;

#[test]
#[ignore = "network + demo creds — run manually (see module doc)"]
fn binance_spot_demo_ladder() {
    vike_log::test_init();
    let directory = vike_secrets::smoke_directory();
    let account = directory.credentials_for("binance", &AccountLabel::Default, AccountTier::Demo);
    let Some(creds) = api_credentials(&account, "binance") else {
        tracing::warn!(target: "vike_binance", "SKIP: BINANCE_DEMO creds absent");
        return;
    };

    // The demo hosts: the release's `venue_url` rows (`crates/vike-venues/src/venue_urls.rs`'s `VENUE_URL_SEED`).
    let hosts = BinanceUrls::resolve(&VenueUrls::seed("binance"), AccountTier::Demo)
        .expect("the seed holds every binance row");
    let demo_rest = hosts.spot_rest;

    let transport = UreqTransport::new("binance")
        .with_rate_gate(vike_binance::transport::ratelimit::spot_rest_gate());
    let signer = BinanceHmacSigner::new(&creds, now_ms);

    // exchangeInfo -> real symbol properties
    let info = transport
        .public(demo_rest, PATH_EXCHANGE_INFO, &[("symbol", "BTCUSDT".into())])
        .expect("exchangeInfo");
    let properties = parse_symbol_properties(&info)["BTCUSDT"];
    tracing::info!(
        target: "vike_binance",
        "properties: tick={} step={} min_notional={}",
        properties.tick_size, properties.step_size, properties.min_notional
    );

    let client = VenueRestBinanceSpot {
        link_id: None,
        signer,
        transport,
        base_url: demo_rest.to_string(),
        symbol: "BTCUSDT".to_string(),
        properties,
        base_asset: "BTC".to_string(),
    };

    // clock skew -> signer offset (the -1021 killer)
    let offset = client.server_time_offset(now_ms()).expect("server time");
    client.signer.set_offset_ms(offset);
    tracing::info!(target: "vike_binance", "server-time offset: {offset} ms");

    // reconcile, as production reads it: balance + open orders (a spot lane reports no position)
    let recon = vike_binance::recon_client(&creds, "BTCUSDT", hosts)
        .expect("binance recon_client is always Some");
    let balance = recon.fetch_balance().expect("fetch_balance (spot)");
    let open_orders = recon.fetch_order_status_reports(0).expect("fetch_order_status_reports");
    let positions = recon.fetch_position_status_reports().expect("fetch_position_status_reports");
    tracing::info!(
        target: "vike_binance",
        "reconcile: USDT balance={balance:?} open_orders={}",
        open_orders.len()
    );
    // the spot rule: the mounted pair's base coin, a `Cash` floor row with no price
    assert_eq!(positions.len(), 1, "a spot lane reports its pair's wallet: {positions:?}");
    assert_eq!(positions[0].margin_mode, vike_model::MarginMode::Cash, "{positions:?}");
    let mark =
        ticker_price(&client.transport, demo_rest, "BTCUSDT").expect("ticker mark must be live");

    // far-from-market LIMIT BUY that must REST: half the mark, notional >= min
    let limit_px = mark * 0.5;
    let qty = (properties.min_notional.max(5.0) * 1.6) / limit_px;
    let order_client_id = format!("vtr6smoke{}", now_ms() % 100_000_000);
    let request: vike_model::OrderRequest = serde_json::from_value(serde_json::json!({
        "order_client_id": order_client_id, "venue": "binance", "symbol": "BTCUSDT",
        "side": 1, "qty": qty, "order_type": "limit", "price": limit_px,
        "ts": now_ms()
    }))
    .unwrap();
    let events = client.submit_order(&request);
    tracing::debug!(target: "vike_binance", "submit events: {events:?}");
    assert_matches!(
        events.last(),
        Some(Event::OrderAccepted(_)),
        "demo order must be ACCEPTED: {events:?}"
    );

    // it rests: openOrders contains our order client id
    let open = client
        .transport
        .signed(demo_rest, PATH_OPEN_ORDERS, "GET", &[("symbol", "BTCUSDT".into())], &client.signer)
        .expect("openOrders");
    let resting =
        open.as_array().unwrap().iter().any(|o| {
            o.get("clientOrderId").and_then(|c| c.as_str()) == Some(order_client_id.as_str())
        });
    assert!(resting, "order {order_client_id} must rest in openOrders");
    tracing::info!(target: "vike_binance", "order {order_client_id} is resting");

    // cancel + verify gone + idempotent re-cancel (-2011 swallowed)
    client.cancel_order(&order_client_id).expect("cancel");
    let open = client
        .transport
        .signed(demo_rest, PATH_OPEN_ORDERS, "GET", &[("symbol", "BTCUSDT".into())], &client.signer)
        .expect("openOrders after cancel");
    let still =
        open.as_array().unwrap().iter().any(|o| {
            o.get("clientOrderId").and_then(|c| c.as_str()) == Some(order_client_id.as_str())
        });
    assert!(!still, "order {order_client_id} must be gone after cancel");
    client
        .cancel_order(&order_client_id)
        .expect("re-cancel must be idempotent (unknown != rejection)");
    tracing::info!(target: "vike_binance", "cancel + idempotent re-cancel OK — demo ladder green");
}
