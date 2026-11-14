//! tif step-2 LIVE demo smoke (network + creds — run manually):
//!     cargo test -p vike-okx --test okx_smokes -- --ignored --nocapture okx_time_in_force_smoke::
//!
//! Proves the okx TIF flip against the real venue (simulated-trading): a far-from-market LIMIT
//! BUY submitted with IOC then FOK rides the wire as the `ioc`/`fok` ordType and the venue
//! confirms the immediate-cancel semantics — the orders-history record reports the requested
//! ordType and `state == "canceled"` with zero fills (nothing rests, account stays flat). A Day
//! request is loud-denied client-side (terminal OrderRejected, wire untouched). Double-gated
//! exactly like every other `*_smoke.rs` in this crate (network + the default DEMO
//! account's creds + passphrase via `vike_secrets::smoke_directory`; self-skips with a
//! `tracing::warn!` when absent).

use std::assert_matches;
use std::time::{Duration, Instant};

use serde_json::json;

use crate::demo_credentials::load_demo_credentials;
use vike_bridge_core::rest::VenueRest;
use vike_bridge_core::transport::json::RestTransportJson;
use vike_model::TimeInForce;
use vike_model::events::Event;
use vike_model::now_ms;
use vike_okx::execution::rest::{
    PATH_INSTRUMENTS, PATH_ORDERS_PENDING, VenueRestOkx, parse_okx_perp_instruments,
};
use vike_okx::signer::OkxV5Signer;
use vike_okx::test_support::demo_net;
use vike_okx::transport::client::rest_transport;
use vike_okx::transport::client::unwrap_okx;

const SYMBOL: &str = "BTC-USDT-SWAP";

// Per venue: built from JSON on `SYMBOL`, unlike the `BTCUSDT` struct literals of
// `crates/bridges/binance/tests/binance_tif_smoke.rs`'s and bybit's `time_in_force_request`.
fn time_in_force_request(
    order_client_id: &str,
    qty: f64,
    price: f64,
    tif: TimeInForce,
) -> vike_model::OrderRequest {
    let mut req: vike_model::OrderRequest = serde_json::from_value(json!({
        "order_client_id": order_client_id, "venue": "okx", "symbol": SYMBOL,
        "side": 1, "qty": qty, "order_type": "limit", "price": price, "ts": now_ms()
    }))
    .unwrap();
    req.time_in_force = tif;
    req
}

#[test]
#[ignore = "network + demo creds — run manually (see module doc)"]
fn okx_time_in_force_ladder() {
    vike_log::test_init();
    let Some(creds) = load_demo_credentials() else { return };
    assert!(creds.passphrase.is_some(), "OKX needs the API passphrase");

    let transport =
        rest_transport(true).with_rate_gate(vike_okx::transport::ratelimit::rest_rate_gate()); // demo: x-simulated-trading: 1
    let info = transport
        .public(
            &demo_net().rest,
            PATH_INSTRUMENTS,
            &[("instType", "SWAP".into()), ("instId", SYMBOL.into())],
        )
        .expect("public instruments");
    let instrument = parse_okx_perp_instruments(
        &unwrap_okx(info).map(|d| json!({ "data": d })).expect("instruments data"),
    )[SYMBOL]
        .clone();

    let rest = VenueRestOkx {
        signer: OkxV5Signer::new(&creds, now_ms),
        transport: rest_transport(true)
            .with_rate_gate(vike_okx::transport::ratelimit::rest_rate_gate()),
        base_url: demo_net().rest,
        symbol: SYMBOL.to_string(),
        properties: instrument.properties,
        ct_val: instrument.ct_val,
        leverage: 2.0,
        broker_code: None,
    };
    let mark = rest.last_price().expect("ticker");
    assert!(mark > 0.0, "live mark required");

    // one contract's worth of BASE, ~10% below market → cannot cross: an IOC/FOK LIMIT BUY must
    // be canceled immediately at the venue with zero fills
    let leg_base = instrument.properties.min_qty * instrument.ct_val;
    let limit_px = mark * 0.9;

    for (tag, tif, wire) in [("I", TimeInForce::Ioc, "ioc"), ("F", TimeInForce::Fok, "fok")] {
        let order_client_id = format!("vtrtif{tag}{}", now_ms() % 100_000_000);
        let events =
            rest.submit_order(&time_in_force_request(&order_client_id, leg_base, limit_px, tif));
        tracing::debug!(target: "vike_okx", "submit events: {events:?}");
        assert_matches!(
            events.last(),
            Some(Event::OrderAccepted(_)),
            "{wire} submit must be ACK'd: {events:?}"
        );

        // the venue's own record (orders-history; brief retry for propagation): the requested
        // ordType on the order, immediately canceled, zero filled
        let deadline = Instant::now() + Duration::from_secs(10);
        let row = loop {
            let hist = rest.get_orders_history(100).expect("orders-history");
            if let Some(row) = hist.as_array().and_then(|rows| {
                rows.iter()
                    .find(|o| {
                        o.get("clOrdId").and_then(|c| c.as_str()) == Some(order_client_id.as_str())
                    })
                    .cloned()
            }) {
                break row;
            }
            assert!(
                Instant::now() < deadline,
                "{order_client_id} never appeared in orders-history"
            );
            std::thread::sleep(Duration::from_millis(500));
        };
        let got_ord_type = row.get("ordType").and_then(|v| v.as_str());
        let state = row.get("state").and_then(|v| v.as_str());
        let fill_sz = row.get("accFillSz").and_then(|v| v.as_str()).unwrap_or("0");
        tracing::info!(
            target: "vike_okx",
            "{order_client_id}: ordType={got_ord_type:?} state={state:?} accFillSz={fill_sz}"
        );
        assert_eq!(got_ord_type, Some(wire), "the requested TIF must ride ordType: {row}");
        assert_eq!(
            state,
            Some("canceled"),
            "a non-crossing {wire} limit must be canceled immediately at the venue: {row}"
        );
        let filled = if fill_sz.is_empty() { 0.0 } else { fill_sz.parse::<f64>().unwrap_or(1.0) };
        assert_eq!(filled, 0.0, "no fill far from market");
    }

    // Day (and Gtd) are Unsupported on okx — loud client-side deny, wire untouched
    let order_client_id = format!("vtrtifD{}", now_ms() % 100_000_000);
    let events = rest.submit_order(&time_in_force_request(
        &order_client_id,
        leg_base,
        limit_px,
        TimeInForce::Day,
    ));
    assert_matches!(
        &events[1], Event::OrderRejected(r) if r.reason.contains("not supported"),
        "Day must be loud-denied: {events:?}"
    );

    // nothing rests: orders-pending has none of our order client ids
    let pending = rest
        .call_get(PATH_ORDERS_PENDING, &[("instType", json!("SWAP")), ("instId", json!(SYMBOL))])
        .expect("orders-pending");
    assert!(
        pending.as_array().unwrap_or(&vec![]).iter().all(|o| {
            o.get("clOrdId").and_then(|c| c.as_str()).is_none_or(|c| !c.starts_with("vtrtif"))
        }),
        "no tif-smoke order may rest: {pending}"
    );
    tracing::info!(
        target: "vike_okx",
        "okx TIF ladder green: ioc/fok honored on ordType + canceled at venue, Day loud-denied, book flat"
    );
}
