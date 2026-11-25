//! A STOP-LIMIT on okx is ONE `ordType=conditional` algo order, the stop's own, whose `slOrdPx` is the
//! limit price instead of the stop's `-1` (the official "Place algo order" table:
//! <https://www.okx.com/docs-v5/en/#order-book-trading-algo-trading-post-place-algo-order>). Pinned here
//! with a transport that records every call:
//!
//! - the submit: the stop-limit body is the stop's byte for byte but `slOrdPx`, and the stop keeps `-1`;
//! - the refusals before the wire: no limit, no trigger, a non-finite price, a time in force other than
//!   GTC; and every kind the ORDER endpoint has no word for (a take-profit, an unknown word, a stop
//!   through the batch) is refused by name instead of going out as a market order;
//! - the order report: a resting algo row with a priced `slOrdPx` is a `StopLimit`, `-1` a `Stop`;
//! - the cancel of a FIRED stop-limit: its trigger placed a regular limit order under the venue's own
//!   `clOrdId`, which is cancelled by `ordId`; a fired STOP's cancel makes exactly the calls it made.

use std::sync::Mutex;

use serde_json::{Value, json};
use vike_bridge_core::rest::{OrderCancelAck, VenueRest};
use vike_bridge_core::signer::SignerJson;
use vike_bridge_core::transport::VenueApiError;
use vike_bridge_core::transport::json::RestTransportJson;
use vike_model::events::Event;
use vike_model::{OrderRequest, OrderType, TimeInForce};
use vike_okx::execution::rest::VenueRestOkx;
use vike_okx::reconcile::parse_algo_pending;

use crate::r6_okx_parity::{fixture, okx_perp};

type Reply = Box<dyn Fn(&str, &Value) -> Result<Value, VenueApiError> + Send>;

/// Answers by `(last path segment, the call's params or body)` and records `(METHOD tail, params)`.
/// Shared with `take_profit.rs` and `algo_amend.rs`.
pub(crate) struct Wire {
    reply: Reply,
    calls: Mutex<Vec<(String, Value)>>,
}

impl Wire {
    pub(crate) fn new(
        reply: impl Fn(&str, &Value) -> Result<Value, VenueApiError> + Send + 'static,
    ) -> Self {
        Self { reply: Box::new(reply), calls: Mutex::default() }
    }
    /// A transport no test may reach: every refusal below happens before the wire.
    pub(crate) fn none() -> Self {
        Self::new(|tail, params| panic!("nothing may be sent, got {tail} {params}"))
    }
    fn record(&self, method: &str, path: &str, body: Value) -> Result<Value, VenueApiError> {
        let tail = path.rsplit('/').next().unwrap_or(path).to_string();
        let answer = (self.reply)(&tail, &body);
        self.calls.lock().unwrap().push((format!("{method} {tail}"), body));
        answer
    }
    pub(crate) fn calls(&self) -> Vec<String> {
        self.calls.lock().unwrap().iter().map(|(c, _)| c.clone()).collect()
    }
    pub(crate) fn params(&self, n: usize) -> Value {
        self.calls.lock().unwrap()[n].1.clone()
    }
}

impl RestTransportJson for Wire {
    fn signed(
        &self,
        _base: &str,
        path: &str,
        method: &str,
        params: &[(&str, Value)],
        _signer: &dyn SignerJson,
    ) -> Result<Value, VenueApiError> {
        let obj: serde_json::Map<String, Value> =
            params.iter().map(|(k, v)| (k.to_string(), v.clone())).collect();
        self.record(method, path, Value::Object(obj))
    }
    fn signed_json(
        &self,
        _base: &str,
        path: &str,
        method: &str,
        body: &str,
        _signer: &dyn SignerJson,
    ) -> Result<Value, VenueApiError> {
        self.record(method, path, serde_json::from_str(body).expect("a JSON body"))
    }
    vike_bridge_core::no_public_call!();
}

pub(crate) fn rest(wire: Wire) -> VenueRestOkx<Wire> {
    okx_perp(&fixture(), wire)
}

pub(crate) fn accepted() -> Value {
    json!({"code": "0", "msg": "", "data": [{"algoId": "algo-1", "algoClOrdId": "sl1", "sCode": "0", "sMsg": ""}]})
}

pub(crate) fn refused(code: &str) -> Value {
    json!({"code": "1", "msg": "All operations failed", "data": [{"sCode": code, "sMsg": "venue words"}]})
}

pub(crate) fn done() -> Value {
    json!({"code": "0", "msg": "", "data": [{"sCode": "0", "sMsg": ""}]})
}

/// A sell stop-limit on the fixture's BTC-USDT-SWAP (tick 0.1, ctVal 0.01): 0.01 BTC = one contract.
fn stop_limit(price: Option<f64>, trigger: Option<f64>) -> OrderRequest {
    OrderRequest {
        order_client_id: "sl1".into(),
        venue: "okx".into(),
        symbol: "BTC-USDT-SWAP".into(),
        side: -1,
        qty: 0.01,
        order_type: OrderType::StopLimit,
        price,
        trigger_price: trigger,
        reduce_only: true,
        ..OrderRequest::default()
    }
}

fn submitted_body(request: &OrderRequest) -> (Vec<Event>, Value) {
    let c = rest(Wire::new(|tail, _| match tail {
        "order-algo" => Ok(accepted()),
        other => panic!("only order-algo may be called, got {other}"),
    }));
    let events = VenueRest::submit_order(&c, request);
    assert_eq!(c.transport.calls(), ["POST order-algo"], "one algo POST and nothing else");
    (events, c.transport.params(0))
}

// ---- submit ---------------------------------------------------------------------------------------

#[test]
fn a_stop_limit_is_the_stops_conditional_algo_order_with_its_limit_in_sl_ord_px() {
    let (events, limit_body) = submitted_body(&stop_limit(Some(57_900.54), Some(58_000.0)));
    match &events[..] {
        [Event::OrderSubmitted(_), Event::OrderAccepted(a)] => {
            assert_eq!(a.venue_order_id.as_deref(), Some("algo-1"), "the venue id is the algoId");
        }
        other => panic!("Submitted + Accepted, got {other:?}"),
    }
    assert_eq!(limit_body["ordType"], "conditional");
    assert_eq!(limit_body["slTriggerPx"], "58000.0");
    assert_eq!(limit_body["slOrdPx"], "57900.5", "the limit, down to the 0.1 tick grid");
    assert_eq!(limit_body["sz"], "1.00", "0.01 BTC = one contract, on the 0.01 lot grid");
    assert_eq!(limit_body["algoClOrdId"], "sl1");

    // The tie: the stop's body is the same but `slOrdPx`, and the stop keeps `-1` byte for byte.
    let stop =
        OrderRequest { order_type: OrderType::Stop, ..stop_limit(Some(57_900.54), Some(58_000.0)) };
    let (_, stop_body) = submitted_body(&stop);
    assert_eq!(stop_body["slOrdPx"], "-1", "a stop still fires a market order");
    let rest_of = |mut body: Value| {
        body.as_object_mut().unwrap().remove("slOrdPx");
        body
    };
    assert_eq!(rest_of(limit_body), rest_of(stop_body), "everything but slOrdPx is the stop's");
}

#[test]
fn a_stop_limit_that_cannot_be_written_as_one_is_refused_before_the_wire() {
    use vike_model::OrderRefusalId::{NotSent, VenueCapability};
    let gtc = |price, trigger| stop_limit(price, trigger);
    let cases = [
        ("no limit", gtc(None, Some(58_000.0)), "limit price", NotSent),
        ("a zero limit", gtc(Some(0.0), Some(58_000.0)), "limit price", NotSent),
        ("a NaN limit", gtc(Some(f64::NAN), Some(58_000.0)), "finite", NotSent),
        ("no trigger", gtc(Some(57_900.0), None), "trigger price", NotSent),
        (
            "IOC",
            OrderRequest { time_in_force: TimeInForce::Ioc, ..gtc(Some(57_900.0), Some(58_000.0)) },
            "time in force",
            VenueCapability,
        ),
        (
            "post-only",
            OrderRequest {
                time_in_force: TimeInForce::PostOnly,
                ..gtc(Some(57_900.0), Some(58_000.0))
            },
            "time in force",
            VenueCapability,
        ),
    ];
    for (what, request, word, family) in cases {
        let c = rest(Wire::none());
        match &VenueRest::submit_order(&c, &request)[..] {
            [Event::OrderSubmitted(_), Event::OrderRejected(r)] => {
                assert!(r.reason.contains(word), "{what}: {}", r.reason);
                assert_eq!(r.refusal_id.as_deref(), Some(family.word()), "{what}");
            }
            other => panic!("{what}: Submitted + a terminal Rejected, got {other:?}"),
        }
    }
}

/// The `_ => market` fallthrough is gone: a kind the order endpoint has no word for is a terminal
/// refusal in words, single or batched, and nothing is signed. (A take-profit has a word since wave 2:
/// the algo endpoint's, `take_profit.rs`.)
#[test]
fn a_kind_the_order_endpoint_has_no_word_for_is_refused_by_name_never_sent_as_market() {
    let kinds = [OrderType::MarketToLimit, OrderType::Unrecognized("trailing_stop".into())];
    for kind in kinds {
        let request =
            OrderRequest { order_type: kind.clone(), ..stop_limit(Some(57_900.0), Some(58_000.0)) };
        let c = rest(Wire::none());
        match &VenueRest::submit_order(&c, &request)[..] {
            [Event::OrderSubmitted(_), Event::OrderRejected(r)] => {
                assert!(r.reason.contains(kind.as_str()), "{kind:?}: {}", r.reason)
            }
            other => panic!("{kind:?}: Submitted + a terminal Rejected, got {other:?}"),
        }
    }
    // A stop has no batch route to the algo endpoint: through the batch it is refused, not a market row.
    let stop = OrderRequest { order_type: OrderType::Stop, ..stop_limit(None, Some(58_000.0)) };
    let c = rest(Wire::none());
    match &c.submit_batch(std::slice::from_ref(&stop))[..] {
        [Event::OrderSubmitted(_), Event::OrderRejected(r)] => {
            assert!(r.reason.contains("stop"), "{}", r.reason)
        }
        other => panic!("Submitted + a terminal Rejected, got {other:?}"),
    }
}

// ---- the order report -----------------------------------------------------------------------------

#[test]
fn a_resting_algo_row_with_a_priced_sl_ord_px_is_a_stop_limit() {
    let kind_of = |sl_ord_px: Option<&str>| {
        let mut row = json!({
            "instId": "BTC-USDT-SWAP", "algoId": "algo-9", "algoClOrdId": "sl1",
            "ordType": "conditional", "side": "sell", "sz": "3", "state": "live",
            "slTriggerPx": "58000", "uTime": "1700000000000"});
        if let Some(px) = sl_ord_px {
            row["slOrdPx"] = json!(px);
        }
        let reports = parse_algo_pending(&json!([row]).to_string(), 0.01).expect("parses");
        reports[0].order_type.clone()
    };
    assert_eq!(kind_of(Some("57900.5")), OrderType::StopLimit);
    assert_eq!(kind_of(Some("-1")), OrderType::Stop, "market on trigger");
    assert_eq!(kind_of(Some("")), OrderType::Stop);
    assert_eq!(kind_of(None), OrderType::Stop, "every row read before the field");

    // the terms: the level from `slTriggerPx`, the limit from a priced `slOrdPx` only
    let terms_of = |sl_ord_px: &str| {
        let row = json!({
            "instId": "BTC-USDT-SWAP", "algoId": "algo-9", "algoClOrdId": "sl1",
            "ordType": "conditional", "side": "sell", "sz": "3", "state": "live",
            "slTriggerPx": "58000", "slOrdPx": sl_ord_px, "uTime": "1700000000000"});
        let r = parse_algo_pending(&json!([row]).to_string(), 0.01).expect("parses").remove(0);
        (r.price, r.trigger_price, r.trail, r.trail_percent)
    };
    assert_eq!(terms_of("57900.5"), (Some(57900.5), Some(58000.0), None, None));
    assert_eq!(terms_of("-1"), (None, Some(58000.0), None, None), "market on trigger");
}

// ---- cancel of a FIRED stop-limit -------------------------------------------------------------------

/// `order-algo`'s row of a fired conditional: `effective`, its child in `ordIdList`.
fn fired(sl_ord_px: &str) -> Value {
    json!({"code": "0", "msg": "", "data": [{
        "algoId": "algo-1", "algoClOrdId": "sl1", "instId": "BTC-USDT-SWAP", "ordType": "conditional",
        "state": "effective", "slTriggerPx": "58000", "slOrdPx": sl_ord_px,
        "ordId": "child-7", "ordIdList": ["child-7"]}]})
}

/// The regular book does not know our order client id (the child's `clOrdId` is the venue's), the algo endpoint
/// no longer cancels the fired stop; `child` is the regular book's answer to the child's own cancel.
fn fired_cancel(sl_ord_px: &'static str, child: Value) -> VenueRestOkx<Wire> {
    rest(Wire::new(move |tail, params| match tail {
        "cancel-order" if params.get("clOrdId").is_some() => Ok(refused("51400")),
        "cancel-order" => Ok(child.clone()),
        "cancel-algos" => Ok(refused("51400")),
        "order-algo" => Ok(fired(sl_ord_px)),
        other => panic!("unexpected {other}"),
    }))
}

#[test]
fn a_fired_stop_limits_resting_order_is_cancelled_by_its_order_id() {
    let c = fired_cancel("57900.5", done());
    assert_eq!(c.cancel_order("sl1").expect("cancelled"), OrderCancelAck::Canceled);
    assert_eq!(
        c.transport.calls(),
        ["POST cancel-order", "POST cancel-algos", "GET order-algo", "POST cancel-order"]
    );
    assert_eq!(
        c.transport.params(3),
        json!({"instId": "BTC-USDT-SWAP", "ordId": "child-7"}),
        "the child, by the venue's own order id"
    );
}

/// The child already left (filled or cancelled): its own row closes the order, so the cancel is the
/// fired stop's refusal it always was, never a booked cancel over a fill.
#[test]
fn a_fired_stop_limit_whose_order_already_left_keeps_the_fired_stops_refusal() {
    let c = fired_cancel("57900.5", refused("51401"));
    let err = c.cancel_order("sl1").expect_err("nothing was cancelled");
    assert_eq!(err.code, 51412, "{err:?}");
    assert!(err.msg.contains("effective"), "{}", err.msg);
}

/// A child cancel the venue refuses for another reason is that error, not "gone".
#[test]
fn a_fired_stop_limits_child_cancel_refused_otherwise_is_an_error() {
    let c = fired_cancel("57900.5", refused("50011"));
    assert_eq!(c.cancel_order("sl1").expect_err("refused").code, 50011);
}

/// PRESERVED: a fired STOP (`slOrdPx -1`) fired a market order that rests nowhere, so its cancel makes
/// exactly the three calls it made before and stays the non-terminal refusal.
#[test]
fn a_fired_stops_cancel_makes_the_calls_it_always_made() {
    let c = fired_cancel("-1", done());
    assert_eq!(c.cancel_order("sl1").expect_err("not cancelled").code, 51412);
    assert_eq!(c.transport.calls(), ["POST cancel-order", "POST cancel-algos", "GET order-algo"]);
}
