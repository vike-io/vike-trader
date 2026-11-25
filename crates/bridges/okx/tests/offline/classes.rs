//! **The OKX instrument classes the adapter signs besides a SWAP: a USDT-quoted SPOT pair and a dated
//! USDT-linear FUTURE** (decision 0232; offline: scripted transports, canned envelopes, nothing `#[ignore]`d).
//!
//! Every shape is the one MEASURED on the OKX demo account on 2026-10-30 (acctLv 3, net mode;
//! `tests/okx_classes_measure_smoke.rs` is the record, `tests/fixtures/classes/` the captured rows):
//!
//! * a SPOT order is `tdMode` `cross` (`cash` is refused "Parameter tdMode error" at that level), states no
//!   `posSide` and no `reduceOnly`, and a MARKET one states `tgtCcy=base_ccy` (without it `sz` is the QUOTE
//!   amount); an account whose level takes `cash` is served by one retry after exactly that refusal;
//! * a dated future is written exactly like a SWAP, and is refused inside its last day and after its delivery;
//! * a spot BUY's fee is charged in the BASE coin, so the fill is restated net (stream, replay and reconcile
//!   read alike);
//! * the history reads carry the instrument's own `instType` (`51015` otherwise); positions are one read per
//!   class, a spot pair reports its base coin's wallet figure, and an `orders` row of a MARGIN or OPTION order
//!   is not ours.

use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Mutex};

use serde_json::{Value, json};
use vike_bridge_core::rest::VenueRest;
use vike_bridge_core::signer::SignerJson;
use vike_bridge_core::transport::VenueApiError;
use vike_bridge_core::transport::json::RestTransportJson;
use vike_execution::recon::ReconcileClient;
use vike_model::events::Event;
use vike_model::{MarginMode, OrderRequest, OrderType};
use vike_okx::ReconcileClientOkx;
use vike_okx::execution::event_mapper::map_okx_perp_by_instrument;
use vike_okx::execution::rest::{OkxInstrument, VenueRestOkx, parse_okx_perp_instruments};
use vike_okx::execution::resync::map_okx_history;
use vike_okx::instrument::SpotMode;
use vike_okx::reconcile::parse_fills;
use vike_okx::test_support::signer;

const SPOT: &str = "BTC-USDT";
/// The future's id as these tests spell it: the measured row is `QTUM-USDT-261225`, whose delivery the
/// calendar reaches; the tests keep the row's numbers and give it an id delivered in 2099, so none of them
/// is a clock bomb. (The delivery guard has its own tests, with a delivered id.)
const FUTURE: &str = "QTUM-USDT-991231";
const SWAP: &str = "BTC-USDT-SWAP";

fn instruments() -> Value {
    serde_json::from_str(include_str!("../fixtures/classes/demo_instruments_2026-10-30.json"))
        .expect("fixture is JSON")
}

fn frames() -> Value {
    serde_json::from_str(include_str!("../fixtures/classes/demo_spot_fills_2026-10-30.json"))
        .expect("fixture is JSON")
}

/// The exec plane's parse of one fixture row, by the key the fixture names it under.
fn parsed(key: &str) -> OkxInstrument {
    let row = instruments()[key].clone();
    let id = row["instId"].as_str().expect("instId").to_string();
    parse_okx_perp_instruments(&json!({ "data": [row] }))[&id].clone()
}

type Calls = Arc<Mutex<Vec<(String, Value)>>>;

/// A scripted OKX: every signed call is recorded as (path tail, params) and answered with the next canned
/// envelope; a call with no envelope left PANICS (the wire must not be touched).
struct Script {
    calls: Calls,
    replies: Mutex<VecDeque<Value>>,
}

impl Script {
    fn new(replies: Vec<Value>) -> (Self, Calls) {
        let calls: Calls = Arc::default();
        (Script { calls: Arc::clone(&calls), replies: Mutex::new(replies.into()) }, calls)
    }
}

fn tail(path: &str) -> String {
    path.rsplit('/').next().unwrap_or(path).to_string()
}

fn record(calls: &Calls, path: &str, params: &[(&str, Value)]) {
    let map: serde_json::Map<String, Value> =
        params.iter().map(|(k, v)| ((*k).to_string(), v.clone())).collect();
    calls.lock().unwrap().push((tail(path), Value::Object(map)));
}

impl RestTransportJson for Script {
    fn signed(
        &self,
        _base: &str,
        path: &str,
        _method: &str,
        params: &[(&str, Value)],
        _signer: &dyn SignerJson,
    ) -> Result<Value, VenueApiError> {
        record(&self.calls, path, params);
        Ok(self
            .replies
            .lock()
            .unwrap()
            .pop_front()
            .unwrap_or_else(|| panic!("no reply for {path}")))
    }
    vike_bridge_core::no_public_call!();
}

fn accepted(ord_id: &str) -> Value {
    json!({"code": "0", "msg": "", "data": [{"clOrdId": "c", "ordId": ord_id, "sCode": "0", "sMsg": "Order placed"}]})
}

/// The venue's own answer to `cash` at account level 3 (measured, HTTP 200, the per-order code is the news).
fn td_mode_refusal() -> Value {
    json!({"code": "1", "msg": "All operations failed",
           "data": [{"clOrdId": "c", "ordId": "", "sCode": "51000", "sMsg": "Parameter tdMode error "}]})
}

fn refusal(code: &str, msg: &str) -> Value {
    json!({"code": "1", "msg": "All operations failed",
           "data": [{"clOrdId": "c", "ordId": "", "sCode": code, "sMsg": msg}]})
}

fn rest_of<T: RestTransportJson>(key: &str, transport: T) -> VenueRestOkx<T> {
    let instrument = parsed(key);
    let id = if key == "future" {
        FUTURE.to_string()
    } else {
        instruments()[key]["instId"].as_str().unwrap().to_string()
    };
    VenueRestOkx {
        signer: signer(),
        transport,
        base_url: "https://unused.invalid".to_string(),
        symbol: id,
        // base units, exactly what the exec plane hands the client (`properties` stay in CONTRACTS for a
        // derivative: the client divides by `ct_val` itself)
        properties: instrument.properties,
        ct_val: instrument.ct_val,
        leverage: 2.0,
        broker_code: None,
    }
}

fn request(
    symbol: &str,
    side: i32,
    qty: f64,
    order_type: OrderType,
    price: Option<f64>,
) -> OrderRequest {
    OrderRequest {
        order_client_id: "c".to_string(),
        venue: "okx".to_string(),
        symbol: symbol.to_string(),
        side,
        qty,
        order_type,
        price,
        ..Default::default()
    }
}

fn body(params: Vec<(&'static str, Value)>) -> serde_json::Map<String, Value> {
    params.into_iter().map(|(k, v)| (k.to_string(), v)).collect()
}

/// The body `key`'s client writes for `req` (its transport is never asked).
fn written(key: &str, req: &OrderRequest) -> serde_json::Map<String, Value> {
    body(rest_of(key, Script::new(vec![]).0).build_order_params(req, None).expect("builds"))
}

// ---- the instrument rows ------------------------------------------------------------------------

#[test]
fn a_spot_row_counts_size_in_the_base_coin_and_a_future_row_in_contracts() {
    let spot = parsed("spot");
    assert_eq!(spot.inst_type.as_deref(), Some("SPOT"));
    assert_eq!(spot.ct_val, 1.0, "one unit of sz is one BTC: the identity rescale");
    assert_eq!(spot.base_asset, "BTC");
    assert_eq!(spot.quote_currency.as_deref(), Some("USDT"));
    assert_eq!(spot.settle_ccy, None);
    assert_eq!(
        spot.contract_type, None,
        "the venue sends a spot row's ctType as an empty string: no kind, not an unknown one"
    );
    assert_eq!(spot.properties.step_size, 0.00000001);
    assert_eq!(spot.properties.min_qty, 0.00002);
    assert_eq!(spot.properties.tick_size, 0.1);
    assert_eq!(
        spot.properties.max_qty, 0.0,
        "the venue states a spot maxMktSz in the QUOTE currency"
    );

    let future = parsed("future");
    assert_eq!(future.inst_type.as_deref(), Some("FUTURES"));
    assert_eq!((future.ct_val, future.ct_mult), (10.0, 1.0));
    assert_eq!(future.base_asset, "QTUM");
    assert_eq!(future.settle_ccy.as_deref(), Some("USDT"));
    assert_eq!(future.contract_type.as_deref(), Some("linear"));
    assert_eq!(future.state.as_deref(), Some("live"));

    // the inverse swap is the other money model: settled in the coin, a contract is 100 USD
    let inverse = parsed("inverse_swap");
    assert_eq!(inverse.contract_type.as_deref(), Some("inverse"));
    assert_eq!((inverse.ct_val, inverse.settle_ccy.as_deref()), (100.0, Some("BTC")));
}

// ---- the order body -----------------------------------------------------------------------------

#[test]
fn a_spot_limit_order_is_the_measured_body_with_no_pos_side_and_no_reduce_only() {
    let mut b = written("spot", &request(SPOT, 1, 0.00002, OrderType::Limit, Some(38460.1)));
    // the size is spelled at the lot's precision ("0.00002000"): the venue reads the number
    assert_eq!(sz_of(&b), 0.00002);
    b.remove("sz");
    assert_eq!(
        b,
        body(vec![
            ("instId", json!(SPOT)),
            ("tdMode", json!("cross")),
            ("side", json!("buy")),
            ("ordType", json!("limit")),
            ("clOrdId", json!("c")),
            ("px", json!("38460.1")),
        ])
    );
}

fn sz_of(body: &serde_json::Map<String, Value>) -> f64 {
    body["sz"].as_str().expect("sz is a string").parse().expect("sz is a number")
}

#[test]
fn a_spot_market_order_states_its_size_in_the_base_coin_on_both_sides() {
    for side in [1, -1] {
        let b = written("spot", &request(SPOT, side, 0.00002, OrderType::Market, None));
        assert_eq!(
            b["tgtCcy"],
            json!("base_ccy"),
            "without it `sz` is the QUOTE amount (sz 5 bought 5 USDT)"
        );
        assert_eq!(b["ordType"], json!("market"));
        assert_eq!(sz_of(&b), 0.00002);
        assert!(
            !b.contains_key("px") && !b.contains_key("posSide") && !b.contains_key("reduceOnly")
        );
    }
    // a size below the lot floors to the lot, never up: 0.000019999 BTC is 0.00001999 on a 1e-8 grid
    let b = written("spot", &request(SPOT, 1, 0.000019999, OrderType::Limit, Some(1.0)));
    assert_eq!(sz_of(&b), 0.00001999);
}

#[test]
fn a_spot_td_mode_follows_the_attempt_and_the_request() {
    let rest = rest_of("spot", Script::new(vec![]).0);
    let mode = |margin_mode: Option<MarginMode>, attempt: SpotMode| {
        let mut req = request(SPOT, 1, 0.00002, OrderType::Limit, Some(1.0));
        req.margin_mode = margin_mode;
        rest.build_order_params_in(&req, None, attempt).map(|p| body(p)["tdMode"].clone())
    };
    assert_eq!(mode(None, SpotMode::Cross).unwrap(), json!("cross"));
    assert_eq!(mode(None, SpotMode::Cash).unwrap(), json!("cash"));
    assert_eq!(mode(Some(MarginMode::Cash), SpotMode::Cross).unwrap(), json!("cash"));
    assert_eq!(
        mode(Some(MarginMode::Cross), SpotMode::Cash).unwrap(),
        json!("cash"),
        "the attempt decides"
    );
    let why = mode(Some(MarginMode::Isolated), SpotMode::Cross).unwrap_err();
    assert!(why.contains("Isolated") && why.contains("spot pair"), "{why}");
}

#[test]
fn a_dated_future_is_written_exactly_like_a_swap() {
    let mut b = written("future", &request(FUTURE, 1, 10.0, OrderType::Limit, Some(0.3881)));
    assert_eq!(sz_of(&b), 1.0, "10 QTUM at ctVal 10 is one contract");
    b.remove("sz");
    assert_eq!(
        b,
        body(vec![
            ("instId", json!(FUTURE)),
            ("tdMode", json!("cross")),
            ("side", json!("buy")),
            ("ordType", json!("limit")),
            ("clOrdId", json!("c")),
            ("posSide", json!("net")),
            ("reduceOnly", json!(false)),
            ("px", json!("0.3881")),
        ])
    );
}

// ---- the tdMode retry ---------------------------------------------------------------------------

fn submit(
    symbol_key: &str,
    replies: Vec<Value>,
    req: &OrderRequest,
) -> (Vec<Event>, Vec<(String, Value)>) {
    let (script, calls) = Script::new(replies);
    let rest = rest_of(symbol_key, script);
    let events = VenueRest::submit_order(&rest, req);
    let calls = calls.lock().unwrap().clone();
    (events, calls)
}

#[test]
fn a_spot_order_the_account_level_refuses_as_cross_is_sent_once_more_as_cash() {
    let req = request(SPOT, 1, 0.00002, OrderType::Limit, Some(38460.1));
    let (events, calls) = submit("spot", vec![td_mode_refusal(), accepted("999")], &req);
    assert_eq!(calls.len(), 2, "{calls:?}");
    assert_eq!(calls[0].1["tdMode"], json!("cross"));
    assert_eq!(calls[1].1["tdMode"], json!("cash"));
    assert_eq!(
        calls[0].1["clOrdId"], calls[1].1["clOrdId"],
        "a refused order rests nowhere: the id is free"
    );
    assert!(
        matches!(&events[1], Event::OrderAccepted(a) if a.venue_order_id.as_deref() == Some("999")),
        "{events:?}"
    );
}

#[test]
fn only_that_one_refusal_of_an_unnamed_spot_mode_is_retried() {
    let req = request(SPOT, 1, 0.00002, OrderType::Limit, Some(38460.1));
    // another refusal: one call, the venue's words reach the order
    let (events, calls) = submit(
        "spot",
        vec![refusal("51020", "Your order should meet or exceed the minimum order amount.")],
        &req,
    );
    assert_eq!(calls.len(), 1);
    assert!(
        matches!(&events[1], Event::OrderRejected(r) if r.reason.contains("minimum order amount")),
        "{events:?}"
    );
    // a request that NAMES its mode is not second-guessed
    let mut named = req.clone();
    named.margin_mode = Some(MarginMode::Cross);
    let (_, calls) = submit("spot", vec![td_mode_refusal()], &named);
    assert_eq!(calls.len(), 1);
    // a future never retries: its tdMode is the request's
    let future_request = request(FUTURE, 1, 10.0, OrderType::Limit, Some(0.3881));
    let (_, calls) = submit("future", vec![td_mode_refusal()], &future_request);
    assert_eq!(calls.len(), 1, "{FUTURE}");
    // refused both ways: the second answer wins and carries the first's words
    let (events, calls) = submit("spot", vec![td_mode_refusal(), td_mode_refusal()], &req);
    assert_eq!(calls.len(), 2);
    assert!(
        matches!(&events[1], Event::OrderRejected(r) if r.reason.contains("tdMode") && r.reason.contains("the same order as tdMode cross")),
        "{events:?}"
    );
}

// ---- the delivery guard -------------------------------------------------------------------------
//
// The guard reads the order's USER name (`QTUMUSDT_YYMMDD`, decision 0290), the day it names at 08:00 UTC
// (`vike_venues::venue_delivery_guard::delivery_verdict`); the client signs its instId.

/// The USER name of the dated `QTUM-USDT` future delivered on the UTC day `at_ms` falls on.
fn dated(at_ms: i64) -> String {
    let (y, m, d) =
        vike_utility::calendar::civil_from_days(at_ms.div_euclid(vike_model::MS_PER_DAY));
    format!("QTUMUSDT_{:02}{m:02}{d:02}", y - 2000)
}

/// The future delivered at the next 08:00 UTC: between zero and 24 h away, inside its last day.
fn inside_the_window() -> String {
    dated(vike_model::now_ms() - 8 * vike_model::MS_PER_HOUR + vike_model::MS_PER_DAY)
}

fn rejected(events: &[Event]) -> Vec<&str> {
    events
        .iter()
        .filter_map(|e| match e {
            Event::OrderRejected(r) => Some(r.order_client_id.as_str()),
            _ => None,
        })
        .collect()
}

#[test]
fn an_order_on_a_delivered_future_is_rejected_in_words_and_never_signed() {
    let (script, calls) = Script::new(vec![]);
    let mut rest = rest_of("future", script);
    rest.symbol = "QTUM-USDT-200101".to_string(); // 2020-01-01: long delivered
    let req = request("QTUMUSDT_200101", 1, 10.0, OrderType::Limit, Some(0.3881));
    let events = VenueRest::submit_order(&rest, &req);
    assert!(
        matches!(&events[1], Event::OrderRejected(r) if r.reason.contains("delivered")),
        "{events:?}"
    );
    let Event::OrderRejected(r) = &events[1] else { unreachable!() };
    assert_eq!(r.refusal_id.as_deref(), Some(vike_model::OrderRefusalId::NotSent.word()));
    assert!(calls.lock().unwrap().is_empty(), "the wire is not touched");
    // a reduce-only order is no way round a delivered instrument
    let mut reduce = req.clone();
    reduce.reduce_only = true;
    let events = VenueRest::submit_order(&rest, &reduce);
    assert!(matches!(&events[1], Event::OrderRejected(_)), "{events:?}");
}

/// Inside the last day an OPENING order is refused, a stop algo too (the gate runs before the algo route, which it
/// once skipped), and a reduce-only one goes to the wire.
#[test]
fn inside_the_last_day_an_opening_order_and_a_stop_are_refused_and_a_closing_one_is_sent() {
    let user = inside_the_window();
    let (script, calls) = Script::new(vec![accepted("8")]);
    let rest = rest_of("future", script);
    for order_type in [OrderType::Limit, OrderType::Stop] {
        let events = VenueRest::submit_order(
            &rest,
            &request(&user, 1, 10.0, order_type.clone(), Some(0.3881)),
        );
        assert_eq!(events.len(), 2, "{order_type}: {events:?}");
        assert!(
            matches!(&events[1], Event::OrderRejected(r) if r.reason.contains("takes no OPENING order")),
            "{order_type}: {events:?}"
        );
    }
    assert!(calls.lock().unwrap().is_empty(), "nothing opening was signed");
    let mut closing = request(&user, -1, 10.0, OrderType::Limit, Some(0.3881));
    closing.reduce_only = true;
    let events = VenueRest::submit_order(&rest, &closing);
    assert!(matches!(&events[1], Event::OrderAccepted(_)), "{events:?}");
    assert_eq!(calls.lock().unwrap().len(), 1);
}

/// The native batch judges each member: the opening one inside the window and the delivered one are refused, and
/// neither enters a wire chunk.
#[test]
fn a_batch_member_inside_the_last_day_or_delivered_is_refused_before_the_wire() {
    let (script, calls) = Script::new(vec![]);
    let rest = rest_of("future", script);
    let mut opening = request(&inside_the_window(), 1, 10.0, OrderType::Limit, Some(0.3881));
    opening.order_client_id = "c-open".to_string();
    let mut delivered = request("QTUMUSDT_200101", -1, 10.0, OrderType::Limit, Some(0.3881));
    delivered.order_client_id = "c-delivered".to_string();
    delivered.reduce_only = true;
    let events = rest.submit_batch(&[opening, delivered]);
    assert_eq!(rejected(&events), ["c-open", "c-delivered"], "{events:?}");
    for event in &events {
        if let Event::OrderRejected(r) = event {
            assert_eq!(r.refusal_id.as_deref(), Some(vike_model::OrderRefusalId::NotSent.word()));
        }
    }
    assert!(calls.lock().unwrap().is_empty(), "the wire is not touched");
}

#[test]
fn a_future_far_from_its_delivery_goes_to_the_wire() {
    let (script, calls) = Script::new(vec![accepted("7")]);
    let mut rest = rest_of("future", script);
    rest.symbol = "QTUM-USDT-991231".to_string(); // 2099
    let events = VenueRest::submit_order(
        &rest,
        &request("QTUMUSDT_991231", 1, 10.0, OrderType::Limit, Some(0.3881)),
    );
    assert!(matches!(&events[1], Event::OrderAccepted(_)), "{events:?}");
    assert_eq!(calls.lock().unwrap().len(), 1);
}

// ---- the history reads --------------------------------------------------------------------------

#[test]
fn the_history_reads_carry_the_instruments_own_inst_type() {
    for (key, want) in [("spot", "SPOT"), ("future", "FUTURES")] {
        let (script, calls) = Script::new(vec![json!({"code": "0", "msg": "", "data": []}); 2]);
        let rest = rest_of(key, script);
        let id = rest.symbol.clone();
        rest.get_orders_history_for(&id, 50).expect("answers");
        rest.get_fills_history_for(&id, 50).expect("answers");
        let calls = calls.lock().unwrap();
        assert_eq!(calls.len(), 2);
        for (path, params) in calls.iter() {
            assert_eq!(params["instType"], json!(want), "{path} for {id}: 51015 otherwise");
            assert_eq!(params["instId"], json!(id));
        }
    }
    // the mounted SWAP keeps its bytes
    let (script, calls) = Script::new(vec![json!({"code": "0", "msg": "", "data": []})]);
    let mut rest = rest_of("spot", script);
    rest.symbol = SWAP.to_string();
    rest.get_orders_history(50).expect("answers");
    assert_eq!(calls.lock().unwrap()[0].1["instType"], json!("SWAP"));
}

// ---- a spot BUY is restated net of its base-coin fee -------------------------------------------

fn fills_of(events: &[Event]) -> Vec<&vike_model::events::FillEvent> {
    events
        .iter()
        .filter_map(|e| match e {
            Event::Fill(f) => Some(f),
            Event::OrderFilled(w) => Some(&w.fill),
            Event::OrderPartiallyFilled(w) => Some(&w.fill),
            _ => None,
        })
        .collect()
}

fn map(frame_key: &str) -> Vec<Event> {
    map_okx_perp_by_instrument(&frames()[frame_key], "okx", SWAP, |instrument| {
        instrument.eq_ignore_ascii_case(SPOT).then_some(1.0)
    })
}

#[test]
fn the_measured_spot_buy_frame_books_the_coin_the_account_received() {
    let events = map("ws_buy_filled");
    let fills = fills_of(&events);
    assert_eq!(fills.len(), 2, "the Fill and the OrderFilled wrap: {events:?}");
    for f in fills {
        assert_eq!(f.symbol.as_str(), SPOT);
        assert!(
            (f.last_qty - 0.00001998).abs() < 1e-15,
            "0.00002 bought less the 2e-8 BTC fee: {}",
            f.last_qty
        );
        assert_eq!(f.last_price, 76927.2);
        assert!(
            (f.commission - 0.00000002 * 76927.2).abs() < 1e-12,
            "the fee at the fill price: {}",
            f.commission
        );
        assert_eq!(f.commission_asset.as_str(), "USDT");
        assert_eq!(
            f.trade_id.to_string(),
            "BTC-USDT:1418224356",
            "a foreign instrument's id is scoped"
        );
    }
}

#[test]
fn a_spot_sell_frame_keeps_its_quantity_and_its_quote_fee() {
    let events = map("ws_sell_filled");
    for f in fills_of(&events) {
        assert_eq!(f.last_qty, 0.00008492);
        assert_eq!(f.commission, 0.006530628236);
        assert_eq!(f.commission_asset.as_str(), "USDT");
    }
}

#[test]
fn the_live_frame_of_a_spot_order_is_an_accept_and_a_margin_row_is_not_ours() {
    assert!(matches!(map("ws_buy_live").as_slice(), [Event::OrderAccepted(_)]));
    assert!(
        map("ws_margin_order").is_empty(),
        "an order placed by hand as MARGIN is not the engine's"
    );
}

#[test]
fn the_history_replay_and_the_reconcile_read_net_the_fill_as_the_stream_does() {
    let f = frames();
    let replay = map_okx_history(
        &json!([f["rest_order_buy_row"].clone()]),
        &json!([f["rest_fills_buy_row"].clone()]),
        "okx",
        SPOT,
        1.0,
    );
    let fills = fills_of(&replay);
    assert!(!fills.is_empty());
    for fill in fills {
        assert!((fill.last_qty - 0.00001998).abs() < 1e-15, "{}", fill.last_qty);
        assert_eq!(fill.commission_asset.as_str(), "USDT");
    }
    let reports = parse_fills(&json!([f["rest_fills_buy_row"].clone()]).to_string(), 1.0).unwrap();
    assert_eq!(reports.len(), 1);
    assert!((reports[0].last_qty - 0.00001998).abs() < 1e-15);
    assert!((reports[0].commission - 0.00000002 * 76903.4).abs() < 1e-12);
    assert_eq!(reports[0].commission_asset, "USDT");
}

// ---- the reconcile client reads each class under its own instType -------------------------------

/// A scripted OKX for the reconcile client: `public/instruments` answers the fixture row of the
/// `(instType, instId)` asked, every signed read is recorded, `positions` answers per `instType` and
/// `account/balance` with `balances`.
struct Venue {
    calls: Calls,
    rows: HashMap<(String, String), Value>,
    positions: HashMap<String, Vec<Value>>,
    balances: Vec<Value>,
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
        record(&self.calls, path, params);
        let class = params
            .iter()
            .find(|(k, _)| *k == "instType")
            .and_then(|(_, v)| v.as_str())
            .unwrap_or("")
            .to_string();
        let data = match tail(path).as_str() {
            "positions" => self.positions.get(&class).cloned().unwrap_or_default(),
            "balance" => self.balances.clone(),
            _ => Vec::new(),
        };
        Ok(json!({"code": "0", "msg": "", "data": data}))
    }
    fn public(
        &self,
        _base: &str,
        _path: &str,
        params: &[(&str, String)],
    ) -> Result<Value, VenueApiError> {
        let get = |k: &str| {
            params.iter().find(|(key, _)| *key == k).map(|(_, v)| v.clone()).unwrap_or_default()
        };
        Ok(self.rows.get(&(get("instType"), get("instId"))).map_or_else(
            // the venue answers a mismatching instType `51015`
            || json!({"code": "51015", "msg": "Instrument ID doesn't match instrument type.", "data": []}),
            |row| json!({"code": "0", "msg": "", "data": [row]}),
        ))
    }
}

/// The spot rule (owner 2026-11-11): a spot pair reports its BASE coin's `cashBal` as a `Cash` FLOOR row (no price,
/// flat under one `lotSz`), read once for every spot pair of the pass. Before it a spot pair reported no row while the
/// mounted SWAP's row was always there, so the orphan sweep raised the engine's own spot holding as an
/// `OrphanLocalPosition` on every pass, which a confirm would have DROPPED from the book.
#[test]
fn a_spot_pair_and_a_future_are_read_under_their_own_inst_type_and_a_spot_pair_reports_its_wallet()
{
    let all = instruments();
    let mut future_row = all["future"].clone();
    future_row["instId"] = json!(FUTURE); // the measured row under the id these tests spell
    let (calls, rows, positions) = (
        Calls::default(),
        HashMap::from([
            (("SPOT".to_string(), SPOT.to_string()), all["spot"].clone()),
            (("FUTURES".to_string(), FUTURE.to_string()), future_row),
        ]),
        HashMap::from([(
            "FUTURES".to_string(),
            vec![json!({"instId": FUTURE, "instType": "FUTURES", "pos": "1", "posSide": "net",
                        "avgPx": "0.7776", "mgnMode": "cross", "uTime": "1793390291654"})],
        )]),
    );
    let client = ReconcileClientOkx::new(
        signer(),
        Venue {
            calls: Arc::clone(&calls),
            rows,
            positions,
            balances: vec![json!({"details": [
                {"ccy": "USDT", "cashBal": "1000"},
                {"ccy": "BTC", "cashBal": "0.5"}
            ]})],
        },
        "https://unused.invalid",
        SWAP,
        0.01,
    );
    client.admit_symbol(&crate::user(SPOT));
    client.admit_symbol(&crate::user(FUTURE));

    let reports = client.fetch_position_status_reports().expect("the pass reads");
    let by: HashMap<&str, f64> = reports.iter().map(|r| (r.symbol.as_str(), r.qty)).collect();
    // every report leaves under its USER name (decision 0290)
    assert_eq!(
        by.get(crate::user(SWAP).as_str()),
        Some(&0.0),
        "the mounted swap is flat: {reports:?}"
    );
    assert_eq!(
        by.get(crate::user(FUTURE).as_str()),
        Some(&10.0),
        "1 contract x ctVal 10: {reports:?}"
    );
    let spot = reports
        .iter()
        .find(|r| r.symbol == crate::user(SPOT))
        .expect("a spot pair reports its wallet");
    assert_eq!((spot.qty, spot.average_price, spot.margin_mode), (0.5, 0.0, MarginMode::Cash));
    let balance_reads: Vec<Value> = calls
        .lock()
        .unwrap()
        .iter()
        .filter(|(t, _)| t == "balance")
        .map(|(_, p)| p["ccy"].clone())
        .collect();
    assert_eq!(balance_reads, [json!("BTC")], "one read, naming the spot pairs' base coins");

    let position_reads: Vec<String> = calls
        .lock()
        .unwrap()
        .iter()
        .filter(|(t, _)| t == "positions")
        .map(|(_, p)| p["instType"].as_str().unwrap().to_string())
        .collect();
    assert_eq!(
        position_reads,
        ["SWAP", "FUTURES"],
        "one account-wide read per class, never SPOT (refused 51000)"
    );

    // the order reads name each instrument and need no instType
    calls.lock().unwrap().clear();
    client.fetch_order_status_reports(0).expect("the pass reads");
    let pending: Vec<String> = calls
        .lock()
        .unwrap()
        .iter()
        .filter(|(t, _)| t == "orders-pending")
        .map(|(_, p)| p["instId"].as_str().unwrap().to_string())
        .collect();
    assert_eq!(pending, [SWAP, SPOT, FUTURE]);
}
