//! **What aster's signed REST clients send, and what they make of each answer, pinned before the R7 move**
//! (the owner's ruling of 2026-11-13: both halves of the aster/binance exec fork go into
//! `vike_binance::family`, "behaviour exactly as now").
//!
//! Each test drives `VenueRestAsterSpot` / `VenueRestAsterPerp` (and the two history replays) over a recording
//! transport and pins, byte for byte, every request the client hands it — the door (`signed`, `requery`
//! or `public`: the requery door is the short-timeout one), the method, the host and path, and the
//! parameters in their order, which is everything the client decides before the signer and the
//! transport build the query, the body and the headers — then the events made of the venue's answer,
//! and every log line the client writes (its level, its `tracing` target, its words and its fields).
//!
//! It also pins what aster does NOT do: binance's three bridge floors (`deny_unsupported_time_in_force`,
//! `deny_unsupported_trigger_by` on spot, `deny_unbuilt_kind`) and its `post_only_cause` are off on this
//! venue, so an order they would refuse goes out exactly as aster always sent it. No socket, no
//! credential: the signer prepares nothing.

use std::sync::Mutex;

use serde_json::{Value, json};
use vike_aster::execution::rest::perp::VenueRestAsterPerp;
use vike_aster::execution::rest::spot::VenueRestAsterSpot;
use vike_bridge_core::rest::{OrderCancelAck, VenueRest};
use vike_bridge_core::signer::{PreparedRequest, Signer};
use vike_bridge_core::transport::{RestTransport, VenueApiError};
use vike_bridge_core::websocket_session::user::RESYNC_HISTORY_LIMIT;
use vike_model::events::{Event, OrderAccepted, OrderModified, OrderRejected, OrderSubmitted};
use vike_model::{OrderRequest, OrderType, SymbolProperties, TimeInForce, TriggerBy};

struct NullSigner;
impl Signer for NullSigner {
    fn prepare(&self, _p: &[(&str, String)], _m: &str, _path: &str) -> PreparedRequest {
        PreparedRequest::default()
    }
}

type Answer = Result<Value, VenueApiError>;
/// The scripted answers, `(method, path, answer)`, each used once.
type Script = Vec<(&'static str, &'static str, Answer)>;
/// One log line: level, target, message, fields.
type Line = (String, String, String, String);
/// One request as the client handed it over: the door, the method, `host + path`, the params in order.
type Sent = (&'static str, String, String, Vec<(String, String)>);

/// Answers each request with the FIRST scripted answer for its `(method, path)` (each answer is used
/// once) and records every request. A request the script does not name panics: an unexpected call is a
/// finding.
struct Wire {
    script: Mutex<Script>,
    sent: Mutex<Vec<Sent>>,
}

impl Wire {
    fn new(script: Script) -> Self {
        Wire { script: Mutex::new(script), sent: Mutex::new(Vec::new()) }
    }
    fn take(
        &self,
        door: &'static str,
        base: &str,
        method: &str,
        path: &str,
        params: &[(&str, String)],
    ) -> Answer {
        let pairs = params.iter().map(|(k, v)| ((*k).to_string(), v.clone())).collect();
        self.sent.lock().unwrap().push((door, method.to_string(), format!("{base}{path}"), pairs));
        let mut script = self.script.lock().unwrap();
        let at = script
            .iter()
            .position(|(m, p, _)| *m == method && *p == path)
            .unwrap_or_else(|| panic!("unscripted {door} {method} {base}{path}"));
        script.remove(at).2
    }
    fn sent(&self) -> Vec<Sent> {
        self.sent.lock().unwrap().clone()
    }
}

impl RestTransport for Wire {
    fn signed(
        &self,
        base: &str,
        path: &str,
        method: &str,
        params: &[(&str, String)],
        _signer: &dyn Signer,
    ) -> Answer {
        self.take("signed", base, method, path, params)
    }
    fn signed_requery(
        &self,
        base: &str,
        path: &str,
        method: &str,
        params: &[(&str, String)],
        _signer: &dyn Signer,
    ) -> Answer {
        self.take("requery", base, method, path, params)
    }
    fn public(&self, base: &str, path: &str, params: &[(&str, String)]) -> Answer {
        self.take("public", base, "GET", path, params)
    }
}

fn refusal(code: i64, msg: &str) -> Answer {
    Err(VenueApiError { code, msg: msg.to_string() })
}

fn sent(door: &'static str, method: &str, url: &str, params: &[(&str, &str)]) -> Sent {
    let pairs = params.iter().map(|(k, v)| ((*k).to_string(), (*v).to_string())).collect();
    (door, method.to_string(), url.to_string(), pairs)
}

fn owned(params: Vec<(&'static str, String)>) -> Vec<(String, String)> {
    params.into_iter().map(|(k, v)| (k.to_string(), v)).collect()
}

const SPOT_HOST: &str = "https://sapi.pin";
const PERP_HOST: &str = "https://fapi.pin";

fn grid() -> SymbolProperties {
    SymbolProperties { tick_size: 0.1, step_size: 0.001, ..Default::default() }
}

fn builder() -> Option<(String, String)> {
    Some(("0xbuilder".to_string(), "0".to_string()))
}

fn spot(script: Script) -> VenueRestAsterSpot<NullSigner, Wire> {
    VenueRestAsterSpot {
        signer: NullSigner,
        transport: Wire::new(script),
        base_url: SPOT_HOST.into(),
        symbol: "BTCUSDT".into(),
        properties: grid(),
        base_asset: "BTC".into(),
        builder: builder(),
    }
}

fn perp(script: Script) -> VenueRestAsterPerp<NullSigner, Wire> {
    VenueRestAsterPerp {
        signer: NullSigner,
        transport: Wire::new(script),
        base_url: PERP_HOST.into(),
        symbol: "BTCUSDT".into(),
        properties: grid(),
        leverage: 3.0,
        builder: builder(),
    }
}

/// A limit BUY `order_client_id` of 0.01 at 50000, stamped `ts` 7.
fn limit(order_client_id: &str) -> OrderRequest {
    OrderRequest {
        order_client_id: order_client_id.to_string(),
        venue: "aster".to_string(),
        symbol: "BTCUSDT".to_string(),
        side: 1,
        qty: 0.01,
        order_type: OrderType::Limit,
        price: Some(50_000.0),
        timestamp: 7,
        ..Default::default()
    }
}

fn submitted(order_client_id: &str) -> Event {
    Event::OrderSubmitted(OrderSubmitted {
        order_client_id: order_client_id.to_string(),
        timestamp: 7,
    })
}

fn accepted(order_client_id: &str, id: &str) -> Event {
    Event::OrderAccepted(OrderAccepted {
        order_client_id: order_client_id.to_string(),
        venue_order_id: Some(id.into()),
        timestamp: 7,
    })
}

/// The venue's refusal of `order_client_id`.
fn rejected(order_client_id: &str, reason: &str) -> Event {
    unlabelled(order_client_id, reason, Some(vike_model::OrderRefusalId::VenueRefused.word()))
}

/// A rejection carrying the catalog id `refusal_id`.
fn unlabelled(order_client_id: &str, reason: &str, refusal_id: Option<&'static str>) -> Event {
    Event::OrderRejected(OrderRejected {
        cause: None,
        order_client_id: order_client_id.to_string(),
        reason: reason.into(),
        timestamp: 7,
        refusal_id: refusal_id.and_then(vike_model::events::refusal_id_of),
    })
}

/// `(level, target, message, fields)` of every line a `vike_aster` target wrote.
fn aster_lines(logs: &[vike_log::capture::CapturedEvent]) -> Vec<Line> {
    logs.iter()
        .filter(|e| e.target.starts_with("vike_aster"))
        .map(|e| {
            (e.level.to_string(), e.target.clone(), e.message.clone(), format!("{:?}", e.fields))
        })
        .collect()
}

fn line(level: &str, target: &str, message: &str, fields: &[(&str, &str)]) -> Line {
    let map: std::collections::BTreeMap<String, String> =
        fields.iter().map(|(k, v)| ((*k).to_string(), (*v).to_string())).collect();
    (level.to_string(), target.to_string(), message.to_string(), format!("{map:?}"))
}

// ---- spot ---------------------------------------------------------------------------------------------

/// A limit goes out ONCE as `POST /api/v3/order` with the builder's params (the builder pair last), and
/// the ack's `orderId` is its venue id.
#[test]
fn spot_limit_is_one_post_of_its_params_and_the_ack_accepts_it() {
    let client = spot(vec![("POST", "/api/v3/order", Ok(json!({"orderId": 11})))]);
    let order = limit("c1");
    let params = client.build_order_params(&order);
    assert_eq!(
        params[params.len() - 2..],
        [("builder", "0xbuilder".to_string()), ("feeRate", "0".to_string())]
    );
    let events = client.submit_order(&order);
    assert_eq!(events, vec![submitted("c1"), accepted("c1", "11")]);
    assert_eq!(
        client.transport.sent(),
        vec![("signed", "POST".to_string(), format!("{SPOT_HOST}/api/v3/order"), owned(params))]
    );
}

/// **Binance's floors are OFF on aster spot**: a GTD limit, a market-to-limit, a stop triggered by the
/// mark price and a post-only stop (each of which binance's spot refuses before the wire) all go out as
/// aster always sent them, one POST each.
#[test]
fn spot_sends_what_binances_floors_would_refuse() {
    let gtd = OrderRequest { time_in_force: TimeInForce::Gtd, gtd_expiry: Some(1), ..limit("c2") };
    let mtl = OrderRequest { order_type: OrderType::MarketToLimit, price: None, ..limit("c3") };
    let mark = OrderRequest {
        order_type: OrderType::Stop,
        price: None,
        trigger_price: Some(48_000.0),
        trigger_by: Some(TriggerBy::Mark),
        ..limit("c4")
    };
    let post_only_stop = OrderRequest { time_in_force: TimeInForce::PostOnly, ..mark.clone() };
    let post_only_stop = OrderRequest { order_client_id: "c5".into(), ..post_only_stop };
    for (order, id) in [(gtd, "21"), (mtl, "22"), (mark, "23"), (post_only_stop, "24")] {
        let client = spot(vec![("POST", "/api/v3/order", Ok(json!({"orderId": id})))]);
        let params = client.build_order_params(&order);
        let order_client_id = order.order_client_id.clone();
        assert_eq!(
            client.submit_order(&order),
            vec![submitted(&order_client_id), accepted(&order_client_id, id)]
        );
        assert_eq!(
            client.transport.sent(),
            vec![(
                "signed",
                "POST".to_string(),
                format!("{SPOT_HOST}/api/v3/order"),
                owned(params)
            )],
            "{order_client_id}"
        );
    }
}

/// A definite refusal is the terminal reject in the venue's words with NO cause (binance's
/// `post_only_cause` is not aster's: a post-only limit that would cross is refused like any other), and
/// one WARN under `vike_aster::execution::rest::spot`.
#[test]
fn spot_refusal_is_the_venues_words_with_no_cause_and_one_warn() {
    let client = spot(vec![(
        "POST",
        "/api/v3/order",
        refusal(-2010, "Order would immediately match and take."),
    )]);
    let order = OrderRequest { time_in_force: TimeInForce::PostOnly, ..limit("c6") };
    let (events, logs) = vike_log::capture::captured(|| client.submit_order(&order));
    assert_eq!(
        events,
        vec![submitted("c6"), rejected("c6", "Order would immediately match and take.")]
    );
    assert_eq!(
        aster_lines(&logs),
        vec![line(
            "WARN",
            "vike_aster::execution::rest::spot",
            "submit rejected",
            &[
                ("code", "-2010"),
                ("msg", "Order would immediately match and take."),
                ("order_client_id", "c6")
            ]
        )]
    );
}

/// A 2xx naming no order, and a status-unknown code, both ask the venue by the BARE order client id on the requery
/// door (aster stamps no prefix); the found order is accepted under its id.
#[test]
fn spot_ambiguous_submit_asks_by_the_bare_order_client_id() {
    for post in [Ok(json!({})), refusal(-1007, "Timeout waiting for response")] {
        let client = spot(vec![
            ("POST", "/api/v3/order", post),
            ("GET", "/api/v3/order", Ok(json!({"orderId": 31, "status": "NEW"}))),
        ]);
        let order = limit("c7");
        let params = client.build_order_params(&order);
        assert_eq!(client.submit_order(&order), vec![submitted("c7"), accepted("c7", "31")]);
        assert_eq!(
            client.transport.sent(),
            vec![
                ("signed", "POST".to_string(), format!("{SPOT_HOST}/api/v3/order"), owned(params)),
                sent(
                    "requery",
                    "GET",
                    &format!("{SPOT_HOST}/api/v3/order"),
                    &[("symbol", "BTCUSDT"), ("origClientOrderId", "c7")]
                ),
            ]
        );
    }
}

/// A cancel is one `DELETE /api/v3/order` by the bare order client id: done, "already gone" on the venue's
/// unknown-order words, and any other answer the error.
#[test]
fn spot_cancel_is_one_delete_by_the_bare_order_client_id() {
    let delete = |answer: Answer| {
        let client = spot(vec![("DELETE", "/api/v3/order", answer)]);
        let got = client.cancel_order("c8").map_err(|e| e.code);
        assert_eq!(
            client.transport.sent(),
            vec![sent(
                "signed",
                "DELETE",
                &format!("{SPOT_HOST}/api/v3/order"),
                &[("symbol", "BTCUSDT"), ("origClientOrderId", "c8")]
            )]
        );
        got
    };
    assert_eq!(delete(Ok(json!({"orderId": 1}))), Ok(OrderCancelAck::Canceled));
    assert_eq!(delete(refusal(-2011, "Unknown order sent.")), Ok(OrderCancelAck::AlreadyGone));
    assert_eq!(delete(refusal(-1003, "Too many requests.")), Err(-1003));
}

/// The active confirm is one requery `GET /api/v3/order` by the bare order client id: a live order is accepted, a
/// closed one carries its terminal, "no such order" is the absent reject, a failed query says nothing.
#[test]
fn spot_confirm_is_one_requery_by_the_bare_order_client_id() {
    let confirm = |answer: Answer| {
        let client = spot(vec![("GET", "/api/v3/order", answer)]);
        let events = client.confirm_order("c9", 7);
        assert_eq!(
            client.transport.sent(),
            vec![sent(
                "requery",
                "GET",
                &format!("{SPOT_HOST}/api/v3/order"),
                &[("symbol", "BTCUSDT"), ("origClientOrderId", "c9")]
            )]
        );
        events
    };
    let tags = |events: Vec<Event>| vike_bridge_core::execution_drive::event_tags(&events);
    assert_eq!(confirm(Ok(json!({"orderId": 41, "status": "NEW"}))), vec![accepted("c9", "41")]);
    let closed = confirm(Ok(json!({"orderId": 42, "status": "CANCELED"})));
    assert_eq!(closed[0], accepted("c9", "42"));
    assert_eq!(tags(closed)[1], "OrderCanceled:c9");
    assert_eq!(tags(confirm(refusal(-2013, "Order does not exist."))), vec!["OrderRejected:c9"]);
    assert!(confirm(refusal(-1003, "Too many requests.")).is_empty());
}

/// Spot has no amend on aster: `VenueRest`'s default, no event and nothing sent.
#[test]
fn spot_modify_sends_nothing() {
    let client = spot(Vec::new());
    let events = client.modify_order(&limit("c10"), Some(0.02), None);
    assert!(events.is_empty(), "{events:?}");
    assert!(client.transport.sent().is_empty());
}

/// The resync's spot replay: the mounted symbol's `allOrders` then `userTrades` (aster's spot path, NOT
/// binance's `myTrades`), each on the requery door at the resync limit, then the same two for a foreign
/// symbol of the lane; a perp label in the foreign list is not read. Fills map onto the mounted symbol
/// bare and onto the foreign one symbol-qualified.
#[test]
fn spot_history_reads_the_mounted_symbol_then_each_foreign_one() {
    let limit_word = RESYNC_HISTORY_LIMIT.to_string();
    let order = |order_client_id: &str| json!([{"orderId": 9, "clientOrderId": order_client_id, "status": "FILLED", "origQty": "1.0", "updateTime": 7}]);
    let trade = json!([{"id": 100, "orderId": 9, "price": "50000", "qty": "1.0", "commission": "0.1", "commissionAsset": "USDT", "time": 5, "side": "BUY", "maker": false}]);
    let client = spot(vec![
        ("GET", "/api/v3/allOrders", Ok(order("cm"))),
        ("GET", "/api/v3/userTrades", Ok(trade.clone())),
        ("GET", "/api/v3/allOrders", Ok(order("cf"))),
        ("GET", "/api/v3/userTrades", Ok(trade)),
    ]);
    let events = vike_aster::execution::spot_history_events_of(
        &client,
        "BTCUSDT",
        &["ETHUSDT".to_string(), "SOLUSDT.P".to_string()],
    );
    let read = |path: &str, symbol: &str| {
        sent(
            "requery",
            "GET",
            &format!("{SPOT_HOST}{path}"),
            &[("symbol", symbol), ("limit", &limit_word)],
        )
    };
    assert_eq!(
        client.transport.sent(),
        vec![
            read("/api/v3/allOrders", "BTCUSDT"),
            read("/api/v3/userTrades", "BTCUSDT"),
            read("/api/v3/allOrders", "ETHUSDT"),
            read("/api/v3/userTrades", "ETHUSDT"),
        ]
    );
    assert_eq!(
        vike_bridge_core::execution_drive::event_tags(&events),
        vec!["Fill:cm:100", "OrderFilled:cm", "Fill:cf:ETHUSDT:100", "OrderFilled:cf"]
    );
}

// ---- perp ---------------------------------------------------------------------------------------------

/// A perp limit is one `POST /fapi/v3/order` of the builder's params (the builder pair last).
#[test]
fn perp_limit_is_one_post_of_its_params_and_the_ack_accepts_it() {
    let client = perp(vec![("POST", "/fapi/v3/order", Ok(json!({"orderId": 51})))]);
    let order = limit("p1");
    let params = client.build_order_params(&order);
    assert_eq!(
        params[params.len() - 2..],
        [("builder", "0xbuilder".to_string()), ("feeRate", "0".to_string())]
    );
    assert_eq!(client.submit_order(&order), vec![submitted("p1"), accepted("p1", "51")]);
    assert_eq!(
        client.transport.sent(),
        vec![("signed", "POST".to_string(), format!("{PERP_HOST}/fapi/v3/order"), owned(params))]
    );
}

/// The ONE floor aster's perp has: a trigger source it cannot express (`Index`) is refused in the shared
/// words with nothing sent. The others are off: a GTD limit and a market-to-limit go out.
#[test]
fn perp_refuses_an_index_trigger_and_sends_what_binances_other_floors_would_refuse() {
    let index = OrderRequest {
        order_type: OrderType::Stop,
        price: None,
        trigger_price: Some(48_000.0),
        trigger_by: Some(TriggerBy::Index),
        ..limit("p2")
    };
    let client = perp(Vec::new());
    assert_eq!(
        client.submit_order(&index),
        vec![
            submitted("p2"),
            unlabelled(
                "p2",
                "trigger_by Index is not supported on aster — order refused, never silently coerced \
                 to a different trigger price series",
                Some(vike_model::OrderRefusalId::VenueCapability.word())
            )
        ]
    );
    assert!(client.transport.sent().is_empty());
    let gtd = OrderRequest { time_in_force: TimeInForce::Gtd, gtd_expiry: Some(1), ..limit("p3") };
    let mtl = OrderRequest { order_type: OrderType::MarketToLimit, price: None, ..limit("p4") };
    for (order, id) in [(gtd, "61"), (mtl, "62")] {
        let client = perp(vec![("POST", "/fapi/v3/order", Ok(json!({"orderId": id})))]);
        let order_client_id = order.order_client_id.clone();
        let params = client.build_order_params(&order);
        assert_eq!(
            client.submit_order(&order),
            vec![submitted(&order_client_id), accepted(&order_client_id, id)]
        );
        assert_eq!(
            client.transport.sent(),
            vec![(
                "signed",
                "POST".to_string(),
                format!("{PERP_HOST}/fapi/v3/order"),
                owned(params)
            )]
        );
    }
}

/// A definite perp refusal: the venue's words, no cause, one WARN under `vike_aster::execution::rest::perp`.
#[test]
fn perp_refusal_is_the_venues_words_with_no_cause_and_one_warn() {
    let client =
        perp(vec![("POST", "/fapi/v3/order", refusal(-5022, "Post Only order will be rejected."))]);
    let order = OrderRequest { time_in_force: TimeInForce::PostOnly, ..limit("p5") };
    let (events, logs) = vike_log::capture::captured(|| client.submit_order(&order));
    assert_eq!(events, vec![submitted("p5"), rejected("p5", "Post Only order will be rejected.")]);
    assert_eq!(
        aster_lines(&logs),
        vec![line(
            "WARN",
            "vike_aster::execution::rest::perp",
            "submit rejected",
            &[
                ("code", "-5022"),
                ("msg", "Post Only order will be rejected."),
                ("order_client_id", "p5")
            ]
        )]
    );
}

/// The native batch: ONE `POST /fapi/v3/batchOrders` of the orders' params as a JSON array (an
/// index-gated order leaves the chunk), each element read by its index (an id accepts, a code refuses
/// in its words, an element with neither is asked about), a whole-batch refusal refusing every order of
/// the chunk in its words.
#[test]
fn perp_batch_is_one_post_and_each_element_is_read_by_its_index() {
    let index = OrderRequest {
        order_type: OrderType::Stop,
        price: None,
        trigger_price: Some(48_000.0),
        trigger_by: Some(TriggerBy::Index),
        ..limit("b0")
    };
    let orders = [index, limit("b1"), limit("b2"), limit("b3")];
    let client = perp(vec![
        (
            "POST",
            "/fapi/v3/batchOrders",
            Ok(json!([{"orderId": 71}, {"code": -2010, "msg": "no money"}, {}])),
        ),
        ("GET", "/fapi/v3/order", Ok(json!({"orderId": 73, "status": "NEW"}))),
    ]);
    let array: Vec<Value> = orders[1..]
        .iter()
        .map(|r| {
            Value::Object(
                client
                    .build_order_params(r)
                    .into_iter()
                    .map(|(k, v)| (k.to_string(), Value::String(v)))
                    .collect(),
            )
        })
        .collect();
    let events = client.submit_batch(&orders);
    assert_eq!(events.len(), 8, "{events:?}");
    assert_eq!(events[..4], [submitted("b0"), submitted("b1"), submitted("b2"), submitted("b3")]);
    assert!(matches!(&events[4], Event::OrderRejected(r) if r.order_client_id == "b0"));
    assert_eq!(
        events[5..],
        [accepted("b1", "71"), rejected("b2", "no money"), accepted("b3", "73")]
    );
    let batch = serde_json::to_string(&array).unwrap();
    assert_eq!(
        client.transport.sent(),
        vec![
            sent(
                "signed",
                "POST",
                &format!("{PERP_HOST}/fapi/v3/batchOrders"),
                &[("batchOrders", &batch)]
            ),
            sent(
                "requery",
                "GET",
                &format!("{PERP_HOST}/fapi/v3/order"),
                &[("symbol", "BTCUSDT"), ("origClientOrderId", "b3")]
            ),
        ]
    );
    let client = perp(vec![("POST", "/fapi/v3/batchOrders", refusal(-1003, "Too many requests."))]);
    let events = client.submit_batch(&orders[1..3]);
    assert_eq!(
        events,
        vec![
            submitted("b1"),
            submitted("b2"),
            rejected("b1", "Too many requests."),
            rejected("b2", "Too many requests.")
        ]
    );
}

/// The perp's cancel, batch cancel, amend and leverage: each ONE signed request on `/fapi/v3`.
#[test]
fn perp_cancel_amend_and_leverage_are_one_request_each_on_fapi_v3() {
    let client = perp(vec![
        ("DELETE", "/fapi/v3/order", refusal(-2011, "Unknown order sent.")),
        ("DELETE", "/fapi/v3/batchOrders", Ok(json!([]))),
        ("PUT", "/fapi/v3/order", Ok(json!({"orderId": 81, "updateTime": 1234}))),
        ("POST", "/fapi/v3/leverage", Ok(json!({"leverage": 3}))),
    ]);
    assert_eq!(client.cancel_order("p6").map_err(|e| e.code), Ok(OrderCancelAck::AlreadyGone));
    assert_eq!(
        client.cancel_batch(&["p7".to_string(), "p8".to_string()]).map_err(|e| e.code),
        Ok(())
    );
    assert_eq!(
        client.modify_order(&limit("p9"), Some(0.02), Some(50_100.0)),
        vec![Event::OrderModified(OrderModified {
            order_client_id: "p9".to_string(),
            venue_order_id: Some("81".into()),
            new_qty: Some(0.02),
            new_price: Some(50_100.0),
            new_trigger_price: None,
            timestamp: 1234,
        })]
    );
    assert_eq!(client.set_leverage().map_err(|e| e.code), Ok(()));
    let url = |path: &str| format!("{PERP_HOST}{path}");
    let quantity = vike_bridge_core::format::format_to_step_f(0.02, 0.001);
    let price = vike_bridge_core::format::format_to_step_f(50_100.0, 0.1);
    assert_eq!(
        client.transport.sent(),
        vec![
            sent(
                "signed",
                "DELETE",
                &url("/fapi/v3/order"),
                &[("symbol", "BTCUSDT"), ("origClientOrderId", "p6")]
            ),
            sent(
                "signed",
                "DELETE",
                &url("/fapi/v3/batchOrders"),
                &[("symbol", "BTCUSDT"), ("origClientOrderIdList", "[\"p7\",\"p8\"]")]
            ),
            sent(
                "signed",
                "PUT",
                &url("/fapi/v3/order"),
                &[
                    ("symbol", "BTCUSDT"),
                    ("side", "BUY"),
                    ("origClientOrderId", "p9"),
                    ("quantity", &quantity),
                    ("price", &price)
                ]
            ),
            sent(
                "signed",
                "POST",
                &url("/fapi/v3/leverage"),
                &[("symbol", "BTCUSDT"), ("leverage", "3")]
            ),
        ]
    );
}

/// The batch cancel reads its answer element by element (S4b BN-2): beside a cancelled order's row, an element
/// saying "Unknown order sent." is already gone, and one under any other code is the batch's error instead of
/// vanishing into an `Ok`.
#[test]
fn perp_batch_cancel_reads_each_element_of_the_answer() {
    let cancelled = json!({"orderId": 9, "clientOrderId": "p7", "status": "CANCELED"});
    let answer = |element: Value| Ok(json!([cancelled, element]));
    let ids = ["p7".to_string(), "p8".to_string()];
    let gone = answer(json!({"code": -2011, "msg": "Unknown order sent."}));
    let client = perp(vec![("DELETE", "/fapi/v3/batchOrders", gone)]);
    assert_eq!(client.cancel_batch(&ids).map_err(|e| e.code), Ok(()));
    for (code, words) in
        [(-2011, "This account may not place or cancel orders."), (-1003, "Too many")]
    {
        let refused = answer(json!({"code": code, "msg": words}));
        let client = perp(vec![("DELETE", "/fapi/v3/batchOrders", refused)]);
        let e = client.cancel_batch(&ids).expect_err("refused");
        assert_eq!((e.code, e.msg.as_str()), (code, words));
        assert_eq!(client.transport.sent().len(), 1, "nothing is retried");
    }
}

/// The perp's ambiguous submit and active confirm ask `GET /fapi/v3/order` on the requery door by the
/// bare order client id.
#[test]
fn perp_ambiguous_submit_and_confirm_ask_by_the_bare_order_client_id() {
    let client = perp(vec![
        ("POST", "/fapi/v3/order", refusal(-1006, "Unexpected response")),
        ("GET", "/fapi/v3/order", Ok(json!({"orderId": 91, "status": "FILLED"}))),
        ("GET", "/fapi/v3/order", refusal(-2013, "Order does not exist.")),
    ]);
    let order = limit("p10");
    let params = client.build_order_params(&order);
    assert_eq!(client.submit_order(&order), vec![submitted("p10"), accepted("p10", "91")]);
    assert_eq!(
        vike_bridge_core::execution_drive::event_tags(&client.confirm_order("p11", 7)),
        vec!["OrderRejected:p11"]
    );
    let ask = |order_client_id: &str| {
        sent(
            "requery",
            "GET",
            &format!("{PERP_HOST}/fapi/v3/order"),
            &[("symbol", "BTCUSDT"), ("origClientOrderId", order_client_id)],
        )
    };
    assert_eq!(
        client.transport.sent(),
        vec![
            ("signed", "POST".to_string(), format!("{PERP_HOST}/fapi/v3/order"), owned(params)),
            ask("p10"),
            ask("p11"),
        ]
    );
}

/// The resync's perp replay: `allOrders` then `userTrades` on `/fapi/v3` for the mounted exchange symbol,
/// then for each foreign perp label's exchange symbol (a spot label is not read); a foreign fill is
/// symbol-qualified on its series label.
#[test]
fn perp_history_reads_the_mounted_symbol_then_each_foreign_one() {
    let limit_word = RESYNC_HISTORY_LIMIT.to_string();
    let order = |order_client_id: &str| json!([{"orderId": 9, "clientOrderId": order_client_id, "status": "FILLED", "origQty": "1.0", "updateTime": 7}]);
    let trade = json!([{"id": 100, "orderId": 9, "price": "50000", "qty": "1.0", "commission": "0.1", "commissionAsset": "USDT", "time": 5, "side": "BUY", "maker": false, "realizedPnl": "0"}]);
    let client = perp(vec![
        ("GET", "/fapi/v3/allOrders", Ok(order("pm"))),
        ("GET", "/fapi/v3/userTrades", Ok(trade.clone())),
        ("GET", "/fapi/v3/allOrders", Ok(order("pf"))),
        ("GET", "/fapi/v3/userTrades", Ok(trade)),
    ]);
    let events = vike_aster::execution::perp_history_events_of(
        &client,
        "BTCUSDT.P",
        &["ETHUSDT.P".to_string(), "SOLUSDT".to_string()],
    );
    let read = |path: &str, symbol: &str| {
        sent(
            "requery",
            "GET",
            &format!("{PERP_HOST}{path}"),
            &[("symbol", symbol), ("limit", &limit_word)],
        )
    };
    assert_eq!(
        client.transport.sent(),
        vec![
            read("/fapi/v3/allOrders", "BTCUSDT"),
            read("/fapi/v3/userTrades", "BTCUSDT"),
            read("/fapi/v3/allOrders", "ETHUSDT"),
            read("/fapi/v3/userTrades", "ETHUSDT"),
        ]
    );
    assert_eq!(
        vike_bridge_core::execution_drive::event_tags(&events),
        vec!["Fill:pm:100", "OrderFilled:pm", "Fill:pf:ETHUSDT.P:100", "OrderFilled:pf"]
    );
}

// ---- the funding-rate history -------------------------------------------------------------------------

/// A spot symbol is refused before anything is asked, in aster's words.
#[test]
fn funding_refuses_a_spot_symbol_in_asters_words() {
    use vike_data::source::{FundingRateSource, SourceError};
    let source = vike_aster::history::FundingRateSourceAster::from_urls(
        &vike_venues::venue_urls::VenueUrls::seed("aster"),
    )
    .expect("the seed");
    match source.fetch("BTCUSDT", 0, 1) {
        Err(SourceError::Refused(why)) => assert_eq!(
            why.text(),
            "aster: \"BTCUSDT\" names SPOT, and funding is a perpetual's series — ask for \
             `BTCUSDT.P`, the store's spelling of the perpetual"
        ),
        Err(SourceError::Fetch(why)) => panic!("asked the venue: {why}"),
        Ok(points) => panic!("asked the venue: {} points", points.len()),
    }
}
