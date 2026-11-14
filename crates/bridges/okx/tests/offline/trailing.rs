//! A TRAILING order on okx is ONE `ordType=move_order_stop` algo order on the stop's endpoint
//! (`order-algo`): `callbackSpread` the trail (a price distance) or `callbackRatio` a percent trail's
//! percent / 100 (only one is sent), `activePx` the
//! activation (absent = active at once), a market order when it fires (the official "Place algo order"
//! table, <https://www.okx.com/docs-v5/en/#order-book-trading-algo-trading-post-place-algo-order>, read
//! 2026-11-08; the rows read back are the "Algo order list" table's). Pinned here:
//!
//! - the submit body, whole, and the KEY ORDER of every algo body (the stop's, the stop-limit's and the
//!   take-profit's are byte for byte what they were before the trailing order shared their head);
//! - a percent trail's body (`callbackRatio` in `callbackSpread`'s place, the ratio exact) and its row
//!   read back as a `Trailing`;
//! - the refusals before the wire: a `trailing_limit` (okx has none), the shared order terms (a percent's
//!   included), a trail below one tick, a `trigger_by` the order cannot state, a SPOT buy; the batch's and
//!   the amend's refusal by name;
//! - the cancel: the stop's path (`cancel-order`, then `cancel-algos`, then `order-algo`);
//! - the reconcile: ONE account-wide `orders-algo-pending` walk with `ordType=move_order_stop` (strict),
//!   the vendor's documented pending row read as a `Trailing`, and a fired trailing order's child fill
//!   named by ONE account-wide `orders-algo-history` read;
//! - THE GATE, both ways: every trailing-only request (the two reads, the business socket's
//!   `algo-advance` subscription) is made only while okx's row lists `trailing`; closed, a pass and a
//!   socket open are byte for byte what they were. Open, a refused trailing read or subscription is
//!   logged and costs the trailing orders alone, never the stops' reads or channel;
//! - the `algo-advance` push: a trailing row's `effective`/`canceled` reach the order as on `orders-algo`.

use std::net::TcpListener;
use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Mutex};

use serde_json::{Value, json};
use tungstenite::Message;
use vike_bridge_core::rest::{OrderCancelAck, VenueRest};
use vike_bridge_core::signer::SignerJson;
use vike_bridge_core::transport::VenueApiError;
use vike_bridge_core::transport::json::RestTransportJson;
use vike_bridge_core::websocket_session::user::WebsocketSessionUserOpenOutcome;
use vike_execution::recon::ReconcileClient;
use vike_model::events::Event;
use vike_model::{OrderRequest, OrderStatus, OrderType, TriggerBy};
use vike_okx::ReconcileClientOkx;
use vike_okx::execution::event_mapper::map_okx_algo_frame;
use vike_okx::execution::rest::VenueRestOkx;
use vike_okx::execution::user_data::open_okx_algo_websocket;
use vike_okx::reconcile::{conditional_kind, parse_algo_pending};
use vike_okx::signer::OkxV5Signer;
use vike_okx::transport::ratelimit::websocket_rate_gate;
use vike_secrets::credentials::Credentials;

use crate::r6_okx_parity::{fixture, okx_perp};
use crate::stop_limit::{Wire, accepted, done, refused, rest};
use crate::triggered_stop::CHILD_ORDID;

const SYMBOL: &str = "BTC-USDT-SWAP";

/// A SELL trailing order closing a long on the fixture's BTC-USDT-SWAP (tick 0.1, ctVal 0.01): one contract.
fn trailing(trail: Option<f64>, activation: Option<f64>) -> OrderRequest {
    OrderRequest {
        order_client_id: "tr1".into(),
        venue: "okx".into(),
        symbol: SYMBOL.into(),
        side: -1,
        qty: 0.01,
        order_type: OrderType::Trailing,
        trail,
        trigger_price: activation,
        reduce_only: true,
        ..OrderRequest::default()
    }
}

/// [`trailing`] by a PERCENT of the extreme instead of a price distance.
fn by_percent(percent: f64, activation: Option<f64>) -> OrderRequest {
    OrderRequest { trail_percent: Some(percent), ..trailing(None, activation) }
}

fn algo_post_only() -> Wire {
    Wire::new(|tail, _| match tail {
        "order-algo" => Ok(accepted()),
        other => panic!("only order-algo may be called, got {other}"),
    })
}

fn body_of(c: &VenueRestOkx<Wire>, request: &OrderRequest) -> (Vec<Event>, Value) {
    let events = VenueRest::submit_order(c, request);
    assert_eq!(c.transport.calls(), ["POST order-algo"], "one algo POST and nothing else");
    (events, c.transport.params(0))
}

fn submitted_body(request: &OrderRequest) -> (Vec<Event>, Value) {
    body_of(&rest(algo_post_only()), request)
}

fn rejection(events: &[Event]) -> String {
    match events {
        [Event::OrderSubmitted(_), Event::OrderRejected(r)] => r.reason.to_string(),
        other => panic!("Submitted + a terminal Rejected, got {other:?}"),
    }
}

// ---- submit ---------------------------------------------------------------------------------------

#[test]
fn a_trailing_order_is_one_move_order_stop_algo_order_with_its_trail_and_activation() {
    let (events, body) = submitted_body(&trailing(Some(150.07), Some(70_000.04)));
    match &events[..] {
        [Event::OrderSubmitted(_), Event::OrderAccepted(a)] => {
            assert_eq!(a.venue_order_id.as_deref(), Some("algo-1"), "the venue id is the algoId");
        }
        other => panic!("Submitted + Accepted, got {other:?}"),
    }
    assert_eq!(
        body,
        json!({"instId": SYMBOL, "tdMode": "cross", "side": "sell", "ordType": "move_order_stop",
               "sz": "1.00", "posSide": "net", "reduceOnly": true, "algoClOrdId": "tr1",
               "callbackSpread": "150.0", "activePx": "70000.0"}),
        "the trail and the activation down to the 0.1 tick grid; no stop-loss, take-profit or ratio word"
    );
    // no activation: the venue activates it as soon as it is placed, so no `activePx` is sent at all
    let (_, now) = submitted_body(&trailing(Some(150.0), None));
    assert!(now.get("activePx").is_none(), "{now}");
    assert_eq!(now["callbackSpread"], "150.0");
}

/// A PERCENT trail is the same order with `callbackRatio` ("e.g. `0.05` represents 5%") in
/// `callbackSpread`'s place, the percent / 100 EXACTLY (the venue documents no grid for it): one
/// distance word, never both.
#[test]
fn a_percent_trailing_order_writes_its_ratio_in_place_of_the_spread() {
    let (events, body) = submitted_body(&by_percent(1.5, Some(70_000.04)));
    assert!(matches!(events.last(), Some(Event::OrderAccepted(_))), "{events:?}");
    assert_eq!(
        body,
        json!({"instId": SYMBOL, "tdMode": "cross", "side": "sell", "ordType": "move_order_stop",
               "sz": "1.00", "posSide": "net", "reduceOnly": true, "algoClOrdId": "tr1",
               "callbackRatio": "0.015", "activePx": "70000.0"})
    );
    // exact, nothing cut to a grid, and normalized: no trailing zero (2% is "0.02", never "0.020")
    for (percent, ratio) in
        [(0.1, "0.001"), (0.25, "0.0025"), (2.0, "0.02"), (10.0, "0.1"), (33.333, "0.33333")]
    {
        let (_, b) = submitted_body(&by_percent(percent, None));
        assert_eq!(b["callbackRatio"], ratio, "{percent}%: {b}");
        assert!(b.get("callbackSpread").is_none() && b.get("activePx").is_none(), "{b}");
    }
}

/// The shared terms rule judges a percent before the wire: exactly one distance, finite, above 0 and
/// below 100.
#[test]
fn a_percent_trail_the_terms_refuse_is_refused_before_the_wire() {
    let both = OrderRequest { trail: Some(150.0), ..by_percent(1.0, None) };
    let cases = [
        ("a price and a percent", both, "not both"),
        ("a zero percent", by_percent(0.0, None), "not above 0 and below 100"),
        ("a hundred percent", by_percent(100.0, None), "not above 0 and below 100"),
        ("a NaN percent", by_percent(f64::NAN, None), "not a finite number"),
    ];
    for (what, request, word) in cases {
        let why = rejection(&VenueRest::submit_order(&rest(Wire::none()), &request));
        assert!(why.contains(word), "{what}: {why}");
    }
}

/// A stated `trigger_by` of `Last` is accepted and writes nothing (the order states no trigger-price type);
/// `Mark` and `Index` cannot be written and are refused before the wire.
#[test]
fn a_trailing_orders_trigger_source_is_last_or_unstated() {
    let last = OrderRequest { trigger_by: Some(TriggerBy::Last), ..trailing(Some(150.0), None) };
    let (events, body) = submitted_body(&last);
    assert!(matches!(events.last(), Some(Event::OrderAccepted(_))), "{events:?}");
    for key in ["slTriggerPxType", "tpTriggerPxType", "triggerPxType"] {
        assert!(body.get(key).is_none(), "{key}: {body}");
    }
    for source in [TriggerBy::Mark, TriggerBy::Index] {
        let request = OrderRequest { trigger_by: Some(source), ..trailing(Some(150.0), None) };
        let why = rejection(&VenueRest::submit_order(&rest(Wire::none()), &request));
        assert!(why.contains("trigger-price type"), "{source:?}: {why}");
    }
}

#[test]
fn a_trailing_order_that_cannot_be_written_as_one_is_refused_before_the_wire() {
    let limit = OrderRequest {
        order_type: OrderType::TrailingLimit,
        limit_offset: Some(10.0),
        ..trailing(Some(150.0), Some(70_000.0))
    };
    let priced = OrderRequest { price: Some(69_000.0), ..trailing(Some(150.0), None) };
    let cases = [
        ("a trailing limit", limit, "no trailing limit order"),
        ("no trail", trailing(None, None), "trail distance above zero"),
        ("a zero trail", trailing(Some(0.0), None), "trail distance above zero"),
        ("a NaN trail", trailing(Some(f64::NAN), None), "finite"),
        ("a trail under one tick", trailing(Some(0.04), None), "rounds to nothing"),
        ("a stated price", priced, "takes no price"),
        ("a zero activation", trailing(Some(150.0), Some(0.0)), "activation price"),
    ];
    for (what, request, word) in cases {
        let why = rejection(&VenueRest::submit_order(&rest(Wire::none()), &request));
        assert!(why.contains(word), "{what}: {why}");
    }
}

/// A SPOT pair's client (`BTC-USDT`, ctVal 1) after it submitted `request`: its transport holds the call.
fn spot(request: &OrderRequest) -> VenueRestOkx<Wire> {
    let c = VenueRestOkx { symbol: "BTC-USDT".into(), ct_val: 1.0, ..rest(algo_post_only()) };
    let _ =
        VenueRest::submit_order(&c, &OrderRequest { symbol: "BTC-USDT".into(), ..request.clone() });
    c
}

/// A SPOT SELL trailing order is the spot stop's shape (no `posSide`, no `reduceOnly`; a sell's `sz` is
/// base by default). A SPOT BUY is refused: the doc counts a spot market buy's `sz` in the quote and
/// documents `tgtCcy` for conditional orders only.
#[test]
fn a_spot_sell_trailing_order_is_written_and_a_spot_buy_is_refused() {
    let sell = OrderRequest { reduce_only: false, ..trailing(Some(150.0), Some(70_000.0)) };
    let c = spot(&sell);
    assert_eq!(c.transport.calls(), ["POST order-algo"]);
    assert_eq!(
        c.transport.params(0),
        json!({"instId": "BTC-USDT", "tdMode": "cross", "side": "sell", "ordType": "move_order_stop",
               "sz": "0.01", "algoClOrdId": "tr1", "callbackSpread": "150.0", "activePx": "70000.0"})
    );
    let buy = OrderRequest { side: 1, ..sell };
    let c = VenueRestOkx { symbol: "BTC-USDT".into(), ct_val: 1.0, ..rest(Wire::none()) };
    let why =
        rejection(&VenueRest::submit_order(&c, &OrderRequest { symbol: "BTC-USDT".into(), ..buy }));
    assert!(why.contains("spot BUY"), "{why}");
}

/// The batch has no route to the algo endpoint: a trailing order through it is refused by name.
#[test]
fn a_trailing_order_through_the_batch_is_refused_by_name() {
    let c = rest(Wire::none());
    match &c.submit_batch(&[trailing(Some(150.0), None)])[..] {
        [Event::OrderSubmitted(_), Event::OrderRejected(r)] => {
            assert!(r.reason.contains("trailing"), "{}", r.reason);
        }
        other => panic!("Submitted + a terminal Rejected, got {other:?}"),
    }
}

/// "Amend algo order" excludes `move_order_stop`: a trailing order's amend is refused in words, with
/// nothing sent, through both seams.
#[test]
fn a_trailing_orders_amend_is_refused_in_words_and_sends_nothing() {
    let c = rest(Wire::none());
    let order = trailing(Some(150.0), Some(70_000.0));
    for events in [
        VenueRest::modify_order(&c, &order, None, Some(71_000.0)),
        VenueRest::modify_trigger(&c, &order, Some(0.02), None, 71_000.0),
    ] {
        match &events[..] {
            [Event::OrderModifyRejected(r)] => {
                assert!(r.reason.contains("cannot amend a trailing order"), "{}", r.reason);
            }
            other => panic!("one non-terminal OrderModifyRejected, got {other:?}"),
        }
    }
}

// ---- the key order of every algo body ---------------------------------------------------------------

/// Records the KEYS of every `order-algo` POST in the order they are sent (the signed body's bytes).
struct Keys(Mutex<Vec<Vec<String>>>);

impl RestTransportJson for Keys {
    fn signed(
        &self,
        _base: &str,
        _path: &str,
        _method: &str,
        params: &[(&str, Value)],
        _signer: &dyn SignerJson,
    ) -> Result<Value, VenueApiError> {
        self.0.lock().unwrap().push(params.iter().map(|(k, _)| k.to_string()).collect());
        Ok(accepted())
    }
    vike_bridge_core::no_public_call!();
}

/// The stop, the stop-limit and the take-profit keep the exact key order they always had now that the
/// trailing order shares their head; the trailing order's own order follows the same head.
#[test]
fn every_algo_body_keeps_its_key_order() {
    let head =
        ["instId", "tdMode", "side", "ordType", "sz", "posSide", "reduceOnly", "algoClOrdId"];
    let with = |tail: &[&str]| head.iter().chain(tail).map(|k| k.to_string()).collect::<Vec<_>>();
    let base = OrderRequest { trigger_price: Some(58_000.0), ..trailing(None, None) };
    let cases = [
        (
            OrderRequest { order_type: OrderType::Stop, ..base.clone() },
            with(&["slTriggerPx", "slOrdPx"]),
        ),
        (
            OrderRequest {
                order_type: OrderType::Stop,
                trigger_by: Some(TriggerBy::Mark),
                ..base.clone()
            },
            with(&["slTriggerPx", "slOrdPx", "slTriggerPxType"]),
        ),
        (
            OrderRequest {
                order_type: OrderType::StopLimit,
                price: Some(57_900.0),
                ..base.clone()
            },
            with(&["slTriggerPx", "slOrdPx"]),
        ),
        (
            OrderRequest { order_type: OrderType::TakeProfit, ..base.clone() },
            with(&["tpTriggerPx", "tpOrdPx"]),
        ),
        (trailing(Some(150.0), Some(70_000.0)), with(&["callbackSpread", "activePx"])),
        (trailing(Some(150.0), None), with(&["callbackSpread"])),
        (by_percent(1.5, Some(70_000.0)), with(&["callbackRatio", "activePx"])),
    ];
    for (request, want) in cases {
        let c = okx_perp(&fixture(), Keys(Mutex::default()));
        let events = VenueRest::submit_order(&c, &request);
        assert!(matches!(events.last(), Some(Event::OrderAccepted(_))), "{events:?}");
        let sent = c.transport.0.lock().unwrap();
        assert_eq!(sent[..], [want], "{:?}", request.order_type);
    }
}

// ---- cancel -------------------------------------------------------------------------------------------

/// A resting trailing order is not on the regular book: the regular cancel's "unknown" goes on to
/// `cancel-algos` by `algoClOrdId`, as a stop's does.
#[test]
fn a_resting_trailing_order_is_cancelled_on_cancel_algos() {
    let c = rest(Wire::new(|tail, _| match tail {
        "cancel-order" => Ok(refused("51400")),
        "cancel-algos" => Ok(done()),
        other => panic!("unexpected {other}"),
    }));
    assert_eq!(c.cancel_order("tr1").expect("cancelled"), OrderCancelAck::Canceled);
    assert_eq!(c.transport.calls(), ["POST cancel-order", "POST cancel-algos"]);
    assert_eq!(c.transport.params(1), json!([{"instId": SYMBOL, "algoClOrdId": "tr1"}]));
}

/// The RECORDED fired stop row (`effective`, its child in `ordIdList`) as a trailing order's.
fn fired_trailing() -> Value {
    let mut envelope = crate::triggered_stop::fixture!("rest_order_algo_fired.json");
    let row = &mut envelope["data"][0];
    row["ordType"] = json!("move_order_stop");
    row["callbackSpread"] = json!("150");
    row["moveTriggerPx"] = row["slTriggerPx"].clone();
    for key in ["slTriggerPx", "slOrdPx", "slTriggerPxType", "actualSide"] {
        row[key] = json!("");
    }
    row["algoClOrdId"] = json!("tr1");
    envelope
}

/// A FIRED trailing order placed a MARKET order, which rests nowhere: its cancel makes exactly a fired
/// stop's calls and ends in the same non-terminal refusal (no child limit to cancel by `ordId`).
#[test]
fn a_fired_trailing_orders_cancel_is_a_fired_stops() {
    let c = rest(Wire::new(|tail, _| match tail {
        "cancel-order" | "cancel-algos" => Ok(refused("51400")),
        "order-algo" => Ok(fired_trailing()),
        other => panic!("unexpected {other}"),
    }));
    let why = c.cancel_order("tr1").expect_err("a fired trailing order is not cancelled");
    assert!(why.msg.contains("effective"), "{}", why.msg);
    assert_eq!(c.transport.calls(), ["POST cancel-order", "POST cancel-algos", "GET order-algo"]);
    assert_eq!(conditional_kind(&fired_trailing()["data"][0]), (OrderType::Trailing, None));
}

// ---- the order report ---------------------------------------------------------------------------------

/// The vendor's documented pending row (`tests/fixtures/algo/doc_orders_algo_pending_take_profit.json`,
/// verbatim) with its take-profit side moved to the trailing order's words: a resting `Trailing`.
fn doc_trailing_row(instrument: &str, order_client_id: &str) -> Value {
    let envelope = crate::triggered_stop::fixture!("doc_orders_algo_pending_take_profit.json");
    let mut row = envelope["data"][0].clone();
    row["ordType"] = json!("move_order_stop");
    row["callbackSpread"] = json!("150");
    row["activePx"] = json!("70000");
    for key in ["tpTriggerPx", "tpOrdPx", "tpTriggerPxType"] {
        row[key] = json!("");
    }
    row["instId"] = json!(instrument);
    row["instType"] = json!("SWAP");
    row["algoClOrdId"] = json!(order_client_id);
    row
}

#[test]
fn the_documented_pending_row_of_a_trailing_order_reads_as_a_resting_trailing_order() {
    let row = doc_trailing_row(SYMBOL, "tr1");
    let reports = parse_algo_pending(&json!([row]).to_string(), 0.01).expect("parses");
    let [r] = &reports[..] else { panic!("one row, got {reports:?}") };
    assert_eq!(r.order_type, OrderType::Trailing);
    assert_eq!(r.status, OrderStatus::Accepted);
    assert_eq!(r.order_client_id.as_deref(), Some("tr1"));
    assert_eq!(r.venue_order_id.as_str(), "1753184812254216192");
    assert!((r.qty - 0.1).abs() < 1e-12, "10 contracts x 0.01: {}", r.qty);
    assert_eq!(
        conditional_kind(&row),
        (OrderType::Trailing, None),
        "no limit: it fires a market order"
    );
    // the terms: `activePx` the activation, `callbackSpread` the distance in price
    assert_eq!(
        (r.price, r.trigger_price, r.trail, r.trail_percent),
        (None, Some(70000.0), Some(150.0), None)
    );
}

/// A PERCENT trailing order's row (`callbackRatio`, an empty `callbackSpread`) reads back as a price
/// one's does: a resting `Trailing` with no limit.
#[test]
fn a_percent_trailing_orders_pending_row_reads_as_a_resting_trailing_order() {
    let mut row = doc_trailing_row(SYMBOL, "tr1");
    row["callbackSpread"] = json!("");
    row["callbackRatio"] = json!("0.015");
    let reports = parse_algo_pending(&json!([row]).to_string(), 0.01).expect("parses");
    let [r] = &reports[..] else { panic!("one row, got {reports:?}") };
    assert_eq!(r.order_type, OrderType::Trailing);
    assert_eq!(r.status, OrderStatus::Accepted);
    assert_eq!(conditional_kind(&row), (OrderType::Trailing, None));
    // `callbackRatio` 0.015 is exactly 1.5 percent; the empty spread states no price distance
    assert_eq!(
        (r.price, r.trigger_price, r.trail, r.trail_percent),
        (None, Some(70000.0), None, Some(1.5))
    );
    row["callbackRatio"] = json!("0.07");
    let r = parse_algo_pending(&json!([row]).to_string(), 0.01).expect("parses").remove(0);
    assert_eq!(
        r.trail_percent,
        Some(7.0),
        "no float noise from the shift (0.07 x 100 is 7.000000000000001)"
    );
}

/// `(path tail, params)` of every request the scripted venue was asked, in order.
type Calls = Arc<Mutex<Vec<(String, Value)>>>;

type Answer = Box<dyn Fn(&str, &Value) -> Result<Value, VenueApiError> + Send + Sync>;

/// Answers by `(path tail, params)` and records every request.
struct Venue {
    answer: Answer,
    calls: Calls,
}

impl RestTransportJson for Venue {
    fn signed(
        &self,
        _base: &str,
        path: &str,
        _method: &str,
        params: &[(&str, Value)],
        _signer: &dyn SignerJson,
    ) -> Result<Value, VenueApiError> {
        let tail = path.rsplit('/').next().unwrap_or(path).to_string();
        let map: serde_json::Map<String, Value> =
            params.iter().map(|(k, v)| (k.to_string(), v.clone())).collect();
        let params = Value::Object(map);
        let answer = (self.answer)(&tail, &params);
        self.calls.lock().unwrap().push((tail, params));
        answer
    }
    vike_bridge_core::no_public_call!("the recon client must use the SIGNED lane, not public");
}

/// A recon client over a scripted venue whose okx row is taken as LISTING `trailing` (the open side of
/// the gate); [`reconcile_default`] follows the row as the code ships it.
fn recon(
    answer: impl Fn(&str, &Value) -> Result<Value, VenueApiError> + Send + Sync + 'static,
) -> (ReconcileClientOkx<Venue>, Calls) {
    let (client, calls) = reconcile_default(answer);
    (client.with_trailing(true), calls)
}

fn reconcile_default(
    answer: impl Fn(&str, &Value) -> Result<Value, VenueApiError> + Send + Sync + 'static,
) -> (ReconcileClientOkx<Venue>, Calls) {
    let calls: Calls = Arc::default();
    let creds = Credentials {
        api_key: "k".to_string().into(),
        api_secret: "s".to_string().into(),
        passphrase: Some("p".to_string().into()),
    };
    let client = ReconcileClientOkx::new(
        OkxV5Signer::new(&creds, || 0),
        Venue { answer: Box::new(answer), calls: Arc::clone(&calls) },
        "https://unused.invalid",
        SYMBOL,
        0.01,
    );
    (client, calls)
}

/// Every read a pass makes answered empty (the fills with the recorded fills, so the naming reads run).
fn quiet(tail: &str, _params: &Value) -> Result<Value, VenueApiError> {
    Ok(match tail {
        "fills" => crate::triggered_stop::fixture!("rest_fills.json"),
        _ => envelope(Vec::new()),
    })
}

/// The gate both ways. CLOSED (okx's row lists no `trailing`): an order pass and a fill pass ask exactly
/// what they asked before the trailing order existed, `ordType=conditional` alone. OPEN: each adds its one
/// account-wide `move_order_stop` read after the per-instrument ones. A client built plain follows the
/// row as it ships (`vike_okx::execution::rest::trailing_listed`).
#[test]
fn the_trailing_reads_are_made_only_while_okxs_row_lists_trailing() {
    let pass = |c: &ReconcileClientOkx<Venue>| {
        c.fetch_order_status_reports(0).expect("orders");
        c.fetch_fill_reports(0).expect("fills");
    };
    let paths = |calls: &Calls| -> Vec<String> {
        calls.lock().unwrap().iter().map(|(t, _)| t.clone()).collect()
    };

    let (closed, calls) = reconcile_default(quiet);
    let closed = closed.with_trailing(false);
    pass(&closed);
    assert_eq!(
        paths(&calls),
        ["orders-pending", "orders-algo-pending", "fills", "orders-algo-history"]
    );
    assert_eq!(
        asked(&calls, "orders-algo-pending"),
        [json!({"ordType": "conditional", "instId": SYMBOL, "limit": "100"})]
    );
    assert_eq!(
        asked(&calls, "orders-algo-history"),
        [json!({"ordType": "conditional", "state": "effective", "instId": SYMBOL, "limit": "100"})]
    );

    let (open, calls) = recon(quiet);
    pass(&open);
    assert_eq!(
        paths(&calls),
        [
            "orders-pending",
            "orders-algo-pending",
            "orders-algo-pending",
            "fills",
            "orders-algo-history",
            "orders-algo-history"
        ]
    );
    assert_eq!(
        asked(&calls, "orders-algo-pending")[1],
        json!({"ordType": "move_order_stop", "limit": "100"})
    );
    assert_eq!(
        asked(&calls, "orders-algo-history")[1],
        json!({"ordType": "move_order_stop", "state": "effective", "limit": "100"})
    );

    let (shipped, calls) = reconcile_default(quiet);
    pass(&shipped);
    let trailing_asked =
        asked(&calls, "orders-algo-pending").iter().any(|p| p["ordType"] == "move_order_stop");
    assert_eq!(
        trailing_asked,
        vike_okx::execution::rest::trailing_listed(),
        "the default is the row's word"
    );
}

fn envelope(rows: Vec<Value>) -> Value {
    json!({"code": "0", "msg": "", "data": rows})
}

fn asked(calls: &Calls, tail: &str) -> Vec<Value> {
    calls.lock().unwrap().iter().filter(|(t, _)| t == tail).map(|(_, p)| p.clone()).collect()
}

/// The resting trailing orders are ONE account-wide read (`ordType=move_order_stop`, no `instId`), after
/// the per-instrument reads: a row of the mounted instrument is a live `Trailing` scaled by its `ctVal`;
/// a row of an instrument this client does not read is not reported; a row that is not a trailing order
/// (a venue that ignored the filter) is left to the conditional read, never reported twice.
#[test]
fn the_resting_trailing_orders_are_one_account_wide_read() {
    let (c, calls) = recon(|tail, params| match (tail, params["ordType"].as_str()) {
        ("orders-pending", _) => Ok(envelope(Vec::new())),
        ("orders-algo-pending", Some("conditional")) => Ok(envelope(Vec::new())),
        ("orders-algo-pending", Some("move_order_stop")) => {
            let mut eth = doc_trailing_row("ETH-USDT-SWAP", "tr2");
            eth["algoId"] = json!("2");
            let mut stop = doc_trailing_row(SYMBOL, "sl9");
            stop["algoId"] = json!("3");
            stop["ordType"] = json!("conditional");
            Ok(envelope(vec![doc_trailing_row(SYMBOL, "tr1"), eth, stop]))
        }
        other => panic!("an order report reads orders, stops and trailing orders only: {other:?}"),
    });
    let reports = c.fetch_order_status_reports(0).expect("report");
    let [r] = &reports[..] else {
        panic!("the one trailing order of the mounted instrument: {reports:?}")
    };
    assert_eq!(r.order_client_id.as_deref(), Some("tr1"));
    assert_eq!(r.order_type, OrderType::Trailing);
    assert!(r.status.is_live());
    assert!((r.qty - 0.1).abs() < 1e-12, "10 contracts x ctVal 0.01: {}", r.qty);
    assert_eq!(
        asked(&calls, "orders-algo-pending"),
        [
            json!({"ordType": "conditional", "instId": SYMBOL, "limit": "100"}),
            json!({"ordType": "move_order_stop", "limit": "100"}),
        ]
    );
}

/// NOT strict while unmeasured: a refused trailing read (the request shape the demo has not seen) is
/// logged and leaves out the trailing rows ALONE: the report still carries the orders and the stops, and
/// the fill read still names a fired stop's fill. A refused trailing history leaves the fills as read.
#[test]
fn a_refused_trailing_read_leaves_out_the_trailing_rows_alone() {
    let refused_shape =
        || Ok(json!({"code": "51000", "msg": "Parameter ordType error", "data": []}));
    let (c, _) = recon(move |tail, params| match (tail, params["ordType"].as_str()) {
        ("orders-pending", _) => Ok(envelope(vec![json!({
            "instId": SYMBOL, "ordId": "o-1", "clOrdId": "lim1", "side": "buy", "ordType": "limit",
            "sz": "1", "px": "50000", "accFillSz": "0", "avgPx": "", "state": "live", "uTime": "1"
        })])),
        ("orders-algo-pending", Some("conditional")) => {
            let mut stop = doc_trailing_row(SYMBOL, "sl1");
            stop["ordType"] = json!("conditional");
            stop["slTriggerPx"] = json!("50000");
            stop["slOrdPx"] = json!("-1");
            Ok(envelope(vec![stop]))
        }
        ("fills", _) => Ok(crate::triggered_stop::fixture!("rest_fills.json")),
        ("orders-algo-history", Some("conditional")) => {
            Ok(crate::triggered_stop::fixture!("rest_orders_algo_history_effective.json"))
        }
        (_, Some("move_order_stop")) => refused_shape(),
        other => panic!("unexpected {other:?}"),
    });
    let reports = c.fetch_order_status_reports(0).expect("the leg is not blinded");
    let order_client_ids: Vec<_> = reports.iter().map(|r| r.order_client_id.as_deref()).collect();
    assert_eq!(
        order_client_ids,
        [Some("lim1"), Some("sl1")],
        "the order and the stop, no trailing row"
    );
    let fills = c.fetch_fill_reports(0).expect("the fill read is not failed either");
    let child = fills.iter().find(|f| f.venue_order_id.as_str() == CHILD_ORDID).expect("child");
    assert_eq!(child.order_client_id.as_deref(), Some(crate::triggered_stop::STOP), "still named");
}

/// A trailing order that FIRED while the node was down left a child whose fill carries the venue's own
/// `clOrdId` and no algo field: ONE account-wide `orders-algo-history` read (`move_order_stop`,
/// `effective`) names it by the trailing order's order client id, beside the stops' per-instrument read.
#[test]
fn a_fired_trailing_orders_child_fill_is_named_by_its_order_client_id() {
    let (c, calls) = recon(|tail, params| match (tail, params["ordType"].as_str()) {
        ("fills", _) => Ok(crate::triggered_stop::fixture!("rest_fills.json")),
        ("orders-algo-history", Some("conditional")) => Ok(envelope(Vec::new())),
        ("orders-algo-history", Some("move_order_stop")) => Ok(envelope(vec![json!({
            "instId": SYMBOL, "algoClOrdId": "tr1", "ordType": "move_order_stop",
            "state": "effective", "ordIdList": [CHILD_ORDID], "ordId": CHILD_ORDID
        })])),
        other => panic!("a fill read asks fills and the two histories only: {other:?}"),
    });
    let fills = c.fetch_fill_reports(0).expect("fills");
    let child = fills.iter().find(|f| f.venue_order_id.as_str() == CHILD_ORDID).expect("child");
    assert_eq!(child.order_client_id.as_deref(), Some("tr1"), "named by the trailing order");
    assert_eq!(
        asked(&calls, "orders-algo-history"),
        [
            json!({"ordType": "conditional", "state": "effective", "instId": SYMBOL, "limit": "100"}),
            json!({"ordType": "move_order_stop", "state": "effective", "limit": "100"}),
        ]
    );
}

// ---- the business socket: `algo-advance` ------------------------------------------------------------

/// The RECORDED `orders-algo` push of a fired stop, as the `algo-advance` push of a trailing order (the
/// WS table gives both channels the same row: `algoClOrdId`, `state`, `ordType`), in `state`.
fn advance_frame(state: &str, ord_type: &str) -> Value {
    let mut frame = crate::triggered_stop::fixture!("ws_orders_algo_effective.json");
    frame["arg"]["channel"] = json!("algo-advance");
    frame["data"][0]["ordType"] = json!(ord_type);
    frame["data"][0]["state"] = json!(state);
    frame
}

/// A trailing order's own state on `algo-advance` maps as a stop's does on `orders-algo` (`effective` an
/// `OrderTriggered`, `canceled` and `order_failed` an `OrderCanceled`, a resting word nothing); an iceberg
/// or a TWAP row on that channel is never ours and maps to nothing.
#[test]
fn a_trailing_orders_state_on_algo_advance_reaches_the_order() {
    let order_client_id = "b7okxtsb";
    let kinds = |frame: &Value| -> Vec<String> {
        map_okx_algo_frame(frame)
            .iter()
            .map(|e| match e {
                Event::OrderTriggered(t) => format!("Triggered:{}", t.order_client_id),
                Event::OrderCanceled(c) => format!("Canceled:{}", c.order_client_id),
                other => panic!("unexpected {other:?}"),
            })
            .collect()
    };
    assert_eq!(
        kinds(&advance_frame("effective", "move_order_stop")),
        [format!("Triggered:{order_client_id}")]
    );
    assert_eq!(
        kinds(&advance_frame("canceled", "move_order_stop")),
        [format!("Canceled:{order_client_id}")]
    );
    assert_eq!(
        kinds(&advance_frame("order_failed", "move_order_stop")),
        [format!("Canceled:{order_client_id}")]
    );
    assert!(kinds(&advance_frame("pause", "move_order_stop")).is_empty());
    for other in ["twap", "smart_iceberg", "conditional"] {
        assert!(
            kinds(&advance_frame("canceled", other)).is_empty(),
            "{other} is not a trailing row"
        );
    }
}

/// A scripted business socket on loopback: acks the login, answers each subscribe (an `error` event for
/// a channel in `refuse`), and returns every subscribed `(channel, instType)` once `orders-algo` is acked.
fn scripted_business_socket(
    refuse: &'static [&'static str],
) -> (String, std::thread::JoinHandle<Vec<(String, String)>>) {
    let listener = TcpListener::bind("127.0.0.1:0").expect("a loopback port");
    let url = format!("ws://{}", listener.local_addr().expect("its address"));
    let server = std::thread::spawn(move || {
        let (stream, _) = listener.accept().expect("the client dials");
        let mut ws = tungstenite::accept(stream).expect("the upgrade");
        let mut subscribed = Vec::new();
        loop {
            let Ok(msg) = ws.read() else { return subscribed };
            let Ok(text) = msg.to_text() else { continue };
            let Ok(frame) = serde_json::from_str::<Value>(text) else { continue };
            let reply = match frame["op"].as_str() {
                Some("login") => json!({"event": "login", "code": "0", "msg": ""}),
                Some("subscribe") => {
                    let arg = frame["args"][0].clone();
                    let channel = arg["channel"].as_str().unwrap_or("").to_string();
                    subscribed.push((
                        channel.clone(),
                        arg["instType"].as_str().unwrap_or("").to_string(),
                    ));
                    if refuse.contains(&channel.as_str()) {
                        json!({"event": "error", "code": "60018", "msg": "Wrong URL or channel"})
                    } else {
                        json!({"event": "subscribe", "arg": arg, "connId": "c1"})
                    }
                }
                _ => continue,
            };
            let done = subscribed.last().is_some_and(|(c, _)| c == "orders-algo");
            ws.send(Message::Text(reply.to_string().into())).expect("the reply goes out");
            if done {
                return subscribed;
            }
        }
    });
    (url, server)
}

/// The business socket's subscriptions both ways of the gate. CLOSED: `orders-algo` alone, exactly the
/// socket of before. OPEN: `algo-advance` FIRST, then `orders-algo`, both `instType=ANY`. OPEN and the venue
/// refuses `algo-advance`: the socket still opens with `orders-algo` (the stops' channel never depends on
/// the new one).
#[test]
fn the_business_socket_subscribes_algo_advance_only_while_okxs_row_lists_trailing() {
    const NONE: &[&str] = &[];
    const ADVANCE: &[&str] = &["algo-advance"];
    let any = |c: &str| (c.to_string(), "ANY".to_string());
    for (trailing, refuse, want) in [
        (false, NONE, vec![any("orders-algo")]),
        (true, NONE, vec![any("algo-advance"), any("orders-algo")]),
        (true, ADVANCE, vec![any("algo-advance"), any("orders-algo")]),
    ] {
        let (url, server) = scripted_business_socket(refuse);
        let outcome = open_okx_algo_websocket(
            &url,
            "key",
            "secret",
            "passphrase",
            || 0,
            trailing,
            &AtomicBool::new(false),
            &websocket_rate_gate(),
        );
        assert!(
            matches!(outcome, WebsocketSessionUserOpenOutcome::Ready(_)),
            "trailing {trailing}, refuse {refuse:?}: the socket opens"
        );
        drop(outcome);
        assert_eq!(
            server.join().expect("the script ends"),
            want,
            "trailing {trailing}, refuse {refuse:?}"
        );
    }
}
