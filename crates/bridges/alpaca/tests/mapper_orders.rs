use vike_alpaca::{build_order_body, decode_trade_event, map_order_response};
use vike_model::events::Event;
use vike_model::{OrderRequest, TimeInForce};

fn req(order_type: &str, side: i32, qty: f64) -> OrderRequest {
    OrderRequest {
        client_order_id: "coid-1".into(),
        venue: "alpaca".into(),
        symbol: "AAPL".into(),
        side,
        qty,
        order_type: order_type.into(),
        price: Some(150.25),
        trigger_price: Some(148.0),
        reduce_only: false,
        time_in_force: TimeInForce::Day,
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
        combo_legs: Vec::new(),
    }
}

#[test]
fn market_body_basic() {
    let b = build_order_body(&req("market", 1, 10.0));
    assert_eq!(b["symbol"], "AAPL");
    assert_eq!(b["side"], "buy");
    assert_eq!(b["type"], "market");
    assert_eq!(b["qty"], "10");
    assert_eq!(b["time_in_force"], "day");
    assert_eq!(b["client_order_id"], "coid-1");
    assert!(b.get("limit_price").is_none());
}

#[test]
fn sell_limit_carries_price() {
    let b = build_order_body(&req("limit", -1, 5.0));
    assert_eq!(b["side"], "sell");
    assert_eq!(b["type"], "limit");
    assert_eq!(b["limit_price"], "150.25");
}

#[test]
fn crypto_symbol_is_slashed() {
    let mut r = req("market", 1, 0.01);
    r.symbol = "BTCUSD".into();
    assert_eq!(build_order_body(&r)["symbol"], "BTC/USD");
}

#[test]
fn equity_btc_suffix_not_slashed() {
    // GBTC (Grayscale Bitcoin Trust) and FBTC (Fidelity Wise Origin Bitcoin Fund) are real
    // US-listed equity tickers ending in "BTC" — they must pass through unmangled, not get
    // auto-slashed into a nonexistent "G/BTC" / "F/BTC" crypto pair.
    let mut r = req("market", 1, 10.0);
    r.symbol = "GBTC".into();
    assert_eq!(build_order_body(&r)["symbol"], "GBTC");

    r.symbol = "FBTC".into();
    assert_eq!(build_order_body(&r)["symbol"], "FBTC");

    // USD suffix auto-slashing still works.
    r.symbol = "BTCUSD".into();
    assert_eq!(build_order_body(&r)["symbol"], "BTC/USD");

    // A pre-slashed BTC-quoted crypto pair passes through unchanged.
    r.symbol = "ETH/BTC".into();
    assert_eq!(build_order_body(&r)["symbol"], "ETH/BTC");
}

#[test]
fn gtd_coerces_to_gtc() {
    let mut r = req("limit", 1, 1.0);
    r.time_in_force = TimeInForce::Gtd;
    assert_eq!(build_order_body(&r)["time_in_force"], "gtc");
}

#[test]
fn accepted_response_maps_to_accepted() {
    let resp: serde_json::Value = serde_json::from_str(
        r#"{"id":"abc-123","client_order_id":"coid-1","status":"accepted","symbol":"AAPL"}"#,
    )
    .unwrap();
    let evs = map_order_response("coid-1", 111, &resp);
    assert_eq!(evs.len(), 1);
    assert!(
        matches!(&evs[0], Event::OrderAccepted(a) if a.venue_order_id.as_deref() == Some("abc-123"))
    );
}

#[test]
fn error_response_maps_to_rejected() {
    let resp: serde_json::Value =
        serde_json::from_str(r#"{"code":40310000,"message":"insufficient buying power"}"#).unwrap();
    let evs = map_order_response("coid-1", 111, &resp);
    assert!(matches!(&evs[0], Event::OrderRejected(r) if r.reason == "insufficient buying power"));
}

#[test]
fn fill_event_maps_to_fill_plus_orderfilled() {
    let v: serde_json::Value = serde_json::from_str(
        r#"{"event":"fill","execution_id":"e-9","timestamp":"2026-07-14T05:39:31.4Z","price":"151.00","qty":"10",
            "order":{"client_order_id":"coid-1","id":"abc-123","symbol":"AAPL","side":"buy","filled_avg_price":"151.00","filled_qty":"10"}}"#,
    ).unwrap();
    let evs = decode_trade_event(&v);
    assert_eq!(evs.len(), 2, "dual-publish: bare Fill then OrderFilled");
    match &evs[0] {
        Event::Fill(f) => {
            assert_eq!(f.side, 1);
            assert_eq!(f.last_qty, 10.0);
            assert_eq!(f.last_px, 151.0);
            // Real RFC3339 SSE timestamp ("2026-07-14T05:39:31.4Z") must parse to a nonzero
            // epoch-ms value, not fall back to 0 (regression guard for the parse_ts_ms fix).
            assert_eq!(f.ts, 1_784_007_571_400, "RFC3339 timestamp must parse to real epoch-ms");
        }
        o => panic!("{o:?}"),
    }
    assert!(matches!(&evs[1], Event::OrderFilled(of) if of.fill.trade_id == "e-9" && of.ts > 0));
}

#[test]
fn canceled_event_maps_to_canceled() {
    let v: serde_json::Value =
        serde_json::from_str(r#"{"event":"canceled","order":{"client_order_id":"coid-1"}}"#)
            .unwrap();
    assert!(
        matches!(&decode_trade_event(&v)[0], Event::OrderCanceled(c) if c.client_order_id == "coid-1")
    );
}
