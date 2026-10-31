//! Binance USDⓈ-M CONDITIONAL orders on the Algo Service, against a SCRIPTED transport that records
//! every request: since 2025-12-09 the venue refuses `STOP`/`STOP_MARKET`/`TAKE_PROFIT*`/
//! `TRAILING_STOP_MARKET` on `POST /fapi/v1/order` and `/fapi/v1/batchOrders` (`-4120`) and takes them on
//! `POST /fapi/v1/algoOrder`. These tests hold the whole bridge side of that move: where a stop is
//! sent, how it is cancelled / looked up / refused a modify, how `ALGO_UPDATE` reaches the order FSM,
//! and that a reconcile pass (legacy snapshot AND the report path) sees a resting stop instead of
//! reaping it.
//!
//! Wire facts quoted from developers.binance.com (New / Cancel / Query Algo Order, Current All Algo
//! Open Orders, Event: Algo Order Update), re-read 2026-10-19 — see `crates/bridges/binance/src/algo.rs`.

use std::sync::Mutex;

use serde_json::{Value, json};
use vike_binance::exec::perp_history_events;
use vike_binance::perp::BinancePerpRest;
use vike_binance::perp_mapper::map_binance_perp_opts;
use vike_binance::recon_client::BinanceReconClient;
use vike_bridge_core::rest::VenueRest;
use vike_bridge_core::signer::Signer;
use vike_bridge_core::transport::{E_TIMEOUT_AMBIGUOUS, RestTransport, VenueApiError};
use vike_exec::recon::ReconClient;
use vike_model::events::Event;
use vike_model::{OrderRequest, OrderStatus, OrderType, SymbolProperties};

use crate::tif_gate::NullSigner;

type Params = Vec<(String, String)>;
type Call = (String, String, Params); // (method, path, params)
type Answer = Box<dyn Fn(&str, &str, &Params) -> Result<Value, VenueApiError> + Send + Sync>;

/// Records every signed request and answers it from a closure `(method, path, params)`.
struct Script {
    calls: Mutex<Vec<Call>>,
    answer: Answer,
}

impl Script {
    fn new(
        answer: impl Fn(&str, &str, &Params) -> Result<Value, VenueApiError> + Send + Sync + 'static,
    ) -> Self {
        Self { calls: Mutex::new(Vec::new()), answer: Box::new(answer) }
    }

    fn calls(&self) -> Vec<Call> {
        self.calls.lock().unwrap().clone()
    }

    /// `"METHOD path"` of every request, in order.
    fn wire(&self) -> Vec<String> {
        self.calls().into_iter().map(|(m, p, _)| format!("{m} {p}")).collect()
    }
}

impl RestTransport for Script {
    fn signed(
        &self,
        _base: &str,
        path: &str,
        method: &str,
        params: &[(&str, String)],
        _signer: &dyn Signer,
    ) -> Result<Value, VenueApiError> {
        let params: Params = params.iter().map(|(k, v)| (k.to_string(), v.clone())).collect();
        self.calls.lock().unwrap().push((method.to_string(), path.to_string(), params.clone()));
        (self.answer)(method, path, &params)
    }

    fn public(
        &self,
        _base: &str,
        path: &str,
        _params: &[(&str, String)],
    ) -> Result<Value, VenueApiError> {
        panic!("unexpected public {path}")
    }
}

fn err(code: i64, msg: &str) -> VenueApiError {
    VenueApiError { code, msg: msg.to_string() }
}

fn get<'a>(params: &'a Params, key: &str) -> Option<&'a str> {
    params.iter().find(|(k, _)| k == key).map(|(_, v)| v.as_str())
}

fn props() -> SymbolProperties {
    SymbolProperties { tick_size: 0.1, step_size: 0.001, ..Default::default() }
}

fn perp(script: Script, link_id: Option<&str>) -> BinancePerpRest<NullSigner, Script> {
    BinancePerpRest {
        link_id: link_id.map(str::to_string),
        signer: NullSigner,
        transport: script,
        base_url: "https://stub".into(),
        algo_coids: Default::default(),
        symbol: "BTCUSDT".into(),
        properties: props(),
        leverage: 2.0,
    }
}

fn stop(coid: &str) -> OrderRequest {
    OrderRequest {
        client_order_id: coid.to_string(),
        venue: "binance".to_string(),
        symbol: "BTCUSDT".to_string(),
        side: -1,
        qty: 0.005,
        order_type: OrderType::Stop,
        trigger_price: Some(58000.0),
        reduce_only: true,
        ..Default::default()
    }
}

fn limit(coid: &str) -> OrderRequest {
    OrderRequest {
        client_order_id: coid.to_string(),
        venue: "binance".to_string(),
        symbol: "BTCUSDT".to_string(),
        side: 1,
        qty: 0.002,
        order_type: OrderType::Limit,
        price: Some(60000.0),
        ..Default::default()
    }
}

/// The New Algo Order ack as the docs show it (keys only; no `orderId` exists on this response).
fn algo_ack(coid: &str) -> Value {
    json!({
        "algoId": 2146760, "clientAlgoId": coid, "algoType": "CONDITIONAL",
        "orderType": "STOP_MARKET", "symbol": "BTCUSDT", "side": "SELL", "positionSide": "BOTH",
        "quantity": "0.005", "algoStatus": "NEW", "triggerPrice": "58000.0", "workingType": "CONTRACT_PRICE",
        "closePosition": false, "priceProtect": false, "reduceOnly": true,
        "createTime": 1750485492076_i64, "updateTime": 1750485492076_i64, "triggerTime": 0, "goodTillDate": 0
    })
}

/// The Query Algo Order body for `status`.
fn algo_row(coid: &str, status: &str) -> Value {
    json!({
        "algoId": 2146760, "clientAlgoId": coid, "algoType": "CONDITIONAL", "orderType": "STOP_MARKET",
        "symbol": "BTCUSDT", "side": "SELL", "positionSide": "BOTH", "quantity": "0.005",
        "algoStatus": status, "actualOrderId": "", "actualPrice": "0.00000", "triggerPrice": "58000.0",
        "price": "0.0", "workingType": "CONTRACT_PRICE", "closePosition": false, "priceProtect": false,
        "reduceOnly": true, "createTime": 1750485492076_i64, "updateTime": 1750514545091_i64,
        "triggerTime": 0, "goodTillDate": 0
    })
}

fn kinds(events: &[Event]) -> Vec<&'static str> {
    events
        .iter()
        .map(|e| match e {
            Event::OrderSubmitted(_) => "submitted",
            Event::OrderAccepted(_) => "accepted",
            Event::OrderRejected(_) => "rejected",
            Event::OrderCanceled(_) => "canceled",
            Event::OrderExpired(_) => "expired",
            Event::OrderTriggered(_) => "triggered",
            Event::OrderModifyRejected(_) => "modify_rejected",
            other => panic!("{other:?}"),
        })
        .collect()
}

// ---- 1. submit routing --------------------------------------------------------------------------

#[test]
fn a_stop_goes_to_the_algo_endpoint_and_a_limit_stays_on_order() {
    let c = perp(
        Script::new(|_m, path, p| match path {
            "/fapi/v1/algoOrder" => Ok(algo_ack(get(p, "clientAlgoId").unwrap_or("?"))),
            "/fapi/v1/order" => Ok(json!({"orderId": 5, "status": "NEW"})),
            other => panic!("unexpected {other}"),
        }),
        None,
    );
    let ev = c.submit_order(&stop("sl1"));
    match ev.last() {
        Some(Event::OrderAccepted(a)) => {
            assert_eq!(a.client_order_id, "sl1");
            assert_eq!(
                a.venue_order_id.as_deref(),
                Some("2146760"),
                "the ack's algoId, never orderId"
            );
        }
        other => panic!("{other:?}"),
    }
    let ev = c.submit_order(&limit("lm1"));
    assert!(
        matches!(ev.last(), Some(Event::OrderAccepted(a)) if a.venue_order_id.as_deref() == Some("5"))
    );

    let calls = c.transport.calls();
    assert_eq!(
        (calls[0].0.as_str(), calls[0].1.as_str()),
        ("POST", "/fapi/v1/algoOrder"),
        "{calls:?}"
    );
    let p = &calls[0].2;
    assert_eq!(get(p, "algoType"), Some("CONDITIONAL"));
    assert_eq!(get(p, "type"), Some("STOP_MARKET"));
    assert_eq!(get(p, "triggerPrice"), Some("58000.0"));
    assert_eq!(get(p, "clientAlgoId"), Some("sl1"));
    assert_eq!(get(p, "stopPrice"), None, "the refused name");
    assert_eq!(get(p, "newClientOrderId"), None, "the refused name");
    assert_eq!((calls[1].0.as_str(), calls[1].1.as_str()), ("POST", "/fapi/v1/order"), "{calls:?}");
}

#[test]
fn a_stop_in_a_batch_is_sent_singly_and_the_event_order_is_kept() {
    let c = perp(
        Script::new(|_m, path, p| match path {
            "/fapi/v1/batchOrders" => {
                let n = get(p, "batchOrders")
                    .and_then(|b| serde_json::from_str::<Vec<Value>>(b).ok())
                    .map_or(0, |v| v.len());
                Ok(Value::Array((0..n).map(|i| json!({"orderId": 100 + i})).collect()))
            }
            "/fapi/v1/algoOrder" => Ok(algo_ack("b")),
            other => panic!("unexpected {other}"),
        }),
        None,
    );
    let ev = c.submit_batch(&[limit("a"), stop("b"), limit("c")]);
    assert_eq!(
        c.transport.wire(),
        ["POST /fapi/v1/batchOrders", "POST /fapi/v1/algoOrder", "POST /fapi/v1/batchOrders"],
        "the stop leaves the chunk and is sent on its own, between its neighbours"
    );
    assert_eq!(
        kinds(&ev),
        ["submitted", "submitted", "submitted", "accepted", "accepted", "accepted"]
    );
    let ids: Vec<&str> = ev
        .iter()
        .filter_map(|e| match e {
            Event::OrderAccepted(a) => Some(a.client_order_id.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(ids, ["a", "b", "c"], "accepts come back in request order");
}

#[test]
fn a_refused_stop_keeps_the_venues_own_words() {
    let c = perp(
        Script::new(|_m, path, _p| match path {
            "/fapi/v1/algoOrder" => Err(err(-2021, "Order would immediately trigger.")),
            _ => Err(err(
                -4120,
                "Order type not supported for this endpoint. Please use the Algo Order API endpoints instead.",
            )),
        }),
        None,
    );
    let ev = c.submit_order(&stop("sl2"));
    match ev.last() {
        Some(Event::OrderRejected(r)) => {
            assert!(r.reason.contains("immediately trigger"), "{}", r.reason)
        }
        other => panic!("{other:?}"),
    }
}

/// A stop whose POST times out may be live at the venue: it is looked up, on the ALGO endpoint first.
#[test]
fn an_ambiguous_stop_submit_is_resolved_on_the_algo_endpoint() {
    let c = perp(
        Script::new(|m, path, p| match (m, path) {
            ("POST", _) => Err(err(E_TIMEOUT_AMBIGUOUS, "network error: timeout")),
            ("GET", "/fapi/v1/algoOrder") => Ok(algo_row(get(p, "clientAlgoId").unwrap(), "NEW")),
            ("GET", "/fapi/v1/order") => Err(err(-2013, "Order does not exist.")),
            other => panic!("unexpected {other:?}"),
        }),
        None,
    );
    let ev = c.submit_order(&stop("sl3"));
    assert_eq!(kinds(&ev), ["submitted", "accepted"], "{ev:?}");
    assert!(
        matches!(ev.last(), Some(Event::OrderAccepted(a)) if a.venue_order_id.as_deref() == Some("2146760"))
    );
}

// ---- 2. cancel ----------------------------------------------------------------------------------

#[test]
fn cancelling_a_stop_this_client_placed_goes_straight_to_the_algo_delete() {
    for link in [None, Some("LINK")] {
        let c = perp(
            Script::new(|m, _path, p| match m {
                "POST" => Ok(algo_ack(get(p, "clientAlgoId").unwrap_or("?"))),
                "DELETE" => Ok(
                    json!({"algoId": 2146760, "clientAlgoId": "x", "code": "200", "msg": "success"}),
                ),
                other => panic!("unexpected {other:?}"),
            }),
            link,
        );
        c.submit_order(&stop("sl4"));
        assert!(c.cancel_order("sl4").is_ok());
        assert_eq!(
            c.transport.wire(),
            ["POST /fapi/v1/algoOrder", "DELETE /fapi/v1/algoOrder"],
            "no regular DELETE for a member"
        );
        let calls = c.transport.calls();
        let posted = get(&calls[0].2, "clientAlgoId").unwrap().to_string();
        let deleted = get(&calls[1].2, "clientAlgoId").unwrap();
        assert_eq!(deleted, posted, "cancelled under the id the venue stored ({link:?})");
        assert_eq!(get(&calls[1].2, "symbol"), None, "Cancel Algo Order takes no symbol");
    }
}

/// After a restart the set is empty: the regular DELETE says unknown, and the same id is tried ONCE
/// on the algo endpoint before anyone concludes "already gone".
#[test]
fn a_cancel_the_regular_endpoint_does_not_know_is_retried_once_on_the_algo_endpoint() {
    let c = perp(
        Script::new(|m, path, _p| match (m, path) {
            ("DELETE", "/fapi/v1/order") => Err(err(-2011, "Unknown order sent.")),
            ("DELETE", "/fapi/v1/algoOrder") => {
                Ok(json!({"algoId": 1, "clientAlgoId": "z9", "code": "200", "msg": "success"}))
            }
            other => panic!("unexpected {other:?}"),
        }),
        Some("LINK"),
    );
    assert!(c.cancel_order("z9").is_ok());
    assert_eq!(c.transport.wire(), ["DELETE /fapi/v1/order", "DELETE /fapi/v1/algoOrder"]);
    assert_eq!(get(&c.transport.calls()[1].2, "clientAlgoId"), Some("x-LINK-z9"));
}

#[test]
fn unknown_on_both_endpoints_is_already_gone() {
    for algo_code in [-2011, -2013] {
        let c = perp(
            Script::new(move |_m, path, _p| match path {
                "/fapi/v1/order" => Err(err(-2011, "Unknown order sent.")),
                _ => Err(err(algo_code, "Order does not exist.")),
            }),
            None,
        );
        assert!(c.cancel_order("gone").is_ok(), "algo answered {algo_code}");
    }
}

/// A resting stop must never be reported cancelled because the retry itself failed.
#[test]
fn a_failed_algo_retry_is_an_error_not_already_gone() {
    let c = perp(
        Script::new(|_m, path, _p| match path {
            "/fapi/v1/order" => Err(err(-2011, "Unknown order sent.")),
            _ => Err(err(-1003, "Too many requests.")),
        }),
        None,
    );
    let e = c.cancel_order("live-stop").expect_err("the stop may still rest");
    assert_eq!(e.code, -1003);
}

#[test]
fn the_ordinary_cancel_path_is_unchanged() {
    let c = perp(
        Script::new(|_m, path, _p| match path {
            "/fapi/v1/order" => Ok(json!({"orderId": 1, "status": "CANCELED"})),
            other => panic!("unexpected {other}"),
        }),
        None,
    );
    assert!(c.cancel_order("lm").is_ok());
    assert_eq!(c.transport.wire(), ["DELETE /fapi/v1/order"]);
}

#[test]
fn a_batch_cancel_sends_stops_singly_and_retries_unknown_elements_on_the_algo_endpoint() {
    let c = perp(
        Script::new(|m, path, p| match (m, path) {
            ("POST", _) => Ok(algo_ack(get(p, "clientAlgoId").unwrap_or("?"))),
            ("DELETE", "/fapi/v1/algoOrder") => {
                Ok(json!({"algoId": 1, "clientAlgoId": "x", "code": "200", "msg": "success"}))
            }
            // the regular batch: `lm` is ours and goes; `old-stop` is unknown here (a stop from before a restart)
            ("DELETE", "/fapi/v1/batchOrders") => Ok(json!([
                {"orderId": 7, "clientOrderId": "lm", "status": "CANCELED"},
                {"code": -2011, "msg": "Unknown order sent."}
            ])),
            other => panic!("unexpected {other:?}"),
        }),
        None,
    );
    c.submit_order(&stop("sl5"));
    assert!(c.cancel_batch(&["sl5".into(), "lm".into(), "old-stop".into()]).is_ok());
    let wire = c.transport.wire();
    assert_eq!(wire[0], "POST /fapi/v1/algoOrder");
    let algo_deletes: Vec<String> = c
        .transport
        .calls()
        .iter()
        .filter(|(m, p, _)| m == "DELETE" && p == "/fapi/v1/algoOrder")
        .map(|(_, _, p)| get(p, "clientAlgoId").unwrap().to_string())
        .collect();
    assert_eq!(algo_deletes, ["sl5", "old-stop"], "the member, then the unknown element: {wire:?}");
    let batch =
        c.transport.calls().into_iter().find(|(_, p, _)| p == "/fapi/v1/batchOrders").unwrap();
    let list: Vec<String> =
        serde_json::from_str(get(&batch.2, "origClientOrderIdList").unwrap()).unwrap();
    assert_eq!(list, ["lm", "old-stop"], "the member never rides the regular batch");
}

// ---- 3. lookup / confirm ------------------------------------------------------------------------

fn lookup_script(algo: Result<Value, i64>) -> Script {
    Script::new(move |_m, path, _p| match path {
        "/fapi/v1/order" => Err(err(-2013, "Order does not exist.")),
        "/fapi/v1/algoOrder" => algo.clone().map_err(|code| err(code, "stub")),
        other => panic!("unexpected {other}"),
    })
}

#[test]
fn a_confirm_that_finds_no_regular_order_falls_back_to_the_algo_status() {
    let table: [(&str, &[&str]); 7] = [
        ("NEW", &["accepted"]),
        ("TRIGGERING", &["accepted"]),
        ("TRIGGERED", &["accepted"]),
        ("FINISHED", &["accepted"]),
        ("CANCELED", &["accepted", "canceled"]),
        ("EXPIRED", &["accepted", "expired"]),
        ("REJECTED", &["rejected"]),
    ];
    for (status, want) in table {
        let c = perp(lookup_script(Ok(algo_row("sl6", status))), None);
        let ev = c.confirm_order("sl6", 5);
        assert_eq!(kinds(&ev), want, "algoStatus {status}: {ev:?}");
        if want[0] == "accepted" {
            assert!(
                matches!(&ev[0], Event::OrderAccepted(a) if a.venue_order_id.as_deref() == Some("2146760"))
            );
        }
    }
}

#[test]
fn absent_on_both_endpoints_is_the_venue_confirms_absent_reject() {
    let c = perp(lookup_script(Err(-2013)), None);
    let ev = c.confirm_order("nope", 5);
    assert_eq!(kinds(&ev), ["rejected"], "{ev:?}");
}

#[test]
fn a_failed_algo_query_says_nothing() {
    for code in [E_TIMEOUT_AMBIGUOUS, -2015, -1003, 503] {
        let c = perp(lookup_script(Err(code)), None);
        assert!(c.confirm_order("q", 5).is_empty(), "code {code}");
    }
}

// ---- 4. modify ----------------------------------------------------------------------------------

#[test]
fn modifying_a_stop_is_an_advisory_without_a_wire_call() {
    let c = perp(
        Script::new(|_m, path, _p| panic!("a stop modify must not touch the wire (saw {path})")),
        None,
    );
    let ev = c.modify_order(&stop("sl7"), Some(0.004), None);
    assert_eq!(kinds(&ev), ["modify_rejected"], "{ev:?}");
    assert!(c.transport.calls().is_empty());
}

// ---- 5. ALGO_UPDATE -----------------------------------------------------------------------------

/// `Event: Algo Order Update`, the USDⓈ-M envelope: `e`, `T`, `E`, `o{caid, aid, at, o, s, S, ps, f,
/// q, X, ai, ap, aq, act, tp, p, V, wt, pm, cp, pP, R, tt, gtd, rm, ia}`.
fn algo_update(status: &str, caid: &str, ai: &str, rm: &str) -> Value {
    json!({
        "e": "ALGO_UPDATE", "T": 1750515742297_i64, "E": 1750515742303_i64,
        "o": {
            "caid": caid, "aid": 2148719, "at": "CONDITIONAL", "o": "STOP_MARKET", "s": "BTCUSDT",
            "S": "SELL", "ps": "BOTH", "f": "GTC", "q": "0.005", "X": status, "ai": ai,
            "ap": "0.00000", "aq": "0.00000", "act": "0", "tp": "58000", "p": "0",
            "V": "NONE", "wt": "CONTRACT_PRICE", "pm": "NONE", "cp": false, "pP": false, "R": true,
            "tt": 0, "gtd": 0, "rm": rm, "ia": false
        }
    })
}

fn map(frame: &Value) -> Vec<Event> {
    map_binance_perp_opts(frame, "binance", "BTCUSDT.P", false)
}

#[test]
fn each_algo_status_reaches_the_order_fsm_as_the_right_event() {
    let table: [(&str, &[&str]); 7] = [
        // the stream's second ack: every regular order is rescued by its NEW frame when the REST ack
        // is lost, and a stop must be too (the FSM drops it silently when the REST ack came first)
        ("NEW", &["accepted"]),
        ("CANCELED", &["canceled"]),
        ("EXPIRED", &["expired"]),
        ("REJECTED", &["canceled"]), // OrderRejected is legal only from Initialized/Submitted
        ("TRIGGERING", &["triggered"]),
        ("TRIGGERED", &["triggered"]),
        ("FINISHED", &[]), // the child's own fills / cancel end the order
    ];
    for (status, want) in table {
        let ev = map(&algo_update(status, "sl8", "", ""));
        assert_eq!(kinds(&ev), want, "X={status}: {ev:?}");
    }
}

/// A real engine holding `coid` (a stop on the perp series label) in `status`, started on that label.
fn engine_holding(
    coid: &str,
    status: OrderStatus,
) -> vike_exec::ExecutionEngine<vike_exec::testing::RecordingClient> {
    use vike_exec::testing::RecordingClient;
    use vike_exec::{Account, BalanceMode, ExecutionEngine, RiskGate};
    use vike_model::{ManagedOrder, RiskLimits};

    let mut engine = ExecutionEngine::new(
        Account::new(1.0, "binance", None, BalanceMode::Delta),
        RiskGate::new(RiskLimits::new()),
        RecordingClient::default(),
        "binance",
        "BTCUSDT.P",
    );
    let mut request = stop(coid);
    request.symbol = "BTCUSDT.P".to_string();
    let mut order = ManagedOrder::new(request);
    order.status = status;
    engine.registry.insert(coid.to_string(), order);
    engine
}

fn fold(
    engine: &mut vike_exec::ExecutionEngine<vike_exec::testing::RecordingClient>,
    ev: &[Event],
) {
    use vike_exec::EventHandler;
    let mut outbox = vike_exec::Outbox::default();
    for e in ev {
        engine.on_event(e, &mut outbox);
    }
}

/// **A lost REST ack of a stop is rescued by the stream.** REST degraded, WS healthy (the usual incident
/// shape): the POST times out and the lookup fails too, so the bridge says NOTHING (no accept, and above
/// all no reject: the stop may rest at the venue). `ALGO_UPDATE NEW` then arrives within ~100 ms, as the
/// capture measured, and must accept the order, exactly as `ORDER_TRADE_UPDATE x=NEW` does for every
/// regular order — otherwise the stuck-order watchdog's last-resort backstop rejects a resting stop.
#[test]
fn a_stop_whose_rest_ack_is_lost_is_accepted_by_its_new_frame() {
    let c = perp(
        Script::new(|_m, _path, _p| Err(err(E_TIMEOUT_AMBIGUOUS, "network error: timeout"))),
        None,
    );
    let sent = c.submit_order(&stop("sl31"));
    assert_eq!(kinds(&sent), ["submitted"], "no accept and no reject on a lost ack: {sent:?}");

    let mut engine = engine_holding("sl31", OrderStatus::Initialized);
    fold(&mut engine, &sent);
    assert_eq!(engine.registry["sl31"].status, OrderStatus::Submitted);

    let new_frame = map(&algo_update("NEW", "sl31", "", ""));
    match &new_frame[..] {
        [Event::OrderAccepted(a)] => {
            assert_eq!(a.client_order_id, "sl31");
            assert_eq!(a.venue_order_id.as_deref(), Some("2148719"), "the frame's algo id");
        }
        other => panic!("{other:?}"),
    }
    fold(&mut engine, &new_frame);
    assert_eq!(engine.registry["sl31"].status, OrderStatus::Accepted, "rescued, never rejected");

    // a repeat (the REST ack that finally arrives, or a second NEW) is refused silently
    fold(&mut engine, &new_frame);
    assert_eq!(engine.registry["sl31"].status, OrderStatus::Accepted);
}

/// The frame can also win the race against the REST ack's own events: NEW first, then the ack's
/// `[OrderSubmitted, OrderAccepted]`, ends accepted (the late ones are refused).
#[test]
fn the_new_frame_may_arrive_before_the_rest_ack_is_folded() {
    let c =
        perp(Script::new(|_m, _path, p| Ok(algo_ack(get(p, "clientAlgoId").unwrap_or("?")))), None);
    let ack = c.submit_order(&stop("sl32"));
    let mut engine = engine_holding("sl32", OrderStatus::Initialized);
    fold(&mut engine, &map(&algo_update("NEW", "sl32", "", "")));
    fold(&mut engine, &ack);
    assert_eq!(engine.registry["sl32"].status, OrderStatus::Accepted, "{ack:?}");
}

#[test]
fn a_rejected_trigger_carries_the_venues_reason_and_the_coid_is_unprefixed() {
    let ev = map(&algo_update("REJECTED", "x-LINK-sl9", "", "Reduce Only reject"));
    match &ev[..] {
        [Event::OrderCanceled(c)] => {
            assert_eq!(c.client_order_id, "sl9");
            assert_eq!(&*c.reason, "Reduce Only reject");
            assert_eq!(c.ts, 1750515742297);
        }
        other => panic!("{other:?}"),
    }
}

/// The portfolio-margin spelling of the same event nests the object under `ao`: read both.
#[test]
fn the_algo_object_is_read_under_o_or_ao() {
    let mut frame = algo_update("CANCELED", "sl10", "", "");
    let o = frame["o"].take();
    frame["ao"] = o;
    frame.as_object_mut().unwrap().remove("o");
    assert_eq!(kinds(&map(&frame)), ["canceled"]);
}

/// **The trigger sequence MEASURED on the DEMO host (2026-10-19, `tests/perp_algo_smoke.rs`), frame for
/// frame, ids replaced**, folded through a real engine: `ALGO_UPDATE TRIGGERING` -> the CHILD market
/// order's `ORDER_TRADE_UPDATE` `NEW` and `TRADE` (its `c` IS the `clientAlgoId`, `si` the algo id,
/// `st` `ALGO_CONDITION`) -> `ALGO_UPDATE TRIGGERED` (`ai` set) -> `FINISHED`. The fill arrives BEFORE
/// `TRIGGERED`, and nothing needs re-keying: the stop's own order ends FILLED, the position moves, and
/// the late `TRIGGERED` lands on a filled order without harm.
#[test]
fn the_measured_trigger_sequence_fills_the_stops_own_order() {
    use vike_exec::testing::RecordingClient;
    use vike_exec::{Account, BalanceMode, EventHandler, ExecutionEngine, Outbox, RiskGate};
    use vike_model::{ManagedOrder, RiskLimits};

    const COID: &str = "vtalgoN00000001";
    const ALGO_ID: i64 = 1_000_000_000_000_001;
    const CHILD_ID: i64 = 1_000_000_001;
    let algo = |t: i64, x: &str, more: Value| {
        let mut o = json!({
            "caid": COID, "aid": ALGO_ID, "at": "CONDITIONAL", "o": "STOP_MARKET", "s": "ETHUSDT",
            "S": "SELL", "ps": "BOTH", "f": "GTC", "q": "0.013", "X": x, "ai": "", "tp": "2400.53",
            "p": "0", "V": "EXPIRE_MAKER", "wt": "CONTRACT_PRICE", "pm": "NONE", "cp": false,
            "pP": false, "R": true, "tt": 1792436059647_i64, "gtd": 0, "ia": false
        });
        o.as_object_mut().unwrap().extend(more.as_object().unwrap().clone());
        json!({"e": "ALGO_UPDATE", "T": t, "E": t + 14, "o": o})
    };
    let child = |x: &str, status: &str, ap: &str, l: &str, trade: i64, rp: &str| {
        json!({"e": "ORDER_TRADE_UPDATE", "T": 1792436059663_i64, "E": 1792436059663_i64, "o": {
            "s": "ETHUSDT", "c": COID, "S": "SELL", "o": "MARKET", "f": "GTC", "q": "0.013", "p": "0",
            "ap": ap, "sp": "0", "x": x, "X": status, "i": CHILD_ID, "l": l, "z": l,
            "L": if l == "0" { "0" } else { "2386.54" },
            "n": if l == "0" { "0" } else { "0.01241" }, "N": "USDT", "T": 1792436059663_i64,
            "t": trade, "b": "0", "a": "0", "m": false, "R": true, "wt": "CONTRACT_PRICE",
            "ot": "MARKET", "ps": "BOTH", "cp": false, "rp": rp, "pP": false, "si": ALGO_ID,
            "ss": -1, "st": "ALGO_CONDITION", "V": "EXPIRE_MAKER", "pm": "NONE", "gtd": 0, "er": "0"
        }})
    };
    let sequence = [
        algo(1792436059647, "TRIGGERING", json!({})),
        child("NEW", "NEW", "0", "0", 0, "0"),
        child("TRADE", "FILLED", "2386.54", "0.013", 340869935, "-0.19045"),
        algo(
            1792436059675,
            "TRIGGERED",
            json!({"ai": CHILD_ID.to_string(), "ap": "0", "aq": "0", "act": "MARKET"}),
        ),
        algo(
            1792436059675,
            "FINISHED",
            json!({"ai": CHILD_ID.to_string(), "ap": "2386.54", "aq": "0.013", "act": "MARKET"}),
        ),
    ];

    let mut engine = ExecutionEngine::new(
        Account::new(1.0, "binance", None, BalanceMode::Delta),
        RiskGate::new(RiskLimits::new()),
        RecordingClient::default(),
        "binance",
        "ETHUSDT.P",
    );
    let mut request = stop(COID);
    request.symbol = "ETHUSDT.P".to_string();
    request.qty = 0.013;
    let mut resting = ManagedOrder::new(request);
    resting.status = OrderStatus::Accepted;
    engine.registry.insert(COID.to_string(), resting);

    let mapped: Vec<Vec<Event>> = sequence
        .iter()
        .map(|frame| map_binance_perp_opts(frame, "binance", "ETHUSDT.P", false))
        .collect();
    assert_eq!(kinds(&mapped[0]), ["triggered"]);
    assert_eq!(kinds(&mapped[1]), ["accepted"], "the child's NEW names the stop's own coid");
    let owners: Vec<&str> = mapped[2]
        .iter()
        .map(|e| match e {
            Event::Fill(f) => f.client_order_id.as_str(),
            Event::OrderFilled(f) => f.client_order_id.as_str(),
            other => panic!("{other:?}"),
        })
        .collect();
    assert_eq!(owners, [COID, COID], "the fill and its wrap carry the stop's coid with no re-key");
    assert_eq!(kinds(&mapped[3]), ["triggered"]);
    assert!(mapped[4].is_empty(), "FINISHED ends nothing the child's frames did not");

    let mut outbox = Outbox::default();
    for ev in mapped.iter().flatten() {
        engine.on_event(ev, &mut outbox);
    }
    assert_eq!(engine.registry[COID].status, OrderStatus::Filled, "the stop's own order is filled");
    let position: f64 = engine
        .local_view()
        .positions
        .iter()
        .filter(|((s, _), _)| s == "ETHUSDT.P")
        .map(|(_, q)| *q)
        .sum();
    assert!((position + 0.013).abs() < 1e-9, "the SELL moved the position: {position}");
}

// ---- 6. reconcile: the legacy snapshot ----------------------------------------------------------

struct Recon {
    algo: Result<Value, i64>,
}

impl RestTransport for Recon {
    fn signed(
        &self,
        _base: &str,
        path: &str,
        _method: &str,
        _params: &[(&str, String)],
        _signer: &dyn Signer,
    ) -> Result<Value, VenueApiError> {
        if path.ends_with("/positionRisk") {
            Ok(json!([]))
        } else if path.ends_with("/balance") {
            Ok(json!([{"asset": "USDT", "balance": "1000.0"}]))
        } else if path.ends_with("/openOrders") {
            Ok(json!([]))
        } else if path.ends_with("/openAlgoOrders") {
            self.algo.clone().map_err(|code| err(code, "stub"))
        } else {
            panic!("unexpected path {path}")
        }
    }
    fn public(&self, _b: &str, path: &str, _p: &[(&str, String)]) -> Result<Value, VenueApiError> {
        panic!("unexpected public {path}")
    }
}

fn open_algo_row(coid: &str) -> Value {
    json!({
        "algoId": 2148627, "clientAlgoId": coid, "algoType": "CONDITIONAL", "orderType": "STOP_MARKET",
        "symbol": "BTCUSDT", "side": "SELL", "positionSide": "BOTH", "timeInForce": "GTC",
        "quantity": "0.005", "algoStatus": "NEW", "actualOrderId": "", "actualPrice": "0.00000",
        "triggerPrice": "58000.000", "price": "0.000", "icebergQuantity": null,
        "tpTriggerPrice": "0.000", "tpPrice": "0.000", "slTriggerPrice": "0.000", "slPrice": "0.000",
        "tpOrderType": "", "selfTradePreventionMode": "EXPIRE_MAKER", "workingType": "CONTRACT_PRICE",
        "priceMatch": "NONE", "closePosition": false, "priceProtect": false, "reduceOnly": true,
        "createTime": 1750514941540_i64, "updateTime": 1750514941540_i64, "triggerTime": 0, "goodTillDate": 0
    })
}

fn snapshot_client(algo: Result<Value, i64>) -> BinancePerpRest<NullSigner, Recon> {
    BinancePerpRest {
        link_id: None,
        signer: NullSigner,
        transport: Recon { algo },
        base_url: "https://stub".into(),
        algo_coids: Default::default(),
        symbol: "BTCUSDT".into(),
        properties: props(),
        leverage: 2.0,
    }
}

#[test]
fn the_reconcile_snapshot_carries_a_resting_stop_as_accepted() {
    let c = snapshot_client(Ok(json!([open_algo_row("x-LINK-sl11")])));
    let snap = c.reconcile_positions().unwrap();
    assert_eq!(snap.open_orders.len(), 1, "{:?}", snap.open_orders);
    let mo = &snap.open_orders[0];
    assert_eq!(mo.client_order_id(), "sl11", "broker prefix stripped");
    assert_eq!(mo.status, OrderStatus::Accepted);
    assert_eq!(mo.request.order_type, OrderType::Stop);
    assert_eq!(mo.request.side, -1);
    assert_eq!(mo.request.qty, 0.005);
    assert_eq!(mo.request.trigger_price, Some(58000.0));
    assert!(mo.request.reduce_only);
    assert_eq!(mo.venue_order_id.as_deref(), Some("2148627"));
}

/// `apply_snapshot` terminalizes every live local order the snapshot lacks. A failed algo read that
/// looked like "no stops" would therefore cancel a live stop in the book while it rests at the venue.
#[test]
fn a_failed_open_algo_read_does_not_reap_a_live_algo_stop() {
    use vike_exec::testing::RecordingClient;
    use vike_exec::{Account, BalanceMode, ExecutionEngine, RiskGate};
    use vike_model::{ManagedOrder, RiskLimits};

    let mut engine = ExecutionEngine::new(
        Account::new(1.0, "binance", None, BalanceMode::Delta),
        RiskGate::new(RiskLimits::new()),
        RecordingClient::default(),
        "binance",
        "BTCUSDT",
    );
    // a live, venue-accepted stop in the book (what a stop placed through the algo endpoint becomes)
    let mut live = ManagedOrder::new(stop("sl12"));
    live.status = OrderStatus::Accepted;
    engine.registry.insert("sl12".to_string(), live);

    // 1. the algo read FAILED: the fetch fails as a whole, so there is no snapshot to apply
    let failed = snapshot_client(Err(-1003)).reconcile_positions();
    assert!(failed.is_err(), "a failed algo read must not become a snapshot without stops");

    // 2. the read succeeded: the stop is in the snapshot, so it survives
    let snap = snapshot_client(Ok(json!([open_algo_row("sl12")]))).reconcile_positions().unwrap();
    engine.apply_snapshot(&snap);
    assert_eq!(
        engine.registry["sl12"].status,
        OrderStatus::Accepted,
        "a listed stop is not reaped"
    );

    // 3. control: the snapshot a SWALLOWED failure would have produced does reap it — so (1) is the guard
    let without = snapshot_client(Ok(json!([]))).reconcile_positions().unwrap();
    engine.apply_snapshot(&without);
    assert_eq!(engine.registry["sl12"].status, OrderStatus::Canceled, "control: absent => reaped");
}

// ---- 7. reconcile: the report path --------------------------------------------------------------

type Seen = std::sync::Arc<Mutex<Vec<(String, Option<String>)>>>; // (path, symbol)

/// `openAlgoOrders` answers by the `symbol` it is asked about: a body, or the venue code it fails with.
type AlgoAnswer = Box<dyn Fn(&str) -> Result<Value, i64> + Send + Sync>;

struct ReportWire {
    algo: AlgoAnswer,
    seen: Seen,
}

impl RestTransport for ReportWire {
    fn signed(
        &self,
        _base: &str,
        path: &str,
        _method: &str,
        params: &[(&str, String)],
        _signer: &dyn Signer,
    ) -> Result<Value, VenueApiError> {
        let symbol = params.iter().find(|(k, _)| *k == "symbol").map(|(_, v)| v.clone());
        self.seen.lock().unwrap().push((path.to_string(), symbol.clone()));
        if path.ends_with("/openOrders") {
            Ok(json!([]))
        } else if path.ends_with("/openAlgoOrders") {
            let symbol = symbol.unwrap_or_default();
            (self.algo)(&symbol).map_err(|code| err(code, "stub"))
        } else {
            panic!("unexpected path {path}")
        }
    }
    fn public(&self, _b: &str, path: &str, _p: &[(&str, String)]) -> Result<Value, VenueApiError> {
        panic!("unexpected public {path}")
    }
}

fn report_client_with(
    algo: impl Fn(&str) -> Result<Value, i64> + Send + Sync + 'static,
) -> (BinanceReconClient<NullSigner, ReportWire>, Seen) {
    let seen = Seen::default();
    let client = BinanceReconClient::perp(
        NullSigner,
        ReportWire { algo: Box::new(algo), seen: seen.clone() },
        "https://stub",
        "BTCUSDT",
    );
    (client, seen)
}

fn report_client_seen(
    algo: Result<Value, i64>,
) -> (BinanceReconClient<NullSigner, ReportWire>, Seen) {
    report_client_with(move |_| algo.clone())
}

fn report_client(algo: Result<Value, i64>) -> BinanceReconClient<NullSigner, ReportWire> {
    report_client_seen(algo).0
}

#[test]
fn the_report_path_lists_a_resting_stop() {
    let c = report_client(Ok(json!([open_algo_row("x-LINK-sl13")])));
    let reports = c.fetch_order_status_reports(0).unwrap();
    assert_eq!(reports.len(), 1, "{reports:?}");
    let r = &reports[0];
    assert_eq!(r.client_order_id.as_deref(), Some("sl13"));
    assert_eq!(r.symbol, "BTCUSDT.P", "the perp series label");
    assert_eq!(r.order_type, OrderType::Stop);
    assert_eq!(r.status, OrderStatus::Accepted);
    assert_eq!(r.qty, 0.005);
    assert_eq!(&*r.venue_order_id, "2148627");
    assert_eq!(r.side, -1);
}

/// The restore planner reads an order a SUCCESSFUL report lacks as gone, so a failed algo read must
/// fail the report instead of returning the ordinary orders alone.
#[test]
fn a_failed_open_algo_read_fails_the_report() {
    let c = report_client(Err(-1003));
    assert!(c.fetch_order_status_reports(0).is_err());
}

#[test]
fn every_admitted_symbol_is_read_for_its_algo_orders_too() {
    let (c, seen) = report_client_seen(Ok(json!([])));
    c.admit_symbol("ETHUSDT.P");
    assert!(c.fetch_order_status_reports(0).unwrap().is_empty());
    let algo_symbols: Vec<Option<String>> = seen
        .lock()
        .unwrap()
        .iter()
        .filter(|(path, _)| path == "/fapi/v1/openAlgoOrders")
        .map(|(_, symbol)| symbol.clone())
        .collect();
    assert_eq!(algo_symbols, [Some("BTCUSDT".to_string()), Some("ETHUSDT".to_string())]);
}

/// As the family's `openOrders` read does: the MOUNTED symbol's answer passes through untouched, an
/// ADMITTED symbol's answer keeps only the rows of the symbol that was asked for.
#[test]
fn a_row_for_an_unasked_symbol_is_dropped_from_an_admitted_algo_read() {
    let row = |sym: &str, coid: &str| {
        let mut r = open_algo_row(coid);
        r["symbol"] = json!(sym);
        r
    };
    let (c, _) = report_client_with(move |symbol| {
        Ok(json!([row(symbol, &format!("c-{symbol}")), row("SOLUSDT", "stray")]))
    });
    c.admit_symbol("ETHUSDT.P");
    let reports = c.fetch_order_status_reports(0).unwrap();
    let coids: Vec<_> = reports.iter().filter_map(|r| r.client_order_id.clone()).collect();
    assert_eq!(coids, ["c-BTCUSDT", "stray", "c-ETHUSDT"]);
}

/// A symbol the venue no longer lists (`-1121`) holds no algo orders for an ADMITTED symbol and must not
/// blind the account; the same code on the MOUNTED symbol is still an error.
#[test]
fn a_delisted_admitted_symbol_reads_as_no_algo_rows() {
    let (c, _) = report_client_with(|symbol| {
        if symbol == "ETHUSDT" { Err(-1121) } else { Ok(json!([open_algo_row("sl")])) }
    });
    c.admit_symbol("ETHUSDT.P");
    assert_eq!(c.fetch_order_status_reports(0).unwrap().len(), 1);

    let (c, _) = report_client_with(|_| Err(-1121));
    assert!(c.fetch_order_status_reports(0).is_err(), "the mounted symbol is not excused");
}

/// A rate-limit answer on an ADMITTED symbol's algo read starts the same 60 s pass-wide pause the
/// family's `openOrders` read starts (`FamilyReconClient::admitted_rows`): the next fetches make NO
/// request until it lapses, so the extra reads cannot walk the account into a ban. Any other failure
/// still fails the pass without a pause, and the mounted symbol's read (like the family's) starts none.
#[test]
fn a_rate_limited_admitted_algo_read_backs_the_pass_off() {
    for code in [-1003, 429, 418] {
        let (c, seen) =
            report_client_with(
                move |symbol| {
                    if symbol == "ETHUSDT" { Err(code) } else { Ok(json!([])) }
                },
            );
        c.admit_symbol("ETHUSDT.P");
        assert!(c.fetch_order_status_reports(0).is_err(), "{code}");
        let before = seen.lock().unwrap().len();
        let err = c.fetch_order_status_reports(0).unwrap_err();
        assert!(err.contains("backing off"), "{code}: {err}");
        let err = c.fetch_position_status_reports().unwrap_err();
        assert!(err.contains("backing off"), "{code}: {err}");
        assert_eq!(seen.lock().unwrap().len(), before, "{code}: a backed-off pass sends nothing");
    }

    // any other failure fails the pass, but the next one is allowed to ask again
    let (c, seen) =
        report_client_with(|symbol| if symbol == "ETHUSDT" { Err(-2015) } else { Ok(json!([])) });
    c.admit_symbol("ETHUSDT.P");
    assert!(c.fetch_order_status_reports(0).is_err());
    let before = seen.lock().unwrap().len();
    let err = c.fetch_order_status_reports(0).unwrap_err();
    assert!(!err.contains("backing off"), "{err}");
    assert!(seen.lock().unwrap().len() > before, "no pause for a non-rate-limit failure");

    // the mounted symbol starts none either (the family's rule for its own read)
    let (c, seen) = report_client_with(|_| Err(-1003));
    assert!(c.fetch_order_status_reports(0).is_err());
    let before = seen.lock().unwrap().len();
    let err = c.fetch_order_status_reports(0).unwrap_err();
    assert!(!err.contains("backing off"), "{err}");
    assert!(seen.lock().unwrap().len() > before);
}

// ---- 8. the post-reconnect resync ---------------------------------------------------------------

/// The history client: `allOrders` and `userTrades` answer `all_orders` / nothing, `allAlgoOrders`
/// answers `algo` (a body, or the venue code it fails with).
fn history_client(
    algo: Result<Value, i64>,
    all_orders: Value,
) -> BinancePerpRest<NullSigner, Script> {
    perp(
        Script::new(move |m, path, _p| match path {
            "/fapi/v1/allOrders" => Ok(all_orders.clone()),
            "/fapi/v1/userTrades" => Ok(json!([])),
            "/fapi/v1/allAlgoOrders" => algo.clone().map_err(|code| err(code, "stub")),
            other => panic!("unexpected {m} {other}"),
        }),
        None,
    )
}

/// **A stop that dies in a WS gap is seen by the resync.** `allOrders` does not list an algo order, so
/// without `GET /fapi/v1/allAlgoOrders` a cancel / expiry / rejected trigger that landed while the
/// stream was down never reached the FSM and the book kept a stop the venue no longer had.
#[test]
fn a_stop_that_died_during_a_gap_is_terminal_after_the_resync() {
    for (status, want) in [
        ("CANCELED", OrderStatus::Canceled),
        ("EXPIRED", OrderStatus::Expired),
        ("REJECTED", OrderStatus::Canceled), // the FSM allows OrderRejected only before the ack
    ] {
        let c = history_client(Ok(json!([algo_row("x-LINK-sl40", status)])), json!([]));
        let ev = perp_history_events(&c, "BTCUSDT.P");
        let mut engine = engine_holding("sl40", OrderStatus::Accepted);
        fold(&mut engine, &ev);
        assert_eq!(engine.registry["sl40"].status, want, "algoStatus {status}: {ev:?}");
    }
}

#[test]
fn the_resync_reads_all_algo_orders_for_its_symbol_with_a_limit() {
    let c = history_client(Ok(json!([])), json!([]));
    perp_history_events(&c, "BTCUSDT.P");
    let calls = c.transport.calls();
    let algo: Vec<_> =
        calls.iter().filter(|(m, p, _)| m == "GET" && p == "/fapi/v1/allAlgoOrders").collect();
    assert_eq!(algo.len(), 1, "{:?}", c.transport.wire());
    assert_eq!(get(&algo[0].2, "symbol"), Some("BTCUSDT"));
    assert!(get(&algo[0].2, "limit").is_some_and(|l| l.parse::<u32>().is_ok_and(|n| n > 0)));
    // no window: the venue answers a span over seven days with -4165 (`algo_old_stop.rs`), and an
    // old stop's update time is what its default window is cut on
    let keys: Vec<&str> = algo[0].2.iter().map(|(k, _)| k.as_str()).collect();
    assert_eq!(keys, ["symbol", "limit"], "{:?}", algo[0].2);
}

/// A live stop is untouched: its row says `NEW` (or it has triggered), nothing is replayed for it, and a
/// terminal row for a stop the book already closed lands on a terminal order and changes nothing.
#[test]
fn the_resync_leaves_live_and_already_closed_stops_alone() {
    let rows = json!([
        algo_row("sl41", "NEW"),
        algo_row("sl42", "TRIGGERED"),
        algo_row("sl43", "FINISHED"),
        algo_row("sl44", "CANCELED"),
    ]);
    let c = history_client(Ok(rows), json!([]));
    let ev = perp_history_events(&c, "BTCUSDT.P");
    assert_eq!(kinds(&ev), ["canceled"], "only the terminal row replays: {ev:?}");

    let mut live = engine_holding("sl41", OrderStatus::Accepted);
    fold(&mut live, &ev);
    assert_eq!(live.registry["sl41"].status, OrderStatus::Accepted);

    let mut closed = engine_holding("sl44", OrderStatus::Canceled);
    fold(&mut closed, &ev);
    assert_eq!(closed.registry["sl44"].status, OrderStatus::Canceled);
}

/// The new read is best effort like its two neighbours: a failed `allAlgoOrders` (or a junk body) costs
/// the algo rows only, never the order and fill replay that already worked.
#[test]
fn a_failed_all_algo_orders_read_keeps_the_rest_of_the_replay() {
    let regular = json!([{
        "orderId": 7, "clientOrderId": "reg1", "symbol": "BTCUSDT", "status": "CANCELED",
        "side": "BUY", "type": "LIMIT", "origQty": "0.002", "executedQty": "0", "price": "60000",
        "time": 1, "updateTime": 2
    }]);
    for algo in [Err(-1003), Ok(json!({"not": "an array"}))] {
        let c = history_client(algo, regular.clone());
        let ev = perp_history_events(&c, "BTCUSDT.P");
        assert_eq!(kinds(&ev), ["canceled"], "{ev:?}");
    }
}

// ---- 9. the coid set is seeded by adoption --------------------------------------------------------

#[test]
fn reading_the_open_algo_list_seeds_the_coid_set_so_a_cancel_goes_straight_to_the_algo_endpoint() {
    let c = perp(
        Script::new(|m, path, _p| match (m, path) {
            (_, "/fapi/v2/positionRisk") => Ok(json!([])),
            (_, "/fapi/v2/balance") => Ok(json!([{"asset": "USDT", "balance": "1.0"}])),
            (_, "/fapi/v1/openOrders") => Ok(json!([])),
            (_, "/fapi/v1/openAlgoOrders") => Ok(json!([open_algo_row("x-LINK-sl50")])),
            ("DELETE", "/fapi/v1/algoOrder") => {
                Ok(json!({"algoId": 1, "clientAlgoId": "x", "code": "200", "msg": "success"}))
            }
            other => panic!("unexpected {other:?}"),
        }),
        Some("LINK"),
    );
    assert!(!c.algo_coids.contains("sl50"));
    c.reconcile_positions().unwrap();
    assert!(c.algo_coids.contains("sl50"), "the adopted stop is remembered under its bare coid");
    assert!(c.cancel_order("sl50").is_ok());
    let wire = c.transport.wire();
    assert_eq!(wire.last().map(String::as_str), Some("DELETE /fapi/v1/algoOrder"), "{wire:?}");
    assert!(!wire.iter().any(|w| w == "DELETE /fapi/v1/order"), "no regular DELETE: {wire:?}");
}
