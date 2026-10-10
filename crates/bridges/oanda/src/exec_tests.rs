use super::*;
use std::assert_matches;

fn req(order_type: &str, side: i32, qty: f64) -> OrderRequest {
    OrderRequest {
        account: None,
        combo_legs: Vec::new(),
        client_order_id: "coid-1".into(),
        venue: VENUE.into(),
        symbol: "EURUSD".into(),
        side,
        qty,
        order_type: order_type.into(),
        price: Some(1.09),
        trigger_price: Some(1.08),
        reduce_only: false,
        time_in_force: TimeInForce::Gtc,
        gtd_expiry: None,
        ts: 111,
        parent_order_id: None,
        linked_order_ids: vec![],
        order_list_id: None,
        contingency_type: None,
        weight: 0.0,
        stop: None,
        trail: None,
        extreme: None,
        on_close: false,
        margin_mode: None,
        trigger_by: None,
    }
}

#[test]
fn market_body_is_signed_fok() {
    let b = build_order_body(&req("market", -1, 1000.0));
    let o = &b["order"];
    assert_eq!(o["type"], "MARKET");
    assert_eq!(o["timeInForce"], "FOK");
    assert_eq!(o["instrument"], "EUR_USD");
    assert_eq!(o["units"], "-1000"); // sell → negative units
    assert_eq!(o["clientExtensions"]["id"], "coid-1");
}

#[test]
fn limit_body_carries_price() {
    let b = build_order_body(&req("limit", 1, 500.0));
    assert_eq!(b["order"]["type"], "LIMIT");
    assert_eq!(b["order"]["units"], "500");
    assert_eq!(b["order"]["price"], "1.09000");
    assert_eq!(b["order"]["timeInForce"], "GTC"); // default TIF
}

#[test]
fn tif_flows_through() {
    let mut r = req("limit", 1, 500.0);
    r.time_in_force = TimeInForce::Ioc;
    assert_eq!(build_order_body(&r)["order"]["timeInForce"], "IOC");
    // market coerces a non-immediate TIF to FOK
    let mut m = req("market", 1, 500.0);
    m.time_in_force = TimeInForce::Gtc;
    assert_eq!(build_order_body(&m)["order"]["timeInForce"], "FOK");
}

/// Equivalence gate for the `venue_tif` routing: the working-order arm's five recorded wire
/// strings, asserted BOTH through `oanda_tif` and against this venue's row of the cross-venue
/// table (byte-for-byte), plus the local MARKET arm's FOK/IOC-only coercion (a genuine venue
/// constraint kept OUT of the resting-path table — see `vike_model::venues::venue_tif`'s module doc).
#[test]
fn tif_truth_table_matches_the_venue_tif_row() {
    use vike_model::TimeInForce::{Day, Fok, Gtc, Gtd, Ioc};
    // Working-order arm — recorded strings; must equal the "oanda" table row.
    for (tif, want) in [(Gtc, "GTC"), (Ioc, "IOC"), (Fok, "FOK"), (Gtd, "GTD"), (Day, "GFD")] {
        assert_eq!(oanda_tif(tif, false), want, "working {tif:?}");
        assert_eq!(
            vike_model::venues::venue_tif::venue_tif(VENUE, tif).wire(),
            Some(want),
            "table row {tif:?}"
        );
    }
    // MARKET arm — local by design (OANDA MARKET accepts only FOK/IOC): recorded strings.
    for (tif, want) in [(Gtc, "FOK"), (Ioc, "IOC"), (Fok, "FOK"), (Gtd, "FOK"), (Day, "FOK")] {
        assert_eq!(oanda_tif(tif, true), want, "market {tif:?}");
    }
}

#[test]
fn market_fill_maps_to_accepted_plus_filled() {
    let resp: serde_json::Value = serde_json::from_str(
            r#"{
                "orderCreateTransaction": {"id": "6372", "type": "MARKET_ORDER"},
                "orderFillTransaction": {"id": "6373", "time": "1478012400.000000000",
                    "instrument": "EUR_USD", "units": "1000", "price": "1.09000", "commission": "0.04"},
                "lastTransactionID": "6373"
            }"#,
        )
        .unwrap();
    // Dual-publish: Accepted, then the bare Fill (Account folds position/PnL), then the
    // OrderFilled wrap (FSM), both fills carrying the same trade_id.
    let evs = map_order_response("coid-1", 111, &resp);
    assert_eq!(evs.len(), 3);
    assert_matches!(
        &evs[0], Event::OrderAccepted(a) if a.venue_order_id.as_deref() == Some("6372")
    );
    match &evs[1] {
        Event::Fill(fill) => {
            assert_eq!(fill.trade_id, "6373");
            assert_eq!(fill.side, 1);
            assert_eq!(fill.last_qty, 1000.0);
            assert_eq!(fill.last_px, 1.09);
            assert_eq!(fill.commission, 0.04);
            assert_eq!(fill.ts, 1_478_012_400_000);
        }
        other => panic!("expected bare Fill second, got {other:?}"),
    }
    match &evs[2] {
        Event::OrderFilled(of) => {
            assert_eq!(of.fill.trade_id, "6373"); // same fill on the wrap
            assert_eq!(of.fill.side, 1);
        }
        other => panic!("expected OrderFilled wrap third, got {other:?}"),
    }
}

#[test]
fn last_transaction_id_watermark_advances_monotonically() {
    let last_seen = Arc::new(AtomicU64::new(0));
    // A POST response carrying lastTransactionID=6373 advances the watermark.
    note_last_transaction_id(&last_seen, &serde_json::json!({"lastTransactionID": "6373"}));
    assert_eq!(last_seen.load(Ordering::Relaxed), 6373);
    // A LOWER (stale/out-of-order) id must NOT move it backwards.
    note_last_transaction_id(&last_seen, &serde_json::json!({"lastTransactionID": "10"}));
    assert_eq!(last_seen.load(Ordering::Relaxed), 6373);
    // A higher one advances it.
    note_last_transaction_id(&last_seen, &serde_json::json!({"lastTransactionID": "9000"}));
    assert_eq!(last_seen.load(Ordering::Relaxed), 9000);
    // A response without the field is a no-op.
    note_last_transaction_id(&last_seen, &serde_json::json!({"orderCreateTransaction": {}}));
    assert_eq!(last_seen.load(Ordering::Relaxed), 9000);
}

#[test]
fn reject_maps_to_rejected() {
    let resp: serde_json::Value = serde_json::from_str(
        r#"{"orderRejectTransaction": {"id": "6372", "rejectReason": "INSUFFICIENT_MARGIN"}}"#,
    )
    .unwrap();
    let evs = map_order_response("coid-1", 111, &resp);
    assert_eq!(evs.len(), 1);
    assert_matches!(&evs[0], Event::OrderRejected(r) if r.reason == "INSUFFICIENT_MARGIN");
}
