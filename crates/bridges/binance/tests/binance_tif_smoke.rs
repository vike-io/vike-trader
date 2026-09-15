//! tif step-2 LIVE demo smokes (network + creds — run manually):
//!     cargo test -p vike-binance --test binance_tif_smoke -- --ignored --nocapture
//!
//! `binance_spot_tif_ladder` proves the binance TIF flip against the real spot demo venue: a
//! far-from-market LIMIT BUY submitted with IOC then FOK carries the requested `timeInForce` on
//! the wire and the venue confirms the immediate-expiry semantics — the queried order reports
//! the requested TIF and status EXPIRED with zero fills (nothing rests, account stays flat). A
//! GTD request is loud-denied client-side (terminal OrderRejected, wire untouched).
//!
//! `binance_perp_gtd_ladder` proves the PERP lane's native GTD flip against the real fapi demo
//! venue: a far-from-market LIMIT BUY with `time_in_force=Gtd` + `gtd_expiry` ~1h out rests, the
//! venue's own order record reports `timeInForce=GTD` with the exact `goodTillDate`, and the
//! order cancels clean (book flat). It also probes the venue's rejection shape for an
//! invalid-horizon `goodTillDate` (below the now+600s floor, POSTed raw past the local gate) and
//! pins that the normal submit path turns the same violation into a proper terminal
//! `OrderRejected` — never silence. Double-gated like the other smokes: `#[ignore]` + self-skip
//! when BINANCE_DEMO creds are absent.

use vike_binance::perp::{
    BinancePerpRest, DEMO_FAPI_REST, PATH_EXCHANGE_INFO as FAPI_EXCHANGE_INFO,
    PATH_OPEN_ORDERS as FAPI_OPEN_ORDERS, PATH_ORDER as FAPI_ORDER, parse_binance_perp_instruments,
};
use vike_binance::spot::{
    BinanceSpotRest, DEMO_REST, PATH_EXCHANGE_INFO, PATH_OPEN_ORDERS, PATH_ORDER,
    parse_symbol_properties,
};
use vike_bridge_core::credentials::{
    Environment, load_credentials_from, load_workspace_dotenv_from,
};
use vike_bridge_core::rest::VenueRest;
use vike_bridge_core::signer::BinanceHmacSigner;
use vike_bridge_core::transport::{RestTransport, UreqTransport};
use vike_model::events::Event;
use vike_model::{OrderRequest, TimeInForce};

use vike_model::clock::now_ms;

fn tif_req(coid: &str, qty: f64, price: f64, tif: TimeInForce) -> OrderRequest {
    OrderRequest {
        client_order_id: coid.to_string(),
        venue: "binance".to_string(),
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
fn binance_spot_tif_ladder() {
    vike_log::test_init();
    let vars = load_workspace_dotenv_from(std::env::var("VIKE_SETTINGS_DIR").ok().as_deref());
    let Some(creds) = load_credentials_from("binance", Environment::Demo, &vars) else {
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

    // far-below LIMIT BUY (cannot cross): IOC/FOK must come back EXPIRED with 0 fills.
    // 0.9× mark: comfortably below the bid (no fill) yet inside the spot PERCENT_PRICE_BY_SIDE
    // band (0.5× trips filter -1013 on the demo venue).
    let limit_px = mark * 0.9;
    let qty = (properties.min_notional.max(5.0) * 1.6) / limit_px;

    for (tag, tif, wire) in [("I", TimeInForce::Ioc, "IOC"), ("F", TimeInForce::Fok, "FOK")] {
        let coid = format!("vtrtif{tag}{}", now_ms() % 100_000_000);
        let events = client.submit_order(&tif_req(&coid, qty, limit_px, tif));
        tracing::debug!(target: "vike_binance", "submit events: {events:?}");
        assert!(
            matches!(events.last(), Some(Event::OrderAccepted(_))),
            "{wire} submit must be ACK'd: {events:?}"
        );

        // the venue's own record: requested TIF on the order, immediately EXPIRED, zero filled
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
        let got_tif = order.get("timeInForce").and_then(|v| v.as_str());
        let status = order.get("status").and_then(|v| v.as_str());
        let executed = order.get("executedQty").and_then(|v| v.as_str()).unwrap_or("0");
        tracing::info!(
            target: "vike_binance",
            "{coid}: timeInForce={got_tif:?} status={status:?} executedQty={executed}"
        );
        assert_eq!(got_tif, Some(wire), "the requested TIF must be on the venue's record");
        assert_eq!(
            status,
            Some("EXPIRED"),
            "a non-crossing {wire} LIMIT must expire immediately at the venue"
        );
        assert_eq!(executed.parse::<f64>().unwrap_or(1.0), 0.0, "no fill far from market");
    }

    // GTD is Unsupported on binance — loud client-side deny, wire untouched
    let coid = format!("vtrtifG{}", now_ms() % 100_000_000);
    let mut req = tif_req(&coid, qty, limit_px, TimeInForce::Gtd);
    req.gtd_expiry = Some(now_ms() + 86_400_000);
    let events = client.submit_order(&req);
    assert!(
        matches!(&events[1], Event::OrderRejected(r) if r.reason.contains("not supported")),
        "GTD must be loud-denied: {events:?}"
    );

    // account is flat of our orders: nothing rests
    let open = client
        .transport
        .signed(DEMO_REST, PATH_OPEN_ORDERS, "GET", &[("symbol", "BTCUSDT".into())], &client.signer)
        .expect("openOrders");
    assert!(
        open.as_array().unwrap().iter().all(|o| {
            o.get("clientOrderId").and_then(|c| c.as_str()).is_none_or(|c| !c.starts_with("vtrtif"))
        }),
        "no tif-smoke order may rest"
    );
    tracing::info!(target: "vike_binance", "binance TIF ladder green: IOC/FOK honored + expired at venue, GTD loud-denied, book flat");
}

/// PERP lane native GTD (see the module doc). Places + cancels ONE real (non-filling,
/// far-from-market) GTD LIMIT on the fapi demo venue, then probes the invalid-horizon
/// rejection shape.
#[test]
#[ignore = "network + demo creds — places + cancels a real (non-filling) GTD demo order — run manually (see module doc)"]
fn binance_perp_gtd_ladder() {
    vike_log::test_init();
    let vars = load_workspace_dotenv_from(std::env::var("VIKE_SETTINGS_DIR").ok().as_deref());
    let Some(creds) = load_credentials_from("binance", Environment::Demo, &vars) else {
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
        symbol: "BTCUSDT".to_string(),
        properties: inst.properties,
        leverage: 5.0,
    };
    // fapi server clock skew: the perp signer syncs against ITS venue's /fapi/v1/time (spot's
    // `server_time_offset` helper reads the SPOT host's /api/v3/time).
    let local = now_ms();
    let server = client
        .transport
        .public(DEMO_FAPI_REST, "/fapi/v1/time", &[])
        .expect("fapi time")
        .get("serverTime")
        .and_then(|t| t.as_i64())
        .expect("serverTime");
    client.signer.set_offset_ms(server - local);

    // self-heal: cancel any vtrgtd* order a previously crashed run may have stranded, so the
    // final book-flat assertion judges only THIS run.
    if let Ok(open) = client.transport.signed(
        DEMO_FAPI_REST,
        FAPI_OPEN_ORDERS,
        "GET",
        &[("symbol", "BTCUSDT".into())],
        &client.signer,
    ) {
        for o in open.as_array().unwrap_or(&vec![]) {
            if let Some(c) = o.get("clientOrderId").and_then(|c| c.as_str())
                && c.starts_with("vtrgtd")
            {
                tracing::warn!(target: "vike_binance", "pre-clean: canceling stranded {c}");
                let _ = VenueRest::cancel_order(&client, c);
            }
        }
    }

    // live mark for far-from-market pricing
    let mark = client
        .transport
        .public(DEMO_FAPI_REST, "/fapi/v1/ticker/price", &[("symbol", "BTCUSDT".into())])
        .expect("ticker")
        .get("price")
        .and_then(|p| p.as_str())
        .and_then(|p| p.parse::<f64>().ok())
        .expect("price");
    assert!(mark > 0.0, "ticker mark must be live");

    // far-below LIMIT BUY that must REST: half the mark never crosses (fapi PERCENT_PRICE only
    // bounds the aggressive side of a BUY), notional comfortably >= the venue minimum.
    let limit_px = mark * 0.5;
    let qty = (client.properties.min_notional.max(5.0) * 1.6) / limit_px;

    // goodTillDate ~1h out, pre-truncated to a whole second (the venue keeps second-level
    // precision and ignores the ms part — a whole-second expiry must round-trip EXACTLY).
    let exp = (now_ms() + 3_600_000) / 1000 * 1000;
    let coid = format!("vtrgtd{}", now_ms() % 100_000_000);
    let mut req = tif_req(&coid, qty, limit_px, TimeInForce::Gtd);
    req.gtd_expiry = Some(exp);
    let events = VenueRest::submit_order(&client, &req);
    tracing::debug!(target: "vike_binance", "submit events: {events:?}");
    assert!(
        matches!(events.last(), Some(Event::OrderAccepted(_))),
        "GTD submit must be ACK'd: {events:?}"
    );

    // the venue's own record: timeInForce=GTD, the EXACT goodTillDate, resting NEW, zero fills.
    // Bounded retry: fapi's order-query replica can lag the submit ack by a moment (-2013
    // "Order does not exist" straight after an ACCEPTED submit — observed live).
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
    let got_tif = order.get("timeInForce").and_then(|v| v.as_str());
    let got_gtd = order.get("goodTillDate").and_then(|v| v.as_i64());
    let status = order.get("status").and_then(|v| v.as_str());
    let executed = order.get("executedQty").and_then(|v| v.as_str()).unwrap_or("0");
    tracing::info!(
        target: "vike_binance",
        "{coid}: timeInForce={got_tif:?} goodTillDate={got_gtd:?} status={status:?} executedQty={executed}"
    );
    assert_eq!(got_tif, Some("GTD"), "the venue's record must carry GTD");
    assert_eq!(got_gtd, Some(exp), "the venue's record must carry the request's exact expiry");
    assert_eq!(status, Some("NEW"), "a far-from-market GTD LIMIT must REST");
    assert_eq!(executed.parse::<f64>().unwrap_or(1.0), 0.0, "no fill far from market");

    // cancel + flat
    VenueRest::cancel_order(&client, &coid).expect("cancel");
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
            o.get("clientOrderId").and_then(|c| c.as_str()).is_none_or(|c| !c.starts_with("vtrgtd"))
        }),
        "no gtd-smoke order may rest"
    );

    // invalid-horizon probe, VENUE-side shape: tamper goodTillDate below the now+600s floor and
    // POST raw — bypassing the local gate on purpose (the gate would otherwise deny before the
    // wire; its client-side pair is pinned offline in `tif_gate.rs` and re-proven below). The
    // venue must reject loudly (fapi -5040 FUTURE_GOOD_TILL_DATE).
    let coid2 = format!("vtrgtdX{}", now_ms() % 100_000_000);
    let mut params = client.build_order_params(&req);
    for (k, v) in params.iter_mut() {
        if *k == "newClientOrderId" {
            *v = coid2.clone();
        }
        if *k == "goodTillDate" {
            *v = (now_ms() + 60_000).to_string();
        }
    }
    let err = client
        .transport
        .signed(DEMO_FAPI_REST, FAPI_ORDER, "POST", &params, &client.signer)
        .expect_err("an invalid-horizon goodTillDate must be rejected by the venue");
    tracing::info!(
        target: "vike_binance",
        "venue rejection shape for short-horizon GTD: code={} msg={}", err.code, err.msg
    );

    // ...and through the NORMAL submit path the same violation is a proper terminal
    // OrderRejected (the local gate, wire untouched) — an operator never sees silence.
    let mut near =
        tif_req(&format!("vtrgtdN{}", now_ms() % 100_000_000), qty, limit_px, TimeInForce::Gtd);
    near.gtd_expiry = Some(now_ms() + 60_000);
    let events = VenueRest::submit_order(&client, &near);
    assert!(
        matches!(&events[1], Event::OrderRejected(r) if r.reason.contains("600s")),
        "short-horizon GTD must be loud-denied client-side: {events:?}"
    );

    tracing::info!(
        target: "vike_binance",
        "binance perp GTD ladder green: GTD+goodTillDate on the venue record, canceled clean, \
         invalid horizon rejected loudly venue-side (code={}) and client-side", err.code
    );
}
