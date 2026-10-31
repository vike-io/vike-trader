//! post-only LIVE demo smokes (network + creds — run manually):
//!     cargo test -p vike-binance --test binance_post_only_smoke -- --ignored --nocapture
//!
//! Proves the binance post-only wire on BOTH lanes against the real demo venues, with the venue's OWN
//! records as the witness:
//!
//! * `binance_perp_post_only_ladder` (USD-M futures, `timeInForce=GTX`): a far-below post-only LIMIT BUY
//!   rests and its record says `GTX`; one priced ABOVE the ask would take liquidity and is REFUSED by the
//!   venue (`-5022`), so the submit answers a terminal `OrderRejected` with the typed cause
//!   `PostOnlyWouldCross`, nothing executes and nothing rests; a post-only MARKET order is denied locally.
//! * `binance_spot_post_only_ladder` (spot, `type=LIMIT_MAKER` and NO `timeInForce`): the same, the crossing
//!   one refused with `-2010 "Order would immediately match and take."`.
//!
//! Double-gated like the other smokes: `#[ignore]` + self-skip when the demo account's creds are absent.

use std::assert_matches;
use vike_binance::perp::{
    BinancePerpRest, DEMO_FAPI_REST, PATH_EXCHANGE_INFO as FAPI_EXCHANGE_INFO,
    PATH_OPEN_ORDERS as FAPI_OPEN_ORDERS, PATH_ORDER as FAPI_ORDER, parse_binance_perp_instruments,
};
use vike_binance::spot::{
    BinanceSpotRest, DEMO_REST, PATH_EXCHANGE_INFO, PATH_OPEN_ORDERS, PATH_ORDER,
    parse_symbol_properties,
};
use vike_bridge_core::rest::VenueRest;
use vike_bridge_core::signer::BinanceHmacSigner;
use vike_bridge_core::transport::{RestTransport, UreqTransport};
use vike_model::AccountTier;
use vike_model::accounts::account_keys::AccountLabel;
use vike_model::events::{Event, RejectCause};
use vike_model::{OrderRequest, OrderType, TimeInForce, now_ms};
use vike_secrets::credentials::api_credentials;

fn po_req(coid: &str, qty: f64, price: Option<f64>, order_type: OrderType) -> OrderRequest {
    OrderRequest {
        client_order_id: coid.to_string(),
        venue: "binance".to_string(),
        symbol: "BTCUSDT".to_string(),
        side: 1,
        qty,
        order_type,
        price,
        time_in_force: TimeInForce::PostOnly,
        ts: now_ms(),
        ..Default::default()
    }
}

#[test]
#[ignore = "network + demo creds — run manually (see module doc)"]
fn binance_perp_post_only_ladder() {
    vike_log::test_init();
    let directory = vike_secrets::smoke_directory();
    let account = directory.credentials_for("binance", &AccountLabel::Default, AccountTier::Demo);
    let Some(creds) = api_credentials(&account, "binance") else {
        tracing::warn!(target: "vike_binance", "SKIP: BINANCE_DEMO creds absent");
        return;
    };
    let transport =
        UreqTransport::new("binance").with_rate_gate(vike_binance::ratelimit::perp_rest_gate());
    let info = transport
        .public(DEMO_FAPI_REST, FAPI_EXCHANGE_INFO, &[("symbol", "BTCUSDT".into())])
        .expect("fapi exchangeInfo");
    let inst = parse_binance_perp_instruments(&info)["BTCUSDT"].clone();
    let client = BinancePerpRest {
        link_id: None,
        signer: BinanceHmacSigner::new(&creds, now_ms),
        transport,
        base_url: DEMO_FAPI_REST.to_string(),
        algo_coids: Default::default(),
        symbol: "BTCUSDT".to_string(),
        properties: inst.properties,
        leverage: 5.0,
    };
    let local = now_ms();
    let server = client
        .transport
        .public(DEMO_FAPI_REST, "/fapi/v1/time", &[])
        .expect("fapi time")
        .get("serverTime")
        .and_then(|t| t.as_i64())
        .expect("serverTime");
    client.signer.set_offset_ms(server - local);
    let mark = client
        .transport
        .public(DEMO_FAPI_REST, "/fapi/v1/ticker/price", &[("symbol", "BTCUSDT".into())])
        .expect("ticker")
        .get("price")
        .and_then(|p| p.as_str())
        .and_then(|p| p.parse::<f64>().ok())
        .expect("price");
    assert!(mark > 0.0, "ticker mark must be live");
    let qty = (client.properties.min_notional.max(5.0) * 1.6) / (mark * 0.5);

    // 1. RESTS: far below the market a post-only buy is a plain maker order, and its record says GTX
    let coid = format!("vtrpo1{}", now_ms() % 100_000_000);
    let events =
        VenueRest::submit_order(&client, &po_req(&coid, qty, Some(mark * 0.5), OrderType::Limit));
    assert_matches!(events.last(), Some(Event::OrderAccepted(_)), "resting post-only: {events:?}");
    let mut order = serde_json::Value::Null;
    for attempt in 0..10 {
        match client.transport.signed(
            DEMO_FAPI_REST,
            FAPI_ORDER,
            "GET",
            &[("symbol", "BTCUSDT".into()), ("origClientOrderId", coid.clone())],
            &client.signer,
        ) {
            Ok(o) => {
                order = o;
                break;
            }
            Err(e) if e.code == -2013 && attempt < 9 => {
                std::thread::sleep(std::time::Duration::from_millis(500));
            }
            Err(e) => panic!("order query: {e:?}"),
        }
    }
    tracing::info!(target: "vike_binance", "resting row: {order}");
    assert_eq!(order.get("timeInForce").and_then(|v| v.as_str()), Some("GTX"), "{order}");
    assert_eq!(order.get("status").and_then(|v| v.as_str()), Some("NEW"), "{order}");
    VenueRest::cancel_order(&client, &coid).expect("cancel the resting post-only order");

    // 2. CROSSES: a buy 1% above the market lifts the ask — the venue must refuse it, typed
    let coid = format!("vtrpo2{}", now_ms() % 100_000_000);
    let events =
        VenueRest::submit_order(&client, &po_req(&coid, qty, Some(mark * 1.01), OrderType::Limit));
    tracing::info!(target: "vike_binance", "crossing submit events: {events:?}");
    assert_matches!(
        events.as_slice(),
        [Event::OrderSubmitted(_), Event::OrderRejected(r)]
            if r.cause == Some(RejectCause::PostOnlyWouldCross),
        "a crossing GTX order is refused with the typed cause: {events:?}"
    );

    // 3. a post-only MARKET order is refused locally
    let coid = format!("vtrpo3{}", now_ms() % 100_000_000);
    let events = VenueRest::submit_order(&client, &po_req(&coid, qty, None, OrderType::Market));
    assert_matches!(
        &events[1], Event::OrderRejected(r) if r.reason.contains("PostOnly needs a limit order"),
        "{events:?}"
    );

    // nothing rests
    let open = client
        .transport
        .signed(
            DEMO_FAPI_REST,
            FAPI_OPEN_ORDERS,
            "GET",
            &[("symbol", "BTCUSDT".into())],
            &client.signer,
        )
        .expect("openOrders");
    assert!(
        open.as_array().unwrap().iter().all(|o| {
            o.get("clientOrderId").and_then(|c| c.as_str()).is_none_or(|c| !c.starts_with("vtrpo"))
        }),
        "no post-only smoke order may rest"
    );
    tracing::info!(target: "vike_binance", "binance perp post-only ladder green: GTX rests far, crossing refused with the typed cause, market denied, book flat");
}

#[test]
#[ignore = "network + demo creds — run manually (see module doc)"]
fn binance_spot_post_only_ladder() {
    vike_log::test_init();
    let directory = vike_secrets::smoke_directory();
    let account = directory.credentials_for("binance", &AccountLabel::Default, AccountTier::Demo);
    let Some(creds) = api_credentials(&account, "binance") else {
        tracing::warn!(target: "vike_binance", "SKIP: BINANCE_DEMO creds absent");
        return;
    };
    let transport =
        UreqTransport::new("binance").with_rate_gate(vike_binance::ratelimit::spot_rest_gate());
    let signer = BinanceHmacSigner::new(&creds, now_ms);
    let info = transport
        .public(DEMO_REST, PATH_EXCHANGE_INFO, &[("symbol", "BTCUSDT".into())])
        .expect("exchangeInfo");
    let properties = parse_symbol_properties(&info)["BTCUSDT"];
    let client = BinanceSpotRest {
        link_id: None,
        signer,
        transport,
        base_url: DEMO_REST.to_string(),
        symbol: "BTCUSDT".to_string(),
        properties,
        base_asset: "BTC".to_string(),
    };
    let offset = client.server_time_offset(now_ms()).expect("server time");
    client.signer.set_offset_ms(offset);
    let snap = client.connect().expect("connect/reconcile");
    let mark = snap.position_avg_px[0].1;
    assert!(mark > 0.0, "ticker mark must be live");
    let far_px = mark * 0.9;
    let qty = (properties.min_notional.max(5.0) * 1.6) / far_px;

    // 1. RESTS: the wire is type=LIMIT_MAKER and carries NO timeInForce (the venue refuses one beside it)
    let coid = format!("vtrpo1{}", now_ms() % 100_000_000);
    let req = po_req(&coid, qty, Some(far_px), OrderType::Limit);
    let params = client.build_order_params(&req);
    assert!(params.iter().any(|(k, v)| *k == "type" && v == "LIMIT_MAKER"), "{params:?}");
    assert!(params.iter().all(|(k, _)| *k != "timeInForce"), "{params:?}");
    let events = client.submit_order(&req);
    assert_matches!(events.last(), Some(Event::OrderAccepted(_)), "resting post-only: {events:?}");
    let order = client
        .transport
        .signed(
            DEMO_REST,
            PATH_ORDER,
            "GET",
            &[("symbol", "BTCUSDT".into()), ("origClientOrderId", coid.clone())],
            &client.signer,
        )
        .expect("order query");
    tracing::info!(target: "vike_binance", "resting row: {order}");
    assert_eq!(order.get("type").and_then(|v| v.as_str()), Some("LIMIT_MAKER"), "{order}");
    assert_eq!(order.get("status").and_then(|v| v.as_str()), Some("NEW"), "{order}");
    VenueRest::cancel_order(&client, &coid).expect("cancel the resting post-only order");

    // 2. CROSSES: a buy 1% above the market lifts the ask — the venue must refuse it, typed
    let coid = format!("vtrpo2{}", now_ms() % 100_000_000);
    let events = client.submit_order(&po_req(&coid, qty, Some(mark * 1.01), OrderType::Limit));
    tracing::info!(target: "vike_binance", "crossing submit events: {events:?}");
    assert_matches!(
        events.as_slice(),
        [Event::OrderSubmitted(_), Event::OrderRejected(r)]
            if r.cause == Some(RejectCause::PostOnlyWouldCross),
        "a crossing LIMIT_MAKER order is refused with the typed cause: {events:?}"
    );

    // nothing rests
    let open = client
        .transport
        .signed(DEMO_REST, PATH_OPEN_ORDERS, "GET", &[("symbol", "BTCUSDT".into())], &client.signer)
        .expect("openOrders");
    assert!(
        open.as_array().unwrap().iter().all(|o| {
            o.get("clientOrderId").and_then(|c| c.as_str()).is_none_or(|c| !c.starts_with("vtrpo"))
        }),
        "no post-only smoke order may rest"
    );
    tracing::info!(target: "vike_binance", "binance spot post-only ladder green: LIMIT_MAKER rests far, crossing refused with the typed cause, book flat");
}
