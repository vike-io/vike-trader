//! A TAKE-PROFIT on okx (market-if-touched without a price, limit-if-touched with one) is ONE
//! `ordType=conditional` algo order, the stop's own, on its TAKE-PROFIT side: `tpTriggerPx` the trigger,
//! `tpOrdPx` the limit or `-1` for market (the official "Place algo order" table,
//! <https://www.okx.com/docs-v5/en/#order-book-trading-algo-trading-post-place-algo-order>; the rows read
//! back are the "Algo order list" table's,
//! <https://www.okx.com/docs-v5/en/#order-book-trading-algo-trading-get-algo-order-list>). Pinned here with
//! the recording transport of `stop_limit.rs`:
//!
//! - the submit: the take-profit body is the stop's byte for byte with the stop-loss words renamed to the
//!   take-profit ones, and nothing of the stop-loss side is sent (the venue performs only the stop-loss
//!   when both sides are sent);
//! - the refusals before the wire, and the batch's refusal by name;
//! - the order report off the DOCUMENTED pending row (the vendor's own example, kept verbatim in
//!   `tests/fixtures/algo/doc_orders_algo_pending_take_profit.json`);
//! - the cancel of a FIRED limit-if-touched, off the RECORDED fired-stop row (`rest_order_algo_fired.json`)
//!   with its stop-loss side moved to the take-profit side: the child limit is cancelled by `ordId`, as a
//!   fired stop-limit's is; a fired market-if-touched's cancel makes exactly a fired stop's calls.

use serde_json::{Value, json};
use vike_bridge_core::rest::{OrderCancelAck, VenueRest};
use vike_model::events::Event;
use vike_model::{OrderRequest, OrderStatus, OrderType, TimeInForce, TriggerBy};
use vike_okx::execution::rest::VenueRestOkx;
use vike_okx::reconcile::{conditional_kind, parse_algo_pending};

use crate::stop_limit::{Wire, accepted, done, refused, rest};

/// A SELL take-profit closing a long on the fixture's BTC-USDT-SWAP (tick 0.1, ctVal 0.01): one contract.
fn take_profit(limit: Option<f64>, trigger: Option<f64>) -> OrderRequest {
    OrderRequest {
        order_client_id: "sl1".into(),
        venue: "okx".into(),
        symbol: "BTC-USDT-SWAP".into(),
        side: -1,
        qty: 0.01,
        order_type: OrderType::TakeProfit,
        price: limit,
        trigger_price: trigger,
        reduce_only: true,
        ..OrderRequest::default()
    }
}

fn body_of(c: &VenueRestOkx<Wire>, request: &OrderRequest) -> (Vec<Event>, Value) {
    let events = VenueRest::submit_order(c, request);
    assert_eq!(c.transport.calls(), ["POST order-algo"], "one algo POST and nothing else");
    (events, c.transport.params(0))
}

fn algo_post_only() -> Wire {
    Wire::new(|tail, _| match tail {
        "order-algo" => Ok(accepted()),
        other => panic!("only order-algo may be called, got {other}"),
    })
}

fn submitted_body(request: &OrderRequest) -> (Vec<Event>, Value) {
    body_of(&rest(algo_post_only()), request)
}

// ---- submit ---------------------------------------------------------------------------------------

#[test]
fn a_take_profit_is_the_stops_conditional_algo_order_on_its_take_profit_side() {
    // market-if-touched: no price, so `tpOrdPx -1`
    let (events, mit) = submitted_body(&take_profit(None, Some(62_000.04)));
    match &events[..] {
        [Event::OrderSubmitted(_), Event::OrderAccepted(a)] => {
            assert_eq!(a.venue_order_id.as_deref(), Some("algo-1"), "the venue id is the algoId");
        }
        other => panic!("Submitted + Accepted, got {other:?}"),
    }
    assert_eq!(mit["ordType"], "conditional");
    assert_eq!(mit["tpTriggerPx"], "62000.0", "the trigger, down to the 0.1 tick grid");
    assert_eq!(mit["tpOrdPx"], "-1", "market on trigger");
    assert_eq!(mit["sz"], "1.00", "0.01 BTC = one contract");
    assert_eq!(mit["reduceOnly"], true);
    assert_eq!(mit["algoClOrdId"], "sl1");
    for key in ["slTriggerPx", "slOrdPx", "slTriggerPxType", "tpOrdKind"] {
        assert!(mit.get(key).is_none(), "{key} is not sent: {mit}");
    }

    // limit-if-touched: the limit in `tpOrdPx`, on the tick grid
    let (_, lit) = submitted_body(&take_profit(Some(62_100.06), Some(62_000.0)));
    assert_eq!(lit["tpTriggerPx"], "62000.0");
    assert_eq!(lit["tpOrdPx"], "62100.0", "the limit, down to the 0.1 tick grid");

    // The tie: the stop's body with its stop-loss words renamed is the take-profit's, byte for byte.
    let stop = OrderRequest {
        order_type: OrderType::Stop,
        price: None,
        ..take_profit(None, Some(62_000.04))
    };
    let (_, stop_body) = submitted_body(&stop);
    let renamed: serde_json::Map<String, Value> = stop_body
        .as_object()
        .unwrap()
        .iter()
        .map(|(k, v)| (k.replace("sl", "tp").replace("Sl", "Tp"), v.clone()))
        .collect();
    assert_eq!(Value::Object(renamed), mit, "everything but the side's words is the stop's");
}

#[test]
fn a_take_profits_trigger_source_rides_tp_trigger_px_type() {
    for (source, wire) in
        [(TriggerBy::Last, "last"), (TriggerBy::Mark, "mark"), (TriggerBy::Index, "index")]
    {
        let request = OrderRequest {
            trigger_by: Some(source),
            ..take_profit(Some(62_100.0), Some(62_000.0))
        };
        let (_, body) = submitted_body(&request);
        assert_eq!(body["tpTriggerPxType"], wire, "{source:?}");
        assert!(body.get("slTriggerPxType").is_none(), "{body}");
    }
}

#[test]
fn a_take_profit_that_cannot_be_written_as_one_is_refused_before_the_wire() {
    let cases = [
        ("no trigger", take_profit(None, None), "trigger price"),
        ("a zero trigger", take_profit(None, Some(0.0)), "trigger price"),
        ("a NaN trigger", take_profit(None, Some(f64::NAN)), "finite"),
        ("a NaN limit", take_profit(Some(f64::NAN), Some(62_000.0)), "finite"),
        (
            "a limit IOC",
            OrderRequest {
                time_in_force: TimeInForce::Ioc,
                ..take_profit(Some(62_100.0), Some(62_000.0))
            },
            "time in force",
        ),
        (
            "a limit post-only",
            OrderRequest {
                time_in_force: TimeInForce::PostOnly,
                ..take_profit(Some(62_100.0), Some(62_000.0))
            },
            "time in force",
        ),
    ];
    for (what, request, word) in cases {
        let c = rest(Wire::none());
        match &VenueRest::submit_order(&c, &request)[..] {
            [Event::OrderSubmitted(_), Event::OrderRejected(r)] => {
                assert!(r.reason.contains(word), "{what}: {}", r.reason);
            }
            other => panic!("{what}: Submitted + a terminal Rejected, got {other:?}"),
        }
    }
}

/// A market-if-touched fires a market order, which takes no lifetime: like a stop, its time in force is
/// not judged here (the node's door judges lifetimes).
#[test]
fn a_market_if_touched_is_not_refused_for_its_time_in_force_as_a_stop_is_not() {
    let request =
        OrderRequest { time_in_force: TimeInForce::Ioc, ..take_profit(None, Some(62_000.0)) };
    let (events, body) = submitted_body(&request);
    assert!(matches!(events.last(), Some(Event::OrderAccepted(_))), "{events:?}");
    assert_eq!(body["tpOrdPx"], "-1");
}

/// The batch has no route to the algo endpoint: a take-profit through it is refused by name, never a
/// market row.
#[test]
fn a_take_profit_through_the_batch_is_refused_by_name() {
    let c = rest(Wire::none());
    match &c.submit_batch(&[take_profit(Some(62_100.0), Some(62_000.0))])[..] {
        [Event::OrderSubmitted(_), Event::OrderRejected(r)] => {
            assert!(r.reason.contains("take_profit"), "{}", r.reason);
        }
        other => panic!("Submitted + a terminal Rejected, got {other:?}"),
    }
}

/// The algo body a SPOT pair (`BTC-USDT`, ctVal 1) posts for `request`.
fn spot(request: &OrderRequest) -> Value {
    let c = VenueRestOkx { symbol: "BTC-USDT".into(), ct_val: 1.0, ..rest(algo_post_only()) };
    body_of(&c, &OrderRequest { symbol: "BTC-USDT".into(), ..request.clone() }).1
}

/// A SPOT market-if-touched BUY states `tgtCcy=base_ccy`: the doc's `tgtCcy` row says a spot "Market buy
/// conditional order" counts `sz` in the QUOTE by default (the documented pending row shows
/// `tgtCcy quote_currency` on exactly such an order). A limit one and a sell state nothing.
#[test]
fn a_spot_market_if_touched_buy_counts_its_size_in_the_base() {
    let buy =
        |limit| OrderRequest { side: 1, reduce_only: false, ..take_profit(limit, Some(60_000.0)) };
    let mit_buy = spot(&buy(None));
    assert_eq!(mit_buy["tgtCcy"], "base_ccy", "{mit_buy}");
    assert!(mit_buy.get("posSide").is_none() && mit_buy.get("reduceOnly").is_none(), "{mit_buy}");
    assert!(spot(&buy(Some(59_900.0))).get("tgtCcy").is_none(), "a limit's size is base already");
    let mit_sell = OrderRequest { reduce_only: false, ..take_profit(None, Some(64_000.0)) };
    assert!(spot(&mit_sell).get("tgtCcy").is_none(), "a sell's default is the base");
}

/// FIXED in wave 2, a deliberate move of an EXISTING kind's wire: a SPOT market BUY stop states
/// `tgtCcy=base_ccy`, as a market-if-touched buy does. Without it the doc's `tgtCcy` row counts its `sz`
/// in the QUOTE ("Default is quote_ccy for buy"), so a stop for 0.01 BTC would have bought 0.01 USDT of
/// it. Pinned byte for byte; a SELL stop and a BUY stop-limit keep the bytes they had. Proved against the
/// venue only in the one demo pass after the waves.
#[test]
fn a_spot_market_buy_stop_counts_its_size_in_the_base() {
    let stop = |side: i32, kind: OrderType, limit: Option<f64>| OrderRequest {
        side,
        order_type: kind,
        price: limit,
        reduce_only: false,
        ..take_profit(None, Some(60_000.0))
    };
    assert_eq!(
        spot(&stop(1, OrderType::Stop, None)),
        json!({"instId": "BTC-USDT", "tdMode": "cross", "side": "buy", "ordType": "conditional",
               "sz": "0.01", "algoClOrdId": "sl1", "slTriggerPx": "60000.0", "slOrdPx": "-1",
               "tgtCcy": "base_ccy"}),
        "the buy stop's whole body"
    );
    assert_eq!(
        spot(&stop(-1, OrderType::Stop, None)),
        json!({"instId": "BTC-USDT", "tdMode": "cross", "side": "sell", "ordType": "conditional",
               "sz": "0.01", "algoClOrdId": "sl1", "slTriggerPx": "60000.0", "slOrdPx": "-1"}),
        "a SELL stop keeps its bytes (base is its default)"
    );
    let stop_limit_buy = spot(&stop(1, OrderType::StopLimit, Some(60_100.0)));
    assert!(stop_limit_buy.get("tgtCcy").is_none(), "a stop-limit fires a limit: {stop_limit_buy}");
}

// ---- the order report -----------------------------------------------------------------------------

/// The vendor's documented pending row of a take-profit (`tpTriggerPx 10000`, `tpOrdPx -1`, the
/// stop-loss words empty) reads as a resting `TakeProfit`.
#[test]
fn the_documented_pending_take_profit_row_is_a_resting_take_profit() {
    let envelope = crate::triggered_stop::fixture!("doc_orders_algo_pending_take_profit.json");
    let reports = parse_algo_pending(&envelope["data"].to_string(), 0.01).expect("parses");
    assert_eq!(reports.len(), 1);
    let r = &reports[0];
    assert_eq!(r.order_type, OrderType::TakeProfit);
    assert_eq!(r.status, OrderStatus::Accepted);
    assert_eq!(r.venue_order_id.as_str(), "1753184812254216192");
    assert_eq!(r.side, 1);
    assert!((r.qty - 0.1).abs() < 1e-12, "10 contracts x 0.01: {}", r.qty);
    assert_eq!(conditional_kind(&envelope["data"][0]), (OrderType::TakeProfit, None), "market");
    // its terms: the take-profit side's level, no limit (`tpOrdPx -1`)
    assert_eq!(
        (r.price, r.trigger_price, r.trail, r.trail_percent),
        (None, Some(10000.0), None, None)
    );
}

/// Every reading of the kind: the stop-loss rows read exactly as before, a take-profit row is a
/// `TakeProfit` with its limit, and a row with both sides is the stop-loss's (the venue's rule).
#[test]
fn a_conditional_rows_kind_is_read_off_the_side_it_carries() {
    let row = |pairs: &[(&str, &str)]| {
        let mut r = json!({"ordType": "conditional", "state": "live"});
        for (k, v) in pairs {
            r[*k] = json!(v);
        }
        r
    };
    let cases = [
        (row(&[("slTriggerPx", "58000"), ("slOrdPx", "-1")]), (OrderType::Stop, None)),
        (
            row(&[("slTriggerPx", "58000"), ("slOrdPx", "57900.5")]),
            (OrderType::StopLimit, Some(57_900.5)),
        ),
        (row(&[("tpTriggerPx", "62000"), ("tpOrdPx", "-1")]), (OrderType::TakeProfit, None)),
        (
            row(&[
                ("tpTriggerPx", "62000"),
                ("tpOrdPx", "62100"),
                ("slTriggerPx", ""),
                ("slOrdPx", ""),
            ]),
            (OrderType::TakeProfit, Some(62_100.0)),
        ),
        (
            row(&[
                ("slTriggerPx", "58000"),
                ("slOrdPx", "-1"),
                ("tpTriggerPx", "62000"),
                ("tpOrdPx", "62100"),
            ]),
            (OrderType::Stop, None),
        ),
        (row(&[]), (OrderType::Stop, None)),
    ];
    for (r, want) in cases {
        assert_eq!(conditional_kind(&r), want, "{r}");
    }
}

// ---- cancel of a FIRED take-profit ----------------------------------------------------------------

/// The RECORDED fired stop row (`effective`, its child in `ordIdList`), its stop-loss side moved to the
/// take-profit side with `tp_ord_px` as the order price.
fn fired_take_profit(tp_ord_px: &str) -> Value {
    let mut envelope = crate::triggered_stop::fixture!("rest_order_algo_fired.json");
    let row = &mut envelope["data"][0];
    let trigger = row["slTriggerPx"].clone();
    row["tpTriggerPx"] = trigger;
    row["tpTriggerPxType"] = row["slTriggerPxType"].clone();
    row["tpOrdPx"] = json!(tp_ord_px);
    for key in ["slTriggerPx", "slOrdPx", "slTriggerPxType"] {
        row[key] = json!("");
    }
    row["actualSide"] = json!("tp");
    row["algoClOrdId"] = json!("sl1");
    envelope
}

fn fired_cancel(tp_ord_px: &'static str, child: Value) -> VenueRestOkx<Wire> {
    rest(Wire::new(move |tail, params| match tail {
        "cancel-order" if params.get("clOrdId").is_some() => Ok(refused("51400")),
        "cancel-order" => Ok(child.clone()),
        "cancel-algos" => Ok(refused("51400")),
        "order-algo" => Ok(fired_take_profit(tp_ord_px)),
        other => panic!("unexpected {other}"),
    }))
}

#[test]
fn a_fired_limit_if_touched_resting_order_is_cancelled_by_its_order_id() {
    let c = fired_cancel("80300", done());
    assert_eq!(c.cancel_order("sl1").expect("cancelled"), OrderCancelAck::Canceled);
    assert_eq!(
        c.transport.calls(),
        ["POST cancel-order", "POST cancel-algos", "GET order-algo", "POST cancel-order"]
    );
    assert_eq!(
        c.transport.params(3),
        json!({"instId": "BTC-USDT-SWAP", "ordId": "4038740198699216897"}),
        "the recorded child, by the venue's own order id"
    );
}

/// A fired market-if-touched fired a market order that rests nowhere: its cancel makes exactly a fired
/// stop's three calls and stays the non-terminal refusal.
#[test]
fn a_fired_market_if_touched_cancel_makes_a_fired_stops_calls() {
    let c = fired_cancel("-1", done());
    assert_eq!(c.cancel_order("sl1").expect_err("not cancelled").code, 51412);
    assert_eq!(c.transport.calls(), ["POST cancel-order", "POST cancel-algos", "GET order-algo"]);
}
