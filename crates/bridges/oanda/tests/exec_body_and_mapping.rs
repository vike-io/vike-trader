//! Integration coverage for the OANDA exec half's pure helpers: `build_order_body` (vike
//! `OrderRequest` -> the v20 order-POST body) and `map_order_response` (v20 order-POST response
//! -> vike events), plus the A3 exec-side watermark `note_last_transaction_id`. All three are
//! re-exported `#[doc(hidden)]` from the private `exec` module (see `lib.rs`) since their only
//! production caller is the network-calling `run()` loop.

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use vike_model::events::Event;
use vike_model::{OrderRequest, TimeInForce};
use vike_oanda::{build_order_body, map_order_response, note_last_transaction_id};

fn base_req(order_type: &str, side: i32, qty: f64) -> OrderRequest {
    OrderRequest {
        client_order_id: "coid-1".into(),
        venue: "oanda".into(),
        symbol: "EURUSD".into(),
        side,
        qty,
        order_type: order_type.into(),
        ts: 111,
        ..Default::default()
    }
}

// --- build_order_body ------------------------------------------------------------------------

#[test]
fn market_sell_is_signed_negative_and_coerces_to_fok() {
    let b = build_order_body(&base_req("market", -1, 1000.0));
    let o = &b["order"];
    assert_eq!(o["type"], "MARKET");
    assert_eq!(o["instrument"], "EUR_USD");
    assert_eq!(o["units"], "-1000");
    assert_eq!(o["timeInForce"], "FOK");
    assert_eq!(o["clientExtensions"]["id"], "coid-1");
    assert!(o.get("price").is_none(), "MARKET orders never carry a price field");
}

#[test]
fn qty_rounds_to_nearest_whole_unit_away_from_zero() {
    let b = build_order_body(&base_req("market", 1, 1000.5));
    assert_eq!(b["order"]["units"], "1001", "f64::round ties away from zero");
    let b = build_order_body(&base_req("market", 1, 1000.4));
    assert_eq!(b["order"]["units"], "1000");
}

#[test]
fn limit_order_carries_price_at_five_decimal_places() {
    let mut req = base_req("limit", 1, 500.0);
    req.price = Some(1.09);
    let b = build_order_body(&req);
    assert_eq!(b["order"]["type"], "LIMIT");
    assert_eq!(b["order"]["units"], "500");
    assert_eq!(b["order"]["price"], "1.09000");
    assert_eq!(b["order"]["timeInForce"], "GTC", "default TIF for a non-market order");
}

#[test]
fn limit_order_without_a_price_omits_the_price_field() {
    let mut req = base_req("limit", 1, 500.0);
    req.price = None;
    let b = build_order_body(&req);
    assert_eq!(b["order"]["type"], "LIMIT");
    assert!(b["order"].get("price").is_none());
}

#[test]
fn stop_order_uses_trigger_price_not_price() {
    // STOP orders read req.trigger_price, NOT req.price -- a subtle, easy-to-invert contract.
    let mut req = base_req("stop", 1, 100.0);
    req.price = Some(1.09); // must be ignored for STOP
    req.trigger_price = Some(1.075);
    let b = build_order_body(&req);
    assert_eq!(b["order"]["type"], "STOP");
    assert_eq!(b["order"]["price"], "1.07500", "STOP's price field is the TRIGGER price");
}

#[test]
fn stop_order_without_a_trigger_price_omits_the_price_field_even_if_price_is_set() {
    let mut req = base_req("stop", 1, 100.0);
    req.price = Some(1.09);
    req.trigger_price = None;
    let b = build_order_body(&req);
    assert!(b["order"].get("price").is_none());
}

#[test]
fn unrecognized_order_type_silently_falls_back_to_market_with_no_price() {
    // Only "limit" and "stop" are special-cased; anything else (including a plausible-looking
    // "stop_market") becomes a plain MARKET order with NO price field, even if price/trigger_price
    // are set on the request. This documents a real silent-coercion gotcha at the OANDA edge.
    let mut req = base_req("stop_market", 1, 10.0);
    req.price = Some(1.09);
    req.trigger_price = Some(1.08);
    let b = build_order_body(&req);
    assert_eq!(b["order"]["type"], "MARKET");
    assert!(b["order"].get("price").is_none());
}

#[test]
fn time_in_force_truth_table() {
    // Non-market: every TIF maps through 1:1 except Day -> GFD.
    for (tif, want) in [
        (TimeInForce::Ioc, "IOC"),
        (TimeInForce::Fok, "FOK"),
        (TimeInForce::Gtd, "GTD"),
        (TimeInForce::Day, "GFD"),
        (TimeInForce::Gtc, "GTC"),
    ] {
        let mut req = base_req("limit", 1, 10.0);
        req.price = Some(1.0);
        req.time_in_force = tif;
        assert_eq!(build_order_body(&req)["order"]["timeInForce"], want, "limit TIF {tif:?}");
    }
    // Market: only Ioc passes through as IOC; every other TIF coerces to FOK (OANDA MARKET
    // orders accept only FOK/IOC).
    for (tif, want) in [
        (TimeInForce::Ioc, "IOC"),
        (TimeInForce::Fok, "FOK"),
        (TimeInForce::Gtd, "FOK"),
        (TimeInForce::Day, "FOK"),
        (TimeInForce::Gtc, "FOK"),
    ] {
        let mut req = base_req("market", 1, 10.0);
        req.time_in_force = tif;
        assert_eq!(build_order_body(&req)["order"]["timeInForce"], want, "market TIF {tif:?}");
    }
}

#[test]
fn gtd_time_in_force_carries_an_epoch_seconds_gtd_time_only_when_expiry_is_set() {
    let mut req = base_req("limit", 1, 10.0);
    req.price = Some(1.0);
    req.time_in_force = TimeInForce::Gtd;
    req.gtd_expiry = Some(1_478_012_400_000); // ms
    let b = build_order_body(&req);
    assert_eq!(b["order"]["gtdTime"], "1478012400", "ms -> whole epoch-seconds string");

    // GTD selected but no expiry given -> no gtdTime key at all (not "0" or null).
    let mut req2 = base_req("limit", 1, 10.0);
    req2.price = Some(1.0);
    req2.time_in_force = TimeInForce::Gtd;
    req2.gtd_expiry = None;
    assert!(build_order_body(&req2)["order"].get("gtdTime").is_none());
}

#[test]
fn gtd_time_never_appears_on_a_market_order_even_with_gtd_selected() {
    // market coerces TIF to FOK anyway, but the gtdTime gate (`!market && Gtd`) independently
    // guards against ever emitting gtdTime on a MARKET order.
    let mut req = base_req("market", 1, 10.0);
    req.time_in_force = TimeInForce::Gtd;
    req.gtd_expiry = Some(1_000_000);
    assert!(build_order_body(&req)["order"].get("gtdTime").is_none());
}

// --- map_order_response -----------------------------------------------------------------------

#[test]
fn reject_transaction_maps_to_a_single_order_rejected_and_short_circuits() {
    let resp = serde_json::json!({
        "orderRejectTransaction": {"id": "6372", "rejectReason": "INSUFFICIENT_MARGIN"},
        // a contrived, malformed-in-practice response that ALSO carries a fill: the reject arm
        // returns early, so the fill must never surface.
        "orderFillTransaction": {"id": "9", "units": "1", "price": "1.0"}
    });
    let evs = map_order_response("coid-1", 111, &resp);
    assert_eq!(evs.len(), 1, "reject short-circuits before create/fill are even inspected");
    match &evs[0] {
        Event::OrderRejected(r) => {
            assert_eq!(r.client_order_id, "coid-1");
            assert_eq!(r.reason, "INSUFFICIENT_MARGIN");
            assert_eq!(r.ts, 111);
        }
        other => panic!("expected OrderRejected, got {other:?}"),
    }
}

#[test]
fn reject_transaction_missing_reject_reason_falls_back_to_a_generic_reason() {
    let resp = serde_json::json!({"orderRejectTransaction": {"id": "1"}});
    match &map_order_response("coid-1", 1, &resp)[0] {
        Event::OrderRejected(r) => assert_eq!(r.reason, "rejected"),
        other => panic!("expected OrderRejected, got {other:?}"),
    }
}

#[test]
fn accept_only_response_for_a_resting_limit_or_stop_order() {
    let resp = serde_json::json!({"orderCreateTransaction": {"id": "6372", "type": "LIMIT_ORDER"}});
    let evs = map_order_response("coid-1", 111, &resp);
    assert_eq!(evs.len(), 1);
    match &evs[0] {
        Event::OrderAccepted(a) => {
            assert_eq!(a.client_order_id, "coid-1");
            assert_eq!(a.venue_order_id.as_deref(), Some("6372"));
            assert_eq!(a.ts, 111);
        }
        other => panic!("expected OrderAccepted, got {other:?}"),
    }
}

#[test]
fn accept_only_response_missing_create_id_yields_no_venue_order_id() {
    let resp = serde_json::json!({"orderCreateTransaction": {"type": "LIMIT_ORDER"}});
    match &map_order_response("coid-1", 1, &resp)[0] {
        Event::OrderAccepted(a) => assert_eq!(a.venue_order_id, None),
        other => panic!("expected OrderAccepted, got {other:?}"),
    }
}

#[test]
fn market_fill_dual_publishes_accepted_then_bare_fill_then_wrap() {
    let resp = serde_json::json!({
        "orderCreateTransaction": {"id": "6372", "type": "MARKET_ORDER"},
        "orderFillTransaction": {"id": "6373", "time": "1478012400.000000000",
            "instrument": "EUR_USD", "units": "1000", "price": "1.09000", "commission": "0.04"},
        "lastTransactionID": "6373"
    });
    let evs = map_order_response("coid-1", 111, &resp);
    assert_eq!(evs.len(), 3);
    assert!(
        matches!(&evs[0], Event::OrderAccepted(a) if a.venue_order_id.as_deref() == Some("6372"))
    );
    match &evs[1] {
        Event::Fill(fill) => {
            assert_eq!(fill.trade_id, "6373");
            assert_eq!(fill.side, 1);
            assert_eq!(
                fill.last_qty, 1000.0,
                "the whole fill in ONE POST response is the full delta"
            );
            assert_eq!(fill.last_px, 1.09);
            assert_eq!(fill.commission, 0.04);
            assert_eq!(fill.ts, 1_478_012_400_000);
        }
        other => panic!("expected bare Fill second, got {other:?}"),
    }
    match &evs[2] {
        Event::OrderFilled(of) => {
            assert_eq!(of.fill.trade_id, "6373");
            assert_eq!(of.fill.side, 1);
        }
        other => panic!("expected OrderFilled wrap third, got {other:?}"),
    }
}

#[test]
fn response_with_neither_reject_nor_create_nor_fill_yields_no_events() {
    let evs = map_order_response("coid-1", 1, &serde_json::json!({"lastTransactionID": "1"}));
    assert!(evs.is_empty());
}

// --- note_last_transaction_id (A3 exec-side watermark) -----------------------------------------

#[test]
fn watermark_advances_monotonically_and_ignores_missing_or_stale_ids() {
    let last_seen = Arc::new(AtomicU64::new(0));
    note_last_transaction_id(&last_seen, &serde_json::json!({"lastTransactionID": "6373"}));
    assert_eq!(last_seen.load(Ordering::Relaxed), 6373);

    // a LOWER (stale/out-of-order) id must not move it backwards
    note_last_transaction_id(&last_seen, &serde_json::json!({"lastTransactionID": "10"}));
    assert_eq!(last_seen.load(Ordering::Relaxed), 6373);

    // a higher one advances it
    note_last_transaction_id(&last_seen, &serde_json::json!({"lastTransactionID": "9000"}));
    assert_eq!(last_seen.load(Ordering::Relaxed), 9000);

    // a response without the field, or with an unparseable one, is a no-op
    note_last_transaction_id(&last_seen, &serde_json::json!({"orderCreateTransaction": {}}));
    assert_eq!(last_seen.load(Ordering::Relaxed), 9000);
    note_last_transaction_id(&last_seen, &serde_json::json!({"lastTransactionID": "not-a-number"}));
    assert_eq!(last_seen.load(Ordering::Relaxed), 9000);
}
