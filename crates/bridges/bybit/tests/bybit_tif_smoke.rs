//! tif step-2 LIVE demo smoke (network + creds — run manually):
//!     cargo test -p vike-bybit --test bybit_tif_smoke -- --ignored --nocapture
//!
//! Proves the bybit TIF flip against the real V5 demo venue: a far-from-market LIMIT BUY
//! submitted with IOC then FOK carries the requested `timeInForce` on the wire and the venue
//! confirms the immediate-cancel semantics — the order-history record reports the requested TIF
//! and status `Cancelled` with zero fills (nothing rests, account stays flat). A Day request is
//! loud-denied client-side (terminal OrderRejected, wire untouched). Double-gated like the other
//! smokes: `#[ignore]` + self-skip when the demo account's creds are absent.

use std::assert_matches;
use std::time::{Duration, Instant};

use serde_json::json;
use vike_bridge_core::rest::VenueRest;
use vike_bridge_core::smoke::buy;
use vike_bridge_core::transport::json::RestTransportJson;
use vike_bybit::execution::rest::{
    PATH_INSTRUMENTS, PATH_ORDER_HISTORY, PATH_ORDER_REALTIME, VenueRestBybit,
    parse_bybit_perp_instruments, seed_endpoints,
};
use vike_bybit::signer::BybitV5Signer;
use vike_bybit::transport::client::rest_transport;
use vike_model::events::Event;
use vike_model::{OrderRequest, TimeInForce, now_ms};

use vike_model::AccountTier;
use vike_model::accounts::account_keys::AccountLabel;
use vike_secrets::credentials::api_credentials;

fn time_in_force_request(
    order_client_id: &str,
    qty: f64,
    price: f64,
    tif: TimeInForce,
) -> OrderRequest {
    buy(order_client_id, "bybit", "BTCUSDT", qty, vike_model::OrderType::Limit, Some(price), tif)
}

#[test]
#[ignore = "network + demo creds — run manually (see module doc)"]
fn bybit_time_in_force_ladder() {
    vike_log::test_init();
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
        category: vike_bybit::history::Category::Linear,
        signer: BybitV5Signer::new(&creds, now_ms),
        transport: rest_transport()
            .with_rate_gate(vike_bybit::transport::ratelimit::rest_rate_gate()),
        base_url: demo.clone(),
        mainnet: false,
        symbol: "BTCUSDT".to_string(),
        properties: instrument.properties,
        leverage: 2.0,
    };
    let mark = rest.last_price().expect("tickers");
    assert!(mark > 0.0, "live mark required");

    // far-below LIMIT BUY (cannot cross): IOC/FOK must be Cancelled immediately with 0 fills
    let limit_px = mark * 0.9;
    let qty = f64::max(
        instrument.properties.min_qty,
        (instrument.properties.min_notional * 1.4) / limit_px,
    );
    let qty = (qty / instrument.properties.step_size).ceil() * instrument.properties.step_size;

    for (tag, tif, wire) in [("I", TimeInForce::Ioc, "IOC"), ("F", TimeInForce::Fok, "FOK")] {
        let order_client_id = format!("vtrtif{tag}{}", now_ms() % 100_000_000);
        let events = VenueRest::submit_order(
            &rest,
            &time_in_force_request(&order_client_id, qty, limit_px, tif),
        );
        tracing::debug!(target: "vike_bybit", "submit events: {events:?}");
        assert_matches!(
            events.last(),
            Some(Event::OrderAccepted(_)),
            "{wire} submit must be ACK'd: {events:?}"
        );

        // the venue's own record (order history; brief retry for propagation): requested TIF on
        // the order, immediately Cancelled, zero filled
        let deadline = Instant::now() + Duration::from_secs(10);
        let row = loop {
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
                break row;
            }
            assert!(Instant::now() < deadline, "{order_client_id} never appeared in order history");
            std::thread::sleep(Duration::from_millis(500));
        };
        let got_time_in_force = row.get("timeInForce").and_then(|v| v.as_str());
        let status = row.get("orderStatus").and_then(|v| v.as_str());
        let exec_qty = row.get("cumExecQty").and_then(|v| v.as_str()).unwrap_or("0");
        tracing::info!(
            target: "vike_bybit",
            "{order_client_id}: timeInForce={got_time_in_force:?} orderStatus={status:?} cumExecQty={exec_qty}"
        );
        assert_eq!(
            got_time_in_force,
            Some(wire),
            "the requested TIF must be on the venue's record"
        );
        assert_eq!(
            status,
            Some("Cancelled"),
            "a non-crossing {wire} Limit must be cancelled immediately at the venue"
        );
        let exec = if exec_qty.is_empty() { 0.0 } else { exec_qty.parse::<f64>().unwrap_or(1.0) };
        assert_eq!(exec, 0.0, "no fill far from market");
    }

    // Day is Unsupported on bybit — loud client-side deny, wire untouched
    let order_client_id = format!("vtrtifD{}", now_ms() % 100_000_000);
    let events = VenueRest::submit_order(
        &rest,
        &time_in_force_request(&order_client_id, qty, limit_px, TimeInForce::Day),
    );
    assert_matches!(
        &events[1], Event::OrderRejected(r) if r.reason.contains("not supported"),
        "Day must be loud-denied: {events:?}"
    );

    // nothing rests: the realtime open-order view has none of our order client ids
    let open = rest
        .call_public(
            PATH_ORDER_REALTIME,
            &[("category", json!("linear")), ("symbol", json!("BTCUSDT"))],
        )
        .expect("order realtime");
    assert!(
        open.get("list").and_then(|l| l.as_array()).unwrap_or(&vec![]).iter().all(|o| {
            o.get("orderLinkId").and_then(|c| c.as_str()).is_none_or(|c| !c.starts_with("vtrtif"))
        }),
        "no tif-smoke order may rest"
    );
    tracing::info!(target: "vike_bybit", "bybit TIF ladder green: IOC/FOK honored + cancelled at venue, Day loud-denied, book flat");
}
