//! The pure order mapping in `event_mapper`: `build_order_body`'s wire fields (side, type, qty,
//! limit price, TIF with `Gtd` coerced to `gtc`, the wire name the caller read off the kept row),
//! `map_order_response` (accepted vs rejected) and `decode_trade_event` (a fill is a
//! bare `Fill` then `OrderFilled`, with the RFC3339 timestamp parsed; a cancel is `OrderCanceled`, an expiry
//! `OrderExpired`).

use std::assert_matches;
use vike_alpaca::{build_order_body, decode_trade_event, map_order_response};
use vike_model::events::Event;
use vike_model::{OrderRequest, TimeInForce};

fn req(order_type: &str, side: i32, qty: f64) -> OrderRequest {
    OrderRequest {
        order_client_id: "order-client-id-1".into(),
        venue: "alpaca".into(),
        symbol: "AAPL".into(),
        side,
        qty,
        order_type: vike_model::OrderType::from_stored(order_type),
        price: Some(150.25),
        trigger_price: Some(148.0),
        time_in_force: TimeInForce::Day,
        timestamp: 111,
        ..Default::default()
    }
}

#[test]
fn market_body_basic() {
    let b = build_order_body(&req("market", 1, 10.0), "AAPL");
    assert_eq!(b["symbol"], "AAPL");
    assert_eq!(b["side"], "buy");
    assert_eq!(b["type"], "market");
    assert_eq!(b["qty"], "10");
    assert_eq!(b["time_in_force"], "day");
    assert_eq!(b["client_order_id"], "order-client-id-1");
    assert!(b.get("limit_price").is_none());
}

#[test]
fn sell_limit_carries_price() {
    let b = build_order_body(&req("limit", -1, 5.0), "AAPL");
    assert_eq!(b["side"], "sell");
    assert_eq!(b["type"], "limit");
    assert_eq!(b["limit_price"], "150.25");
}

/// The body carries the wire name it is handed (the caller read it off the kept row), never the order's user name.
#[test]
fn the_body_carries_the_wire_name() {
    let mut r = req("market", 1, 0.01);
    r.symbol = "BTCUSD".into();
    assert_eq!(build_order_body(&r, "BTC/USD")["symbol"], "BTC/USD");
}

#[test]
fn gtd_coerces_to_gtc() {
    let mut r = req("limit", 1, 1.0);
    r.time_in_force = TimeInForce::Gtd;
    assert_eq!(build_order_body(&r, "AAPL")["time_in_force"], "gtc");
}

#[test]
fn accepted_response_maps_to_accepted() {
    let response: serde_json::Value = serde_json::from_str(
        r#"{"id":"abc-123","client_order_id":"order-client-id-1","status":"accepted","symbol":"AAPL"}"#,
    )
    .unwrap();
    let evs = map_order_response("order-client-id-1", 111, &response);
    assert_eq!(evs.len(), 1);
    assert_matches!(
        &evs[0], Event::OrderAccepted(a) if a.venue_order_id.as_deref() == Some("abc-123")
    );
}

/// A 2xx with no `id` is no refusal (Alpaca refuses with a 4xx, which never reaches this mapper): the
/// venue may hold the order, so nothing is emitted and the order stays `Submitted`. An empty 2xx body
/// reads `null` (`ALPACA_HTTP`'s `empty_2xx_is_null`).
#[test]
fn a_2xx_without_an_order_id_is_unknown_not_rejected() {
    for response in [
        serde_json::Value::Null,
        serde_json::json!({}),
        serde_json::json!({"code": 40310000, "message": "insufficient buying power"}),
    ] {
        let evs = map_order_response("order-client-id-1", 111, &response);
        assert!(evs.is_empty(), "{response}: {evs:?}");
    }
}

#[test]
fn fill_event_maps_to_fill_plus_orderfilled() {
    let v: serde_json::Value = serde_json::from_str(
        r#"{"event":"fill","execution_id":"e-9","timestamp":"2026-07-14T05:39:31.4Z","price":"151.00","qty":"10",
            "order":{"client_order_id":"order-client-id-1","id":"abc-123","symbol":"AAPL","side":"buy","filled_avg_price":"151.00","filled_qty":"10"}}"#,
    ).unwrap();
    let evs = decode_trade_event(&v, &Default::default());
    assert_eq!(evs.len(), 2, "dual-publish: bare Fill then OrderFilled");
    match &evs[0] {
        Event::Fill(f) => {
            assert_eq!(f.side, 1);
            assert_eq!(f.last_qty, 10.0);
            assert_eq!(f.last_price, 151.0);
            // Real RFC3339 SSE timestamp ("2026-07-14T05:39:31.4Z") must parse to a nonzero
            // epoch-ms value, not fall back to 0 (regression guard for the parse_timestamp_ms fix).
            assert_eq!(
                f.timestamp, 1_784_007_571_400,
                "RFC3339 timestamp must parse to real epoch-ms"
            );
        }
        o => panic!("{o:?}"),
    }
    assert_matches!(&evs[1], Event::OrderFilled(of) if of.fill.trade_id == "e-9" && of.timestamp > 0);
}

/// A fill with no positive price (its own or the order's `filled_avg_price`) or no positive `qty` publishes
/// nothing, never a fill at 0; the same frame with its price still fills at that price.
#[test]
fn a_fill_without_a_price_or_qty_publishes_nothing() {
    let frame = |price: Option<&str>, qty: Option<&str>| {
        let mut v = serde_json::json!({"event": "fill", "execution_id": "e-9",
            "timestamp": "2026-07-14T05:39:31.4Z",
            "order": {"client_order_id": "order-client-id-1", "id": "abc-123", "symbol": "AAPL", "side": "buy"}});
        if let Some(price) = price {
            v["price"] = price.into();
        }
        if let Some(qty) = qty {
            v["qty"] = qty.into();
        }
        decode_trade_event(&v, &Default::default())
    };
    for (price, qty) in
        [(None, Some("10")), (Some("0"), Some("10")), (Some("x"), Some("10")), (Some("151"), None)]
    {
        assert!(frame(price, qty).is_empty(), "price {price:?} qty {qty:?}");
    }
    assert_matches!(
        frame(Some("151.00"), Some("10")).as_slice(),
        [Event::Fill(f), Event::OrderFilled(_)] if f.last_price == 151.0 && f.last_qty == 10.0
    );
}

/// An expired order (a DAY order at the close) is `OrderExpired`, as alpaca's reconcile reads `expired` and as the
/// binance, bybit and ctrader streams say it (owner ruling L2), never a cancel.
#[test]
fn expired_event_maps_to_expired() {
    let v: serde_json::Value = serde_json::from_str(
        r#"{"event":"expired","timestamp":"2026-07-14T05:39:31.4Z","order":{"client_order_id":"order-client-id-1"}}"#,
    )
    .unwrap();
    assert_matches!(
        decode_trade_event(&v, &Default::default()).as_slice(),
        [Event::OrderExpired(e)] if e.order_client_id == "order-client-id-1" && e.timestamp == 1_784_007_571_400
    );
}

#[test]
fn canceled_event_maps_to_canceled() {
    let v: serde_json::Value = serde_json::from_str(
        r#"{"event":"canceled","order":{"client_order_id":"order-client-id-1"}}"#,
    )
    .unwrap();
    assert_matches!(
        &decode_trade_event(&v, &Default::default())[0], Event::OrderCanceled(c) if c.order_client_id == "order-client-id-1"
    );
}
