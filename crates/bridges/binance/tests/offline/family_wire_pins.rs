//! **What binance's signed REST clients send, and what they make of each answer, pinned before the R7 move**
//! (the owner's ruling of 2026-11-13: aster's signed REST and exec plane join `vike_binance::family`,
//! "behaviour exactly as now"). The twin of `crates/bridges/aster/tests/offline/family_wire_pins.rs`, on
//! the paths the move shares: every request the client hands the transport (the door, where the requery
//! door is the short-timeout one; the method; the host and path; the params in order), the events made
//! of each answer, the refusals, and each log line's level, target, words and fields.
//!
//! Binance's own parts ride beside the shared ones and are pinned with them: the broker link id on every
//! order client id the venue is asked about, the three bridge floors and `post_only_cause`, the perp's
//! refusal words (`crate::modes`'s `reject_reason`) and the Algo Service's `allAlgoOrders` read in the
//! perp replay (decision 0186). No socket, no credential: the signer prepares nothing.

use std::sync::Mutex;

use serde_json::{Value, json};
use vike_binance::execution::rest::perp::VenueRestBinancePerp;
use vike_binance::execution::rest::spot::VenueRestBinanceSpot;
use vike_bridge_core::rest::{OrderCancelAck, VenueRest};
use vike_bridge_core::signer::{PreparedRequest, Signer};
use vike_bridge_core::transport::{RestTransport, VenueApiError};
use vike_bridge_core::websocket_session::user::RESYNC_HISTORY_LIMIT;
use vike_model::events::{
    Event, OrderAccepted, OrderModified, OrderRejected, OrderSubmitted, RejectCause,
};
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

/// Answers each request with the FIRST scripted answer for its `(method, path)` (each used once) and
/// records every request; an unscripted request panics.
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

const SPOT_HOST: &str = "https://api.pin";
const PERP_HOST: &str = "https://fapi.pin";

fn grid() -> SymbolProperties {
    SymbolProperties { tick_size: 0.1, step_size: 0.001, ..Default::default() }
}

fn spot(script: Script) -> VenueRestBinanceSpot<NullSigner, Wire> {
    VenueRestBinanceSpot {
        signer: NullSigner,
        transport: Wire::new(script),
        base_url: SPOT_HOST.into(),
        symbol: "BTCUSDT".into(),
        properties: grid(),
        base_asset: "BTC".into(),
        link_id: Some("LINK".into()),
    }
}

fn perp(script: Script) -> VenueRestBinancePerp<NullSigner, Wire> {
    VenueRestBinancePerp {
        signer: NullSigner,
        transport: Wire::new(script),
        base_url: PERP_HOST.into(),
        ty: vike_venues::venue_urls::UrlType::Live,
        margining: vike_model::Margining::Linear,
        symbol: "BTCUSDT".into(),
        properties: grid(),
        leverage: 3.0,
        link_id: Some("LINK".into()),
        algo_order_client_ids: Default::default(),
    }
}

/// A limit BUY `order_client_id` of 0.01 at 50000, stamped `ts` 7.
fn limit(order_client_id: &str) -> OrderRequest {
    OrderRequest {
        order_client_id: order_client_id.to_string(),
        venue: "binance".to_string(),
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

/// The venue's refusal of `order_client_id` (every rejection these pins expect is the venue's own).
fn rejected(order_client_id: &str, reason: &str, cause: Option<RejectCause>) -> Event {
    Event::OrderRejected(OrderRejected {
        cause,
        order_client_id: order_client_id.to_string(),
        reason: reason.into(),
        timestamp: 7,
        refusal_id: vike_model::events::refusal_id_of(
            vike_model::OrderRefusalId::VenueRefused.word(),
        ),
    })
}

/// `(level, target, message, fields)` of every line a `vike_binance` target wrote.
fn binance_lines(logs: &[vike_log::capture::CapturedEvent]) -> Vec<Line> {
    logs.iter()
        .filter(|e| e.target.starts_with("vike_binance"))
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

/// A spot limit is ONE `POST /api/v3/order` of the builder's params, its order client id stamped with
/// the broker link id.
#[test]
fn spot_limit_is_one_post_with_the_broker_stamped_order_client_id() {
    let client = spot(vec![("POST", "/api/v3/order", Ok(json!({"orderId": 11})))]);
    let order = limit("c1");
    let params = client.build_order_params(&order);
    assert!(params.contains(&("newClientOrderId", "x-LINK-c1".to_string())), "{params:?}");
    assert_eq!(client.submit_order(&order), vec![submitted("c1"), accepted("c1", "11")]);
    assert_eq!(
        client.transport.sent(),
        vec![("signed", "POST".to_string(), format!("{SPOT_HOST}/api/v3/order"), owned(params))]
    );
}

/// **Binance's three spot floors refuse before the wire**, each in its own words: a GTD limit (the TIF
/// floor), a stop triggered by the mark price (the trigger floor) and a market-to-limit (the kind floor).
#[test]
fn spot_floors_refuse_before_the_wire() {
    let gtd = OrderRequest { time_in_force: TimeInForce::Gtd, gtd_expiry: Some(1), ..limit("c2") };
    let mark = OrderRequest {
        order_type: OrderType::Stop,
        price: None,
        trigger_price: Some(48_000.0),
        trigger_by: Some(TriggerBy::Mark),
        ..limit("c3")
    };
    let mtl = OrderRequest { order_type: OrderType::MarketToLimit, price: None, ..limit("c4") };
    let floors = [
        vike_venues::venue_time_in_force::deny_unsupported_time_in_force("binance", &gtd),
        vike_bridge_core::trigger::deny_unsupported_trigger_by("binance", &mark),
        vike_binance::execution::rest::perp::deny_unbuilt_kind(&mtl, true),
    ];
    for (order, floor) in [gtd, mark, mtl].iter().zip(floors) {
        let client = spot(Vec::new());
        let floor = floor.expect("the floor refuses it");
        assert_eq!(client.submit_order(order), vec![submitted(&order.order_client_id), floor]);
        assert!(client.transport.sent().is_empty());
    }
}

/// A definite spot refusal keeps the venue's words, names the post-only cross when it is one, and is one
/// WARN under `vike_binance::execution::rest::spot`.
#[test]
fn spot_refusal_carries_the_post_only_cause_and_one_warn() {
    let words = "Order would immediately match and take.";
    let client = spot(vec![("POST", "/api/v3/order", refusal(-2010, words))]);
    let order = OrderRequest { time_in_force: TimeInForce::PostOnly, ..limit("c5") };
    let (events, logs) = vike_log::capture::captured(|| client.submit_order(&order));
    assert_eq!(
        events,
        vec![submitted("c5"), rejected("c5", words, Some(RejectCause::PostOnlyWouldCross))]
    );
    assert_eq!(
        binance_lines(&logs),
        vec![line(
            "WARN",
            "vike_binance::execution::rest::spot",
            "submit rejected",
            &[("code", "-2010"), ("msg", words), ("order_client_id", "c5")]
        )]
    );
}

/// A cancel, an ambiguous submit's re-query and the active confirm each name the BROKER-STAMPED id.
#[test]
fn spot_cancel_requery_and_confirm_name_the_broker_stamped_order_client_id() {
    let client = spot(vec![
        ("DELETE", "/api/v3/order", refusal(-2011, "Unknown order sent.")),
        ("POST", "/api/v3/order", Ok(json!({}))),
        ("GET", "/api/v3/order", Ok(json!({"orderId": 31, "status": "NEW"}))),
        ("GET", "/api/v3/order", refusal(-2013, "Order does not exist.")),
    ]);
    assert_eq!(client.cancel_order("c6").map_err(|e| e.code), Ok(OrderCancelAck::AlreadyGone));
    let order = limit("c7");
    let params = client.build_order_params(&order);
    assert_eq!(client.submit_order(&order), vec![submitted("c7"), accepted("c7", "31")]);
    assert_eq!(
        vike_bridge_core::execution_drive::event_tags(&client.confirm_order("c8", 7)),
        vec!["OrderRejected:c8"]
    );
    let order_url = format!("{SPOT_HOST}/api/v3/order");
    assert_eq!(
        client.transport.sent(),
        vec![
            sent(
                "signed",
                "DELETE",
                &order_url,
                &[("symbol", "BTCUSDT"), ("origClientOrderId", "x-LINK-c6")]
            ),
            ("signed", "POST".to_string(), order_url.clone(), owned(params)),
            sent(
                "requery",
                "GET",
                &order_url,
                &[("symbol", "BTCUSDT"), ("origClientOrderId", "x-LINK-c7")]
            ),
            sent(
                "requery",
                "GET",
                &order_url,
                &[("symbol", "BTCUSDT"), ("origClientOrderId", "x-LINK-c8")]
            ),
        ]
    );
}

/// The resync's spot replay: `allOrders` then `myTrades` (binance's spot path) on the requery door, the
/// mounted symbol first, then a foreign spot symbol; the ids the venue echoes are read with the link id.
#[test]
fn spot_history_reads_the_mounted_symbol_then_each_foreign_one() {
    let limit_word = RESYNC_HISTORY_LIMIT.to_string();
    let order = |order_client_id: &str| json!([{"orderId": 9, "clientOrderId": format!("x-LINK-{order_client_id}"), "status": "FILLED", "origQty": "1.0", "updateTime": 7}]);
    let trade = json!([{"id": 100, "orderId": 9, "price": "50000", "qty": "1.0", "commission": "0.1", "commissionAsset": "USDT", "time": 5, "isBuyer": true, "isMaker": false}]);
    let client = spot(vec![
        ("GET", "/api/v3/allOrders", Ok(order("cm"))),
        ("GET", "/api/v3/myTrades", Ok(trade.clone())),
        ("GET", "/api/v3/allOrders", Ok(order("cf"))),
        ("GET", "/api/v3/myTrades", Ok(trade)),
    ]);
    let events = vike_binance::execution::spot_history_events_of(
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
            read("/api/v3/myTrades", "BTCUSDT"),
            read("/api/v3/allOrders", "ETHUSDT"),
            read("/api/v3/myTrades", "ETHUSDT"),
        ]
    );
    assert_eq!(
        vike_bridge_core::execution_drive::event_tags(&events),
        vec!["Fill:cm:100", "OrderFilled:cm", "Fill:cf:ETHUSDT:100", "OrderFilled:cf"]
    );
}

// ---- perp ---------------------------------------------------------------------------------------------

/// A perp limit is ONE `POST /fapi/v1/order`; a definite refusal names the post-only cross, keeps the
/// venue's words through `reject_reason` (a mainnet client re-probes nothing) and is one WARN under
/// `vike_binance::execution::rest::perp`.
#[test]
fn perp_limit_and_its_refusal() {
    let client = perp(vec![("POST", "/fapi/v1/order", Ok(json!({"orderId": 51})))]);
    let order = limit("p1");
    let params = client.build_order_params(&order);
    assert_eq!(client.submit_order(&order), vec![submitted("p1"), accepted("p1", "51")]);
    assert_eq!(
        client.transport.sent(),
        vec![("signed", "POST".to_string(), format!("{PERP_HOST}/fapi/v1/order"), owned(params))]
    );
    let words = "Post Only order will be rejected.";
    let client = perp(vec![("POST", "/fapi/v1/order", refusal(-5022, words))]);
    let order = OrderRequest { time_in_force: TimeInForce::PostOnly, ..limit("p2") };
    let (events, logs) = vike_log::capture::captured(|| client.submit_order(&order));
    assert_eq!(
        events,
        vec![submitted("p2"), rejected("p2", words, Some(RejectCause::PostOnlyWouldCross))]
    );
    assert_eq!(
        binance_lines(&logs),
        vec![line(
            "WARN",
            "vike_binance::execution::rest::perp",
            "submit rejected",
            &[("code", "-5022"), ("msg", words), ("order_client_id", "p2")]
        )]
    );
}

/// The native batch: ONE `POST /fapi/v1/batchOrders` (a GTD with no date leaves the chunk on the perp
/// floor), each element read by its index, a whole-batch refusal refusing the chunk in its words.
#[test]
fn perp_batch_is_one_post_and_each_element_is_read_by_its_index() {
    let gtd = OrderRequest { time_in_force: TimeInForce::Gtd, ..limit("b0") };
    let orders = [gtd, limit("b1"), limit("b2"), limit("b3")];
    let client = perp(vec![
        (
            "POST",
            "/fapi/v1/batchOrders",
            Ok(json!([{"orderId": 71}, {"code": -2010, "msg": "no money"}, {}])),
        ),
        ("GET", "/fapi/v1/order", Ok(json!({"orderId": 73, "status": "NEW"}))),
        ("GET", "/fapi/v1/algoOrder", refusal(-2013, "Order does not exist.")),
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
        [accepted("b1", "71"), rejected("b2", "no money", None), accepted("b3", "73")]
    );
    let batch = serde_json::to_string(&array).unwrap();
    assert_eq!(
        client.transport.sent(),
        vec![
            sent(
                "signed",
                "POST",
                &format!("{PERP_HOST}/fapi/v1/batchOrders"),
                &[("batchOrders", &batch)]
            ),
            sent(
                "requery",
                "GET",
                &format!("{PERP_HOST}/fapi/v1/order"),
                &[("symbol", "BTCUSDT"), ("origClientOrderId", "x-LINK-b3")]
            ),
        ]
    );
    let client = perp(vec![("POST", "/fapi/v1/batchOrders", refusal(-1003, "Too many requests."))]);
    assert_eq!(
        client.submit_batch(&orders[1..3]),
        vec![
            submitted("b1"),
            submitted("b2"),
            rejected("b1", "Too many requests.", None),
            rejected("b2", "Too many requests.", None)
        ]
    );
}

/// The perp's amend and leverage: ONE signed request each on `/fapi/v1`, the amend naming the
/// broker-stamped id.
#[test]
fn perp_amend_and_leverage_are_one_request_each() {
    let client = perp(vec![
        ("PUT", "/fapi/v1/order", Ok(json!({"orderId": 81, "updateTime": 1234}))),
        ("POST", "/fapi/v1/leverage", Ok(json!({"leverage": 3}))),
    ]);
    assert_eq!(
        client.modify_order(&limit("p3"), Some(0.02), Some(50_100.0)),
        vec![Event::OrderModified(OrderModified {
            order_client_id: "p3".to_string(),
            venue_order_id: Some("81".into()),
            new_qty: Some(0.02),
            new_price: Some(50_100.0),
            new_trigger_price: None,
            timestamp: 1234,
        })]
    );
    assert_eq!(client.set_leverage().map_err(|e| e.code), Ok(()));
    let quantity = vike_bridge_core::format::format_to_step_f(0.02, 0.001);
    let price = vike_bridge_core::format::format_to_step_f(50_100.0, 0.1);
    assert_eq!(
        client.transport.sent(),
        vec![
            sent(
                "signed",
                "PUT",
                &format!("{PERP_HOST}/fapi/v1/order"),
                &[
                    ("symbol", "BTCUSDT"),
                    ("side", "BUY"),
                    ("origClientOrderId", "x-LINK-p3"),
                    ("quantity", &quantity),
                    ("price", &price)
                ]
            ),
            sent(
                "signed",
                "POST",
                &format!("{PERP_HOST}/fapi/v1/leverage"),
                &[("symbol", "BTCUSDT"), ("leverage", "3")]
            ),
        ]
    );
}

/// The resync's perp replay: `allOrders`, `userTrades` and the Algo Service's `allAlgoOrders` on the
/// requery door, the mounted exchange symbol first, then each foreign perp label's.
#[test]
fn perp_history_reads_orders_trades_and_algo_orders_of_each_symbol() {
    let limit_word = RESYNC_HISTORY_LIMIT.to_string();
    let order = |order_client_id: &str| json!([{"orderId": 9, "clientOrderId": format!("x-LINK-{order_client_id}"), "status": "FILLED", "origQty": "1.0", "updateTime": 7}]);
    let trade = json!([{"id": 100, "orderId": 9, "price": "50000", "qty": "1.0", "commission": "0.1", "commissionAsset": "USDT", "time": 5, "side": "BUY", "maker": false, "realizedPnl": "0"}]);
    let client = perp(vec![
        ("GET", "/fapi/v1/allOrders", Ok(order("pm"))),
        ("GET", "/fapi/v1/userTrades", Ok(trade.clone())),
        ("GET", "/fapi/v1/allAlgoOrders", Ok(json!([]))),
        ("GET", "/fapi/v1/allOrders", Ok(order("pf"))),
        ("GET", "/fapi/v1/userTrades", Ok(trade)),
        ("GET", "/fapi/v1/allAlgoOrders", Ok(json!([]))),
    ]);
    let events = vike_binance::execution::perp_history_events_of(
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
            read("/fapi/v1/allOrders", "BTCUSDT"),
            read("/fapi/v1/userTrades", "BTCUSDT"),
            read("/fapi/v1/allAlgoOrders", "BTCUSDT"),
            read("/fapi/v1/allOrders", "ETHUSDT"),
            read("/fapi/v1/userTrades", "ETHUSDT"),
            read("/fapi/v1/allAlgoOrders", "ETHUSDT"),
        ]
    );
    assert_eq!(
        vike_bridge_core::execution_drive::event_tags(&events),
        vec!["Fill:pm:100", "OrderFilled:pm", "Fill:pf:ETHUSDT.P:100", "OrderFilled:pf"]
    );
}

// ---- the funding-rate history -------------------------------------------------------------------------

/// A spot symbol is refused before anything is asked, in binance's words.
#[test]
fn funding_refuses_a_spot_symbol_in_binances_words() {
    use vike_data::source::{FundingRateSource, SourceError};
    // a USER name, read off its kept row (decision 0290)
    vike_binance::instrument::names::kept_list_for_tests();
    let source = vike_binance::history::FundingRateSourceBinance::from_urls(
        &vike_venues::venue_urls::VenueUrls::seed("binance"),
    )
    .expect("the seed");
    match source.fetch("BTCUSDT", 0, 1) {
        Err(SourceError::Refused(why)) => assert_eq!(
            why.text(),
            "binance: \"BTCUSDT\" names SPOT, and funding is a perpetual's series — ask for \
             `BTCUSDT.P`, the store's spelling of the perpetual (USDⓈ-M or COIN-M)"
        ),
        Err(SourceError::Fetch(why)) => panic!("asked the venue: {why}"),
        Ok(points) => panic!("asked the venue: {} points", points.len()),
    }
}

/// **The venue's "this order client id is taken" is no refusal of the order: it is asked about by the id**
/// (decision 0303). Spot's `-2010` "Duplicate order sent." and USDⓈ-M's `-4116` "clientOrderId is
/// duplicated", the venues' documented words, on the single submit and on a batch element: the lookup by
/// the broker-stamped id finds the order resting under it and accepts it, no "submit rejected" line is
/// written, and a batch neighbour's real refusal stays one.
#[test]
fn a_duplicate_order_client_id_answer_is_asked_about_by_the_id_and_never_refused() {
    let client = spot(vec![
        ("POST", "/api/v3/order", refusal(-2010, "Duplicate order sent.")),
        ("GET", "/api/v3/order", Ok(json!({"orderId": 91, "status": "NEW"}))),
    ]);
    let (events, logs) = vike_log::capture::captured(|| client.submit_order(&limit("d1")));
    assert_eq!(events, vec![submitted("d1"), accepted("d1", "91")]);
    assert_eq!(binance_lines(&logs), Vec::<Line>::new());
    assert_eq!(
        client.transport.sent().last(),
        Some(&sent(
            "requery",
            "GET",
            &format!("{SPOT_HOST}/api/v3/order"),
            &[("symbol", "BTCUSDT"), ("origClientOrderId", "x-LINK-d1")]
        ))
    );

    let client = perp(vec![
        ("POST", "/fapi/v1/order", refusal(-4116, "clientOrderId is duplicated")),
        ("GET", "/fapi/v1/order", Ok(json!({"orderId": 92, "status": "NEW"}))),
    ]);
    assert_eq!(client.submit_order(&limit("d2")), vec![submitted("d2"), accepted("d2", "92")]);

    let batch_answer = json!([
        {"code": -4116, "msg": "clientOrderId is duplicated"},
        {"code": -2010, "msg": "no money"}
    ]);
    let client = perp(vec![
        ("POST", "/fapi/v1/batchOrders", Ok(batch_answer)),
        ("GET", "/fapi/v1/order", Ok(json!({"orderId": 93, "status": "NEW"}))),
    ]);
    assert_eq!(
        client.submit_batch(&[limit("d3"), limit("d4")]),
        vec![
            submitted("d3"),
            submitted("d4"),
            accepted("d3", "93"),
            rejected("d4", "no money", None)
        ]
    );
}
