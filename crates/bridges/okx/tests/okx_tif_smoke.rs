//! tif step-2 LIVE demo smoke (network + creds — run manually):
//!     cargo test -p vike-okx --test okx_tif_smoke -- --ignored --nocapture
//!
//! Proves the okx TIF flip against the real venue (simulated-trading): a far-from-market LIMIT
//! BUY submitted with IOC then FOK rides the wire as the `ioc`/`fok` ordType and the venue
//! confirms the immediate-cancel semantics — the orders-history record reports the requested
//! ordType and `state == "canceled"` with zero fills (nothing rests, account stays flat). A Day
//! request is loud-denied client-side (terminal OrderRejected, wire untouched). Double-gated
//! exactly like every other `*_smoke.rs` in this crate (network + `OKX_DEMO_*` creds +
//! passphrase in the workspace `.env` via `load_workspace_dotenv_from`; self-skips with a
//! `tracing::warn!` when absent).

use std::time::{Duration, Instant};

use serde_json::json;

use vike_bridge_core::credentials::{
    load_credentials_from, load_workspace_dotenv_from, Credentials, Environment,
};
use vike_bridge_core::rest::VenueRest;
use vike_bridge_core::signer::OkxV5Signer;
use vike_model::clock::now_ms;
use vike_model::events::Event;
use vike_model::TimeInForce;
use vike_okx::perp::{
    parse_okx_perp_instruments, OkxPerpRest, PATH_INSTRUMENTS, PATH_ORDERS_PENDING, REST,
};
use vike_okx::transport::{unwrap_okx, OkxTransport, UreqOkxTransport};

const SYMBOL: &str = "BTC-USDT-SWAP";

/// The crate's standard smoke double-gate: workspace `.env` → `OKX_DEMO_*`; `None` = self-skip.
fn load_demo_creds() -> Option<Credentials> {
    let vars = load_workspace_dotenv_from(std::env::var("VIKE_SETTINGS_DIR").ok().as_deref());
    let creds = load_credentials_from("okx", Environment::Demo, &vars);
    if creds.is_none() {
        tracing::warn!(target: "vike_okx", "SKIP: OKX_DEMO creds absent");
    }
    creds
}

fn tif_req(coid: &str, qty: f64, price: f64, tif: TimeInForce) -> vike_model::OrderRequest {
    let mut req: vike_model::OrderRequest = serde_json::from_value(json!({
        "client_order_id": coid, "venue": "okx", "symbol": SYMBOL,
        "side": 1, "qty": qty, "order_type": "limit", "price": price, "ts": now_ms()
    }))
    .unwrap();
    req.time_in_force = tif;
    req
}

#[test]
#[ignore = "network + demo creds — run manually (see module doc)"]
fn okx_tif_ladder() {
    vike_log::test_init();
    let Some(creds) = load_demo_creds() else { return };
    assert!(creds.passphrase.is_some(), "OKX needs the API passphrase");

    let transport =
        UreqOkxTransport::new(true).with_rate_gate(vike_okx::ratelimit::rest_rate_gate()); // demo: x-simulated-trading: 1
    let info = transport
        .public(REST, PATH_INSTRUMENTS, &[("instType", "SWAP".into()), ("instId", SYMBOL.into())])
        .expect("public instruments");
    let inst = parse_okx_perp_instruments(
        &unwrap_okx(info).map(|d| json!({ "data": d })).expect("instruments data"),
    )[SYMBOL]
        .clone();

    let rest = OkxPerpRest {
        signer: OkxV5Signer::new(&creds, now_ms),
        transport: UreqOkxTransport::new(true)
            .with_rate_gate(vike_okx::ratelimit::rest_rate_gate()),
        base_url: REST.to_string(),
        symbol: SYMBOL.to_string(),
        properties: inst.properties,
        ct_val: inst.ct_val,
        leverage: 2.0,
        broker_code: None,
    };
    let mark = rest.last_price().expect("ticker");
    assert!(mark > 0.0, "live mark required");

    // one contract's worth of BASE, ~10% below market → cannot cross: an IOC/FOK LIMIT BUY must
    // be canceled immediately at the venue with zero fills
    let leg_base = inst.properties.min_qty * inst.ct_val;
    let limit_px = mark * 0.9;

    for (tag, tif, wire) in [("I", TimeInForce::Ioc, "ioc"), ("F", TimeInForce::Fok, "fok")] {
        let coid = format!("vtrtif{tag}{}", now_ms() % 100_000_000);
        let events = rest.submit_order(&tif_req(&coid, leg_base, limit_px, tif));
        tracing::debug!(target: "vike_okx", "submit events: {events:?}");
        assert!(
            matches!(events.last(), Some(Event::OrderAccepted(_))),
            "{wire} submit must be ACK'd: {events:?}"
        );

        // the venue's own record (orders-history; brief retry for propagation): the requested
        // ordType on the order, immediately canceled, zero filled
        let deadline = Instant::now() + Duration::from_secs(10);
        let row = loop {
            let hist = rest.get_orders_history(100).expect("orders-history");
            if let Some(row) = hist.as_array().and_then(|rows| {
                rows.iter()
                    .find(|o| o.get("clOrdId").and_then(|c| c.as_str()) == Some(coid.as_str()))
                    .cloned()
            }) {
                break row;
            }
            assert!(Instant::now() < deadline, "{coid} never appeared in orders-history");
            std::thread::sleep(Duration::from_millis(500));
        };
        let got_ord_type = row.get("ordType").and_then(|v| v.as_str());
        let state = row.get("state").and_then(|v| v.as_str());
        let fill_sz = row.get("accFillSz").and_then(|v| v.as_str()).unwrap_or("0");
        tracing::info!(
            target: "vike_okx",
            "{coid}: ordType={got_ord_type:?} state={state:?} accFillSz={fill_sz}"
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
    let coid = format!("vtrtifD{}", now_ms() % 100_000_000);
    let events = rest.submit_order(&tif_req(&coid, leg_base, limit_px, TimeInForce::Day));
    assert!(
        matches!(&events[1], Event::OrderRejected(r) if r.reason.contains("not supported")),
        "Day must be loud-denied: {events:?}"
    );

    // nothing rests: orders-pending has none of our coids
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
