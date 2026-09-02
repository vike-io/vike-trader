//! tif step-2 LIVE demo smoke (network + creds — run manually):
//!     cargo test -p vike-bybit --test bybit_tif_smoke -- --ignored --nocapture
//!
//! Proves the bybit TIF flip against the real V5 demo venue: a far-from-market LIMIT BUY
//! submitted with IOC then FOK carries the requested `timeInForce` on the wire and the venue
//! confirms the immediate-cancel semantics — the order-history record reports the requested TIF
//! and status `Cancelled` with zero fills (nothing rests, account stays flat). A Day request is
//! loud-denied client-side (terminal OrderRejected, wire untouched). Double-gated like the other
//! smokes: `#[ignore]` + self-skip when BYBIT_DEMO creds are absent.

use std::time::{Duration, Instant};

use serde_json::json;
use vike_bridge_core::credentials::{
    load_credentials_from, load_workspace_dotenv_from, Environment,
};
use vike_bridge_core::rest::VenueRest;
use vike_bridge_core::signer::BybitV5Signer;
use vike_bybit::perp::{
    parse_bybit_perp_instruments, BybitPerpRest, DEMO_REST, PATH_INSTRUMENTS, PATH_ORDER_HISTORY,
    PATH_ORDER_REALTIME,
};
use vike_bybit::transport::{BybitTransport, UreqBybitTransport};
use vike_model::events::Event;
use vike_model::{OrderRequest, TimeInForce};

use vike_model::clock::now_ms;

fn tif_req(coid: &str, qty: f64, price: f64, tif: TimeInForce) -> OrderRequest {
    OrderRequest {
        client_order_id: coid.to_string(),
        venue: "bybit".to_string(),
        symbol: "BTCUSDT".to_string(),
        side: 1,
        qty,
        order_type: "limit".to_string(),
        price: Some(price),
        time_in_force: tif,
        ts: now_ms(),
        ..Default::default()
    }
}

#[test]
#[ignore = "network + demo creds — run manually (see module doc)"]
fn bybit_tif_ladder() {
    vike_log::test_init();
    let vars = load_workspace_dotenv_from(std::env::var("VIKE_SETTINGS_DIR").ok().as_deref());
    let Some(creds) = load_credentials_from("bybit", Environment::Demo, &vars) else {
        tracing::warn!(target: "vike_bybit", "SKIP: BYBIT_DEMO creds absent");
        return;
    };

    let transport =
        UreqBybitTransport::new().with_rate_gate(vike_bybit::ratelimit::rest_rate_gate());
    let info = transport
        .signed(
            DEMO_REST,
            PATH_INSTRUMENTS,
            "GET",
            &[("category", json!("linear")), ("symbol", json!("BTCUSDT"))],
            &BybitV5Signer::new(&creds, now_ms),
        )
        .expect("instruments-info");
    let inst = parse_bybit_perp_instruments(&info)["BTCUSDT"].clone();

    let rest = BybitPerpRest {
        signer: BybitV5Signer::new(&creds, now_ms),
        transport: UreqBybitTransport::new()
            .with_rate_gate(vike_bybit::ratelimit::rest_rate_gate()),
        base_url: DEMO_REST.to_string(),
        symbol: "BTCUSDT".to_string(),
        properties: inst.properties,
        leverage: 2.0,
    };
    let mark = rest.last_price().expect("tickers");
    assert!(mark > 0.0, "live mark required");

    // far-below LIMIT BUY (cannot cross): IOC/FOK must be Cancelled immediately with 0 fills
    let limit_px = mark * 0.9;
    let qty = f64::max(inst.properties.min_qty, (inst.properties.min_notional * 1.4) / limit_px);
    let qty = (qty / inst.properties.step_size).ceil() * inst.properties.step_size;

    for (tag, tif, wire) in [("I", TimeInForce::Ioc, "IOC"), ("F", TimeInForce::Fok, "FOK")] {
        let coid = format!("vtrtif{tag}{}", now_ms() % 100_000_000);
        let events = VenueRest::submit_order(&rest, &tif_req(&coid, qty, limit_px, tif));
        tracing::debug!(target: "vike_bybit", "submit events: {events:?}");
        assert!(
            matches!(events.last(), Some(Event::OrderAccepted(_))),
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
                        ("orderLinkId", json!(coid)),
                    ],
                )
                .expect("order history");
            if let Some(row) =
                hist.get("list").and_then(|l| l.as_array()).and_then(|l| l.first()).cloned()
            {
                break row;
            }
            assert!(Instant::now() < deadline, "{coid} never appeared in order history");
            std::thread::sleep(Duration::from_millis(500));
        };
        let got_tif = row.get("timeInForce").and_then(|v| v.as_str());
        let status = row.get("orderStatus").and_then(|v| v.as_str());
        let exec_qty = row.get("cumExecQty").and_then(|v| v.as_str()).unwrap_or("0");
        tracing::info!(
            target: "vike_bybit",
            "{coid}: timeInForce={got_tif:?} orderStatus={status:?} cumExecQty={exec_qty}"
        );
        assert_eq!(got_tif, Some(wire), "the requested TIF must be on the venue's record");
        assert_eq!(
            status,
            Some("Cancelled"),
            "a non-crossing {wire} Limit must be cancelled immediately at the venue"
        );
        let exec = if exec_qty.is_empty() { 0.0 } else { exec_qty.parse::<f64>().unwrap_or(1.0) };
        assert_eq!(exec, 0.0, "no fill far from market");
    }

    // Day is Unsupported on bybit — loud client-side deny, wire untouched
    let coid = format!("vtrtifD{}", now_ms() % 100_000_000);
    let events = VenueRest::submit_order(&rest, &tif_req(&coid, qty, limit_px, TimeInForce::Day));
    assert!(
        matches!(&events[1], Event::OrderRejected(r) if r.reason.contains("not supported")),
        "Day must be loud-denied: {events:?}"
    );

    // nothing rests: the realtime open-order view has none of our coids
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
