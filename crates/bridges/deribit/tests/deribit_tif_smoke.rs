//! tif step-2 LIVE Deribit smoke (network + testnet creds — run manually):
//!     cargo test -p vike-deribit --test deribit_tif_smoke -- --ignored --nocapture
//!
//! Proves the deribit TIF flip against test.deribit.com on the most liquid BTC option: a
//! far-below LIMIT BUY submitted with IOC then FOK carries the mapped `time_in_force`
//! (`immediate_or_cancel`/`fill_or_kill`) and the venue confirms immediate-cancel semantics
//! (order history: requested TIF + `cancelled` + zero filled); a Day request maps to
//! `good_til_day`, RESTS, and is cancelled; a Gtd request is loud-denied client-side (terminal
//! OrderRejected, wire untouched). Ends flat. Double-gated like the other smokes: `#[ignore]` +
//! self-skip when DERIBIT_DEMO creds are absent.

use std::time::{Duration, Instant};

use vike_bridge_core::credentials::{
    Environment, load_credentials_from, load_workspace_dotenv_from,
};
use vike_bridge_core::rest::VenueRest;
use vike_bridge_core::transport::{RestTransport, UreqTransport};
use vike_deribit::client::{DeribitRest, parse_deribit_option_instruments};
use vike_deribit::transport::{DeribitOrderTransport, TESTNET_REST, TESTNET_WS};
use vike_model::events::Event;
use vike_model::{OrderRequest, TimeInForce};

use vike_model::clock::now_ms;

fn tif_req(coid: &str, symbol: &str, qty: f64, price: f64, tif: TimeInForce) -> OrderRequest {
    OrderRequest {
        client_order_id: coid.to_string(),
        venue: "deribit".to_string(),
        symbol: symbol.to_string(),
        side: 1,
        qty,
        order_type: "limit".to_string(),
        price: Some(price),
        time_in_force: tif,
        ts: now_ms(),
        ..Default::default()
    }
}

/// Poll order history until our label shows up (WS submit is sync but history can lag briefly).
fn history_row(rest: &DeribitRest, coid: &str) -> serde_json::Value {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let hist = rest.get_order_history(50).expect("order history");
        if let Some(row) = hist
            .as_array()
            .unwrap_or(&vec![])
            .iter()
            .find(|o| o.get("label").and_then(|l| l.as_str()) == Some(coid))
        {
            return row.clone();
        }
        assert!(Instant::now() < deadline, "{coid} never appeared in order history");
        std::thread::sleep(Duration::from_millis(500));
    }
}

#[test]
#[ignore = "network + testnet creds — run manually (see module doc)"]
fn deribit_tif_ladder() {
    vike_log::test_init();
    let vars = load_workspace_dotenv_from(std::env::var("VIKE_SETTINGS_DIR").ok().as_deref());
    let Some(creds) = load_credentials_from("deribit", Environment::Demo, &vars) else {
        tracing::warn!(target: "vike_deribit", "SKIP: DERIBIT_DEMO creds absent");
        return;
    };

    // most liquid BTC option with a live ask (same discovery as deribit_smoke)
    let public = UreqTransport::new("deribit");
    let book = public
        .public(
            TESTNET_REST,
            "/api/v2/public/get_book_summary_by_currency",
            &[("currency", "BTC".into()), ("kind", "option".into())],
        )
        .expect("book summary");
    let mut best: Option<(String, f64, f64)> = None;
    for row in book["result"].as_array().unwrap_or(&vec![]) {
        let Some(ask) = row.get("ask_price").and_then(|a| a.as_f64()) else { continue };
        let oi = row.get("open_interest").and_then(|o| o.as_f64()).unwrap_or(0.0);
        let name = row.get("instrument_name").and_then(|n| n.as_str()).unwrap_or("");
        if ask > 0.0 && !name.is_empty() && best.as_ref().is_none_or(|(_, b_oi, _)| oi > *b_oi) {
            best = Some((name.to_string(), oi, ask));
        }
    }
    let (symbol, oi, ask) = best.expect("a live-ask BTC option on testnet");
    tracing::info!(target: "vike_deribit", "instrument: {symbol} (oi={oi}, ask={ask})");

    let info = public
        .public(
            TESTNET_REST,
            "/api/v2/public/get_instruments",
            &[("currency", "BTC".into()), ("kind", "option".into())],
        )
        .expect("instruments");
    let inst = parse_deribit_option_instruments(&info)[&symbol].clone();

    let mut transport =
        DeribitOrderTransport::new(TESTNET_WS, &creds.api_key, &creds.api_secret, None);
    transport.connect().expect("order-WS auth");
    let rest = DeribitRest::new(transport, &symbol, inst.properties, "BTC");

    let qty = inst.properties.min_qty;
    // far below the ask, under the tiered-tick boundary (same rule as deribit_smoke)
    let low_px = (ask * 0.25).min(0.004).max(inst.properties.tick_size);

    // IOC / FOK: accepted then immediately cancelled at the venue, mapped TIF on the record
    for (tag, tif, wire) in
        [("I", TimeInForce::Ioc, "immediate_or_cancel"), ("F", TimeInForce::Fok, "fill_or_kill")]
    {
        let coid = format!("vtrtif{tag}{}", now_ms() % 100_000_000);
        let events = rest.submit_order(&tif_req(&coid, &symbol, qty, low_px, tif));
        tracing::debug!(target: "vike_deribit", "submit events: {events:?}");
        assert!(
            matches!(events.last(), Some(Event::OrderAccepted(_))),
            "{wire} submit must be accepted: {events:?}"
        );
        let row = history_row(&rest, &coid);
        let got_tif = row.get("time_in_force").and_then(|v| v.as_str());
        let state = row.get("order_state").and_then(|v| v.as_str());
        let filled = row.get("filled_amount").and_then(|v| v.as_f64()).unwrap_or(1.0);
        tracing::info!(
            target: "vike_deribit",
            "{coid}: time_in_force={got_tif:?} order_state={state:?} filled={filled}"
        );
        assert_eq!(got_tif, Some(wire), "the mapped TIF must be on the venue's record");
        assert_eq!(state, Some("cancelled"), "a non-crossing {wire} limit cancels immediately");
        assert_eq!(filled, 0.0, "no fill far from market");
    }

    // Day -> good_til_day: RESTS (open order carries the mapped TIF), then cancel
    let day_coid = format!("vtrtifD{}", now_ms() % 100_000_000);
    let events = rest.submit_order(&tif_req(&day_coid, &symbol, qty, low_px, TimeInForce::Day));
    assert!(
        matches!(events.last(), Some(Event::OrderAccepted(_))),
        "good_til_day submit must be accepted: {events:?}"
    );
    let snap = rest.connect().expect("reconcile while resting");
    assert!(
        snap.open_orders.iter().any(|o| o.request.client_order_id == day_coid),
        "the good_til_day order must REST in open orders"
    );
    rest.cancel_order(&day_coid).expect("cancel the resting Day order");
    let row = history_row(&rest, &day_coid);
    let got_tif = row.get("time_in_force").and_then(|v| v.as_str());
    tracing::info!(
        target: "vike_deribit",
        "{day_coid}: time_in_force={got_tif:?} order_state={:?}",
        row.get("order_state").and_then(|v| v.as_str())
    );
    assert_eq!(got_tif, Some("good_til_day"), "Day must ride as good_til_day");

    // Gtd is Unsupported on deribit — loud client-side deny, wire untouched
    let gtd_coid = format!("vtrtifG{}", now_ms() % 100_000_000);
    let mut req = tif_req(&gtd_coid, &symbol, qty, low_px, TimeInForce::Gtd);
    req.gtd_expiry = Some(now_ms() + 86_400_000);
    let events = rest.submit_order(&req);
    assert!(
        matches!(&events[1], Event::OrderRejected(r) if r.reason.contains("not supported")),
        "Gtd must be loud-denied: {events:?}"
    );

    // flat: no smoke order rests
    let snap = rest.connect().expect("final reconcile");
    assert!(
        snap.open_orders.iter().all(|o| !o.request.client_order_id.starts_with("vtrtif")),
        "no tif-smoke order may rest"
    );
    rest.detach();
    tracing::info!(target: "vike_deribit", "deribit TIF ladder green: IOC/FOK mapped + cancelled at venue, Day rests as good_til_day + cancelled, Gtd loud-denied, book flat");
}
