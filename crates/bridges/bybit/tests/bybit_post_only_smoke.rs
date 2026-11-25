//! post-only LIVE demo smoke (network + creds — run manually):
//!     cargo test -p vike-bybit --test integration -- --ignored --nocapture bybit_post_only_smoke::
//!
//! Proves the bybit post-only wire against the real V5 demo venue, with the venue's OWN records
//! as the witness:
//!
//! 1. a far-below post-only LIMIT BUY rests: the order record reports `timeInForce=PostOnly`, status
//!    `New`; it is cancelled and the book is flat;
//! 2. a post-only LIMIT BUY priced ABOVE the ask would take liquidity: it is created and cancelled by
//!    the venue at once (`rejectReason=EC_PostOnlyWillTakeLiquidity`), executes nothing and does not
//!    rest, and the bridge's own order-stream mapper (`event_mapper::map_order`) turns the venue's
//!    row into the typed terminal event (`OrderCanceled` with the post-only reason);
//! 3. a post-only MARKET order is loud-denied client-side (wire untouched).
//!
//! Double-gated like the other smokes: `#[ignore]` + self-skip when the demo account's creds are absent.

use std::assert_matches;
use std::time::{Duration, Instant};

use serde_json::json;
use vike_bridge_core::rest::VenueRest;
use vike_bridge_core::smoke::buy;
use vike_bridge_core::transport::json::{RestTransportJson, RestTransportJsonUreq};
use vike_bybit::execution::rest::{
    PATH_INSTRUMENTS, PATH_ORDER_HISTORY, PATH_ORDER_REALTIME, VenueRestBybit,
    parse_bybit_perp_instruments, seed_endpoints,
};
use vike_bybit::signer::BybitV5Signer;
use vike_bybit::transport::client::rest_transport;
use vike_model::events::Event;
use vike_model::{OrderRequest, OrderType, TimeInForce, now_ms};

use vike_model::AccountTier;
use vike_model::accounts::account_keys::AccountLabel;
use vike_secrets::credentials::api_credentials;

/// The linear perpetual's USER name (decision 0290): what an order names. The client signs its kept row's venue name,
/// `BTCUSDT` in `linear`, which every raw read here names too.
const SYMBOL: &str = "BTCUSDT.P";

fn po_request(
    order_client_id: &str,
    qty: f64,
    price: Option<f64>,
    order_type: OrderType,
) -> OrderRequest {
    buy(order_client_id, "bybit", SYMBOL, qty, order_type, price, TimeInForce::PostOnly)
}

fn history_row(
    rest: &VenueRestBybit<RestTransportJsonUreq>,
    order_client_id: &str,
) -> serde_json::Value {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let hist = rest
            .call_public(
                PATH_ORDER_HISTORY,
                &[
                    ("category", json!("linear")),
                    ("symbol", json!("BTCUSDT")),
                    ("orderLinkId", json!(order_client_id)),
                ],
            )
            .expect("order history");
        if let Some(row) =
            hist.get("list").and_then(|l| l.as_array()).and_then(|l| l.first()).cloned()
        {
            return row;
        }
        assert!(Instant::now() < deadline, "{order_client_id} never appeared in order history");
        std::thread::sleep(Duration::from_millis(500));
    }
}

#[test]
#[ignore = "network + demo creds — run manually (see module doc)"]
fn bybit_post_only_ladder() {
    vike_log::test_init();
    vike_bybit::history::kept_list_for_tests();
    let directory = vike_secrets::smoke_directory();
    let account = directory.credentials_for("bybit", &AccountLabel::Default, AccountTier::Demo);
    let Some(creds) = api_credentials(&account, "bybit") else {
        tracing::warn!(target: "vike_bybit", "SKIP: BYBIT_DEMO creds absent");
        return;
    };

    let demo = seed_endpoints(AccountTier::Demo).rest; // the demo host, from the release seed's rows
    let transport =
        rest_transport().with_rate_gate(vike_bybit::transport::ratelimit::rest_rate_gate());
    let info = transport
        .signed(
            &demo,
            PATH_INSTRUMENTS,
            "GET",
            &[("category", json!("linear")), ("symbol", json!("BTCUSDT"))],
            &BybitV5Signer::new(&creds, now_ms),
        )
        .expect("instruments-info");
    let instrument = parse_bybit_perp_instruments(&info)["BTCUSDT"].clone();

    let rest = VenueRestBybit {
        signer: BybitV5Signer::new(&creds, now_ms),
        transport: rest_transport()
            .with_rate_gate(vike_bybit::transport::ratelimit::rest_rate_gate()),
        base_url: demo.clone(),
        mainnet: false,
        symbol: "BTCUSDT".to_string(),
        properties: instrument.properties,
        leverage: 2.0,
        category: vike_bybit::history::Category::Linear,
    };
    let mark = rest.last_price().expect("tickers");
    assert!(mark > 0.0, "live mark required");
    let qty = f64::max(
        instrument.properties.min_qty,
        (instrument.properties.min_notional * 1.4) / (mark * 0.9),
    );
    let qty = (qty / instrument.properties.step_size).ceil() * instrument.properties.step_size;

    // 1. RESTS: far below the market a post-only buy is a plain maker order
    let order_client_id = format!("vtrpo1{}", now_ms() % 100_000_000);
    let events = VenueRest::submit_order(
        &rest,
        &po_request(&order_client_id, qty, Some(mark * 0.9), OrderType::Limit),
    );
    assert_matches!(events.last(), Some(Event::OrderAccepted(_)), "resting post-only: {events:?}");
    let row = history_row(&rest, &order_client_id);
    tracing::info!(target: "vike_bybit", "resting row: {row}");
    assert_eq!(row.get("timeInForce").and_then(|v| v.as_str()), Some("PostOnly"), "{row}");
    assert_eq!(row.get("orderStatus").and_then(|v| v.as_str()), Some("New"), "{row}");
    VenueRest::cancel_order(&rest, &order_client_id).expect("cancel the resting post-only order");

    // 2. CROSSES: a buy 1% above the market lifts the ask — the venue must not execute it
    let order_client_id = format!("vtrpo2{}", now_ms() % 100_000_000);
    let events = VenueRest::submit_order(
        &rest,
        &po_request(&order_client_id, qty, Some(mark * 1.01), OrderType::Limit),
    );
    tracing::info!(target: "vike_bybit", "crossing submit events: {events:?}");
    let row = history_row(&rest, &order_client_id);
    tracing::info!(target: "vike_bybit", "crossing row: {row}");
    let exec_qty = row.get("cumExecQty").and_then(|v| v.as_str()).unwrap_or("0");
    assert_eq!(
        exec_qty.parse::<f64>().unwrap_or(1.0),
        0.0,
        "a crossing post-only order must not execute: {row}"
    );
    assert_eq!(row.get("timeInForce").and_then(|v| v.as_str()), Some("PostOnly"), "{row}");
    assert_eq!(
        row.get("rejectReason").and_then(|v| v.as_str()),
        Some("EC_PostOnlyWillTakeLiquidity"),
        "the venue says why: {row}"
    );
    // the bridge's order-stream mapper turns that row into the typed terminal event
    let mapped = vike_bybit::execution::event_mapper::map_order(&row, "bybit", SYMBOL);
    tracing::info!(target: "vike_bybit", "mapped: {mapped:?}");
    assert_matches!(
        mapped.as_slice(),
        [Event::OrderCanceled(c)] if c.post_only_would_cross,
        "{mapped:?}"
    );

    // 3. a post-only MARKET order is refused locally
    let order_client_id = format!("vtrpo3{}", now_ms() % 100_000_000);
    let events =
        VenueRest::submit_order(&rest, &po_request(&order_client_id, qty, None, OrderType::Market));
    assert_matches!(
        &events[1], Event::OrderRejected(r) if r.reason.contains("PostOnly needs a limit order"),
        "{events:?}"
    );

    // nothing rests
    let open = rest
        .call_public(
            PATH_ORDER_REALTIME,
            &[("category", json!("linear")), ("symbol", json!("BTCUSDT"))],
        )
        .expect("order realtime");
    assert!(
        open.get("list").and_then(|l| l.as_array()).unwrap_or(&vec![]).iter().all(|o| {
            o.get("orderLinkId").and_then(|c| c.as_str()).is_none_or(|c| !c.starts_with("vtrpo"))
        }),
        "no post-only smoke order may rest"
    );
    tracing::info!(target: "vike_bybit", "bybit post-only ladder green: rests far, crossing refused by the venue and typed, market denied, book flat");
}
