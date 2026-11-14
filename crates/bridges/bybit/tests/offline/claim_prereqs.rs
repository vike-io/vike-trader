//! **The Bybit claim `lists_trigger_orders` and what it rests on** (decision 0192; offline: no
//! network, nothing `#[ignore]`d). The claim is the restore planner's licence to read a restored STOP
//! the open-order report does not list as gone, once the quiet passes have counted; this suite pins
//! the prerequisites it stands on and the planner's verdicts for bybit's own rows.
//!
//! * the real client answers the claim, on the demo host and on mainnet;
//! * every trigger kind bybit's capability row lets `submit` place has a recorded venue answer that is a
//!   LIVE report of that kind carrying the order client id: the stop (`reconcile_paging.rs`'s `RECORDED_STOP_ROW`) and the
//!   stop-limit ([`RECORDED_STOP_LIMIT_ROWS`], both sides) and the take-profit ([`RECORDED_TAKE_PROFIT_ROWS`],
//!   a limit and a market, both sides), so a kind the row gains fails here until its
//!   own answer is recorded; what the adapter places on either side lists back as the kind it was placed
//!   as (the ownership file records that kind, and the harness asks the same of the parser);
//! * the planner, fed bybit's parsed rows: a listed stop is adopted; a stop that FIRED while the node
//!   was down is `filled_while_down` by its order client id and never gone, whatever the quiet-pass count; silence
//!   alone makes a stop gone only after [`RESTORE_QUIET_PASSES`] counted passes AND only for the claim;
//!   a status word that ends the order is proof.
//!
//! What is NOT pinned here, because no offline test can: that a fired stop's `execution/list` row carries
//! the order client id (measured on the demo account by the drill, recorded in the commit message; the fill row below
//! is the documented shape). The venue echoes `stopOrderType: Stop` for a BUY-side conditional too: the
//! stop-limit rows below were recorded on BOTH sides (demo host, 2026-11-08).

use serde_json::{Value, json};
use vike_bridge_core::signer::SignerJson;
use vike_bridge_core::transport::VenueApiError;
use vike_bridge_core::transport::json::RestTransportJson;
use vike_bybit::reconcile::{parse_fills, parse_orders};
use vike_execution::recon::{
    RESTORE_QUIET_PASSES, RestorePlan, RestoreScope, RestoredOrderRef, plan_restore,
};
use vike_model::{FillReport, OrderStatus, OrderStatusReport, OrderType};
use vike_secrets::credentials::Credentials;
use vike_venues::venue_caps::{TRIGGER_KINDS, capabilities_for};

use super::r6_bybit_parity::{bybit_perp, fixture};
use super::reconcile_paging::RECORDED_STOP_ROW;

const ORDER_CLIENT_ID: &str = "b6bybitstop";

/// The DEMO venue's `/v5/order/realtime` rows for the two resting stop-limits
/// `tests/bybit_smokes/bybit_stop_limit_smoke.rs` placed through `build_order_params` (2026-11-08): a SELL
/// firing on a fall and a BUY firing on a rise, each a conditional `Limit` with `stopOrderType` `Stop`.
const RECORDED_STOP_LIMIT_ROWS: [&str; 2] = [
    r#"{"avgPrice":"","blockTradeId":"","cancelType":"UNKNOWN","closeOnTrigger":false,"createType":"CreateByStopOrder","createdTime":"1794120446348","cumExecFee":"0","cumExecQty":"0","cumExecValue":"0","cumFeeDetail":{},"isLeverage":"","lastPriceOnCreated":"75782.6","leavesQty":"0.001","leavesValue":"73.5071","marketUnit":"","orderId":"eb17e403-279c-421a-ac08-8ced6f6ba186","orderIv":"","orderLinkId":"vtsls20446157","orderStatus":"Untriggered","orderType":"Limit","parentOrderLinkId":"","placeType":"","positionIdx":0,"price":"73507.1","qty":"0.001","reduceOnly":false,"rejectReason":"EC_NoError","side":"Sell","slLimitPrice":"0","slTriggerBy":"","smpGroup":"0","smpOrderId":"","smpType":"None","stopLoss":"","stopOrderType":"Stop","symbol":"BTCUSDT","takeProfit":"","timeInForce":"GTC","tpLimitPrice":"0","tpTriggerBy":"","tpslMode":"","triggerBy":"LastPrice","triggerDirection":2,"triggerPrice":"73509.1","updatedTime":"1794120446348"}"#,
    r#"{"avgPrice":"","blockTradeId":"","cancelType":"UNKNOWN","closeOnTrigger":false,"createType":"CreateByStopOrder","createdTime":"1794120447759","cumExecFee":"0","cumExecQty":"0","cumExecValue":"0","cumFeeDetail":{},"isLeverage":"","lastPriceOnCreated":"75782.7","leavesQty":"0.001","leavesValue":"78.0581","marketUnit":"","orderId":"f4bab509-01a3-401c-97b8-4321410c89b0","orderIv":"","orderLinkId":"vtslb20447670","orderStatus":"Untriggered","orderType":"Limit","parentOrderLinkId":"","placeType":"","positionIdx":0,"price":"78058.1","qty":"0.001","reduceOnly":false,"rejectReason":"EC_NoError","side":"Buy","slLimitPrice":"0","slTriggerBy":"","smpGroup":"0","smpOrderId":"","smpType":"None","stopLoss":"","stopOrderType":"Stop","symbol":"BTCUSDT","takeProfit":"","timeInForce":"GTC","tpLimitPrice":"0","tpTriggerBy":"","tpslMode":"","triggerBy":"LastPrice","triggerDirection":1,"triggerPrice":"78056.1","updatedTime":"1794120447759"}"#,
];

/// The DEMO venue's `/v5/order/realtime` rows for the four resting take-profits
/// `tests/bybit_smokes/bybit_take_profit_smoke.rs` placed (captured on the bybit DEMO 2026-11-08): a
/// `Limit` and a `Market` each way, a BUY firing on a fall and a SELL firing on a rise, every one echoed
/// with `stopOrderType` `Stop` (the profit direction is what makes it a take-profit).
const RECORDED_TAKE_PROFIT_ROWS: [&str; 4] = [
    r#"{"avgPrice":"","blockTradeId":"","cancelType":"UNKNOWN","closeOnTrigger":false,"createType":"CreateByStopOrder","createdTime":"1794160325892","cumExecFee":"0","cumExecQty":"0","cumExecValue":"0","cumFeeDetail":{},"isLeverage":"","lastPriceOnCreated":"75765.5","leavesQty":"0.001","leavesValue":"73.4945","marketUnit":"","orderId":"c9397a1c-2d7f-45a5-8e18-d02a6b24283f","orderIv":"","orderLinkId":"vttplb60325804","orderStatus":"Untriggered","orderType":"Limit","parentOrderLinkId":"","placeType":"","positionIdx":0,"price":"73494.5","qty":"0.001","reduceOnly":false,"rejectReason":"EC_NoError","side":"Buy","slLimitPrice":"0","slTriggerBy":"","smpGroup":"0","smpOrderId":"","smpType":"None","stopLoss":"","stopOrderType":"Stop","symbol":"BTCUSDT","takeProfit":"","timeInForce":"GTC","tpLimitPrice":"0","tpTriggerBy":"","tpslMode":"","triggerBy":"LastPrice","triggerDirection":2,"triggerPrice":"73492.5","updatedTime":"1794160325892"}"#,
    r#"{"avgPrice":"","blockTradeId":"","cancelType":"UNKNOWN","closeOnTrigger":false,"createType":"CreateByStopOrder","createdTime":"1794160327341","cumExecFee":"0","cumExecQty":"0","cumExecValue":"0","cumFeeDetail":{},"isLeverage":"","lastPriceOnCreated":"75767","leavesQty":"0.001","leavesValue":"78.0365","marketUnit":"","orderId":"6663ceec-ce4f-4b40-af18-da55c501a9e0","orderIv":"","orderLinkId":"vttpls60327253","orderStatus":"Untriggered","orderType":"Limit","parentOrderLinkId":"","placeType":"","positionIdx":0,"price":"78036.5","qty":"0.001","reduceOnly":false,"rejectReason":"EC_NoError","side":"Sell","slLimitPrice":"0","slTriggerBy":"","smpGroup":"0","smpOrderId":"","smpType":"None","stopLoss":"","stopOrderType":"Stop","symbol":"BTCUSDT","takeProfit":"","timeInForce":"GTC","tpLimitPrice":"0","tpTriggerBy":"","tpslMode":"","triggerBy":"LastPrice","triggerDirection":1,"triggerPrice":"78038.5","updatedTime":"1794160327341"}"#,
    r#"{"avgPrice":"","blockTradeId":"","cancelType":"UNKNOWN","closeOnTrigger":false,"createType":"CreateByStopOrder","createdTime":"1794160328790","cumExecFee":"0","cumExecQty":"0","cumExecValue":"0","cumFeeDetail":{},"isLeverage":"","lastPriceOnCreated":"75767","leavesQty":"0.001","leavesValue":"0","marketUnit":"","orderId":"104909f9-f211-441c-8ed5-716bd99f7f0b","orderIv":"","orderLinkId":"vttpmb60328703","orderStatus":"Untriggered","orderType":"Market","parentOrderLinkId":"","placeType":"","positionIdx":0,"price":"0","qty":"0.001","reduceOnly":false,"rejectReason":"EC_NoError","side":"Buy","slLimitPrice":"0","slTriggerBy":"","smpGroup":"0","smpOrderId":"","smpType":"None","stopLoss":"","stopOrderType":"Stop","symbol":"BTCUSDT","takeProfit":"","timeInForce":"IOC","tpLimitPrice":"0","tpTriggerBy":"","tpslMode":"","triggerBy":"LastPrice","triggerDirection":2,"triggerPrice":"73492.5","updatedTime":"1794160328790"}"#,
    r#"{"avgPrice":"","blockTradeId":"","cancelType":"UNKNOWN","closeOnTrigger":false,"createType":"CreateByStopOrder","createdTime":"1794160330186","cumExecFee":"0","cumExecQty":"0","cumExecValue":"0","cumFeeDetail":{},"isLeverage":"","lastPriceOnCreated":"75767","leavesQty":"0.001","leavesValue":"0","marketUnit":"","orderId":"068e90d7-c4bd-440f-9951-c1ce7643f486","orderIv":"","orderLinkId":"vttpms60330099","orderStatus":"Untriggered","orderType":"Market","parentOrderLinkId":"","placeType":"","positionIdx":0,"price":"0","qty":"0.001","reduceOnly":false,"rejectReason":"EC_NoError","side":"Sell","slLimitPrice":"0","slTriggerBy":"","smpGroup":"0","smpOrderId":"","smpType":"None","stopLoss":"","stopOrderType":"Stop","symbol":"BTCUSDT","takeProfit":"","timeInForce":"IOC","tpLimitPrice":"0","tpTriggerBy":"","tpslMode":"","triggerBy":"LastPrice","triggerDirection":1,"triggerPrice":"78038.5","updatedTime":"1794160330186"}"#,
];

/// The venue's recorded answers for one placeable trigger kind, or `None` when none is recorded.
fn recorded_rows(kind: &OrderType) -> Option<Vec<Value>> {
    let parse = |row: &str| serde_json::from_str::<Value>(row).expect("a recorded row parses");
    match kind {
        OrderType::Stop => Some(vec![parse(RECORDED_STOP_ROW)]),
        OrderType::StopLimit => Some(RECORDED_STOP_LIMIT_ROWS.iter().map(|r| parse(r)).collect()),
        OrderType::TakeProfit => Some(RECORDED_TAKE_PROFIT_ROWS.iter().map(|r| parse(r)).collect()),
        _ => None,
    }
}

/// The claim of the REAL client, built the way the daemon builds it.
fn claim(mainnet: bool) -> bool {
    let tier = if mainnet { vike_model::AccountTier::Live } else { vike_model::AccountTier::Demo };
    let host = vike_bybit::execution::rest::seed_endpoints(tier).rest;
    vike_bybit::recon_client(&Credentials::for_test("k", "s", None), "BTCUSDT", &host)
        .expect("the factory builds offline")
        .lists_trigger_orders()
}

fn body(rows: Vec<Value>) -> String {
    json!({ "list": rows }).to_string()
}

/// The recorded demo answer for a resting stop, with `status` and `order_client_id` swapped in.
fn recorded_stop(status: &str) -> Value {
    let mut row: Value = serde_json::from_str(RECORDED_STOP_ROW).expect("the recorded row parses");
    row["orderStatus"] = json!(status);
    row
}

fn listed(status: &str) -> Vec<OrderStatusReport> {
    parse_orders(&body(vec![recorded_stop(status)])).expect("a well-formed order/realtime body")
}

/// The `execution/list` row of the stop's market child filling (documented shape: `orderLinkId` and
/// `orderId` are the stop's own, a conditional keeps its ids when it fires).
fn fired_stop_fills() -> Vec<FillReport> {
    parse_fills(&body(vec![json!({
        "execType": "Trade", "symbol": "BTCUSDT", "execId": "e-fired", "orderId": "574d70ee-5014-4a90-afc9-aa47d3e5e42d",
        "orderLinkId": ORDER_CLIENT_ID, "side": "Sell", "execQty": "0.001", "execPrice": "60690.5",
        "execFee": "0.03", "feeCurrency": "USDT", "isMaker": false, "execTime": "1792812000000",
        "orderType": "Market", "stopOrderType": "Stop", "createType": "CreateByStopOrder"
    })]))
    .expect("a well-formed execution list")
}

/// Bybit's side of the restore for one pass: scope, a stop the ownership file holds (a trigger kind
/// records no venue id) and how many quiet passes have counted.
fn scope(lists_trigger_orders: bool) -> RestoreScope {
    RestoreScope {
        venue: "bybit".to_string(),
        account: None,
        symbols: vec!["BTCUSDT".to_string()],
        lists_trigger_orders,
    }
}

fn restored_stop(quiet_passes: u32) -> RestoredOrderRef {
    RestoredOrderRef {
        order_client_id: ORDER_CLIENT_ID.to_string(),
        venue: Some("bybit".to_string()),
        symbol: Some("BTCUSDT".to_string()),
        account: None,
        order_type: Some(OrderType::Stop),
        venue_order_id: None,
        quiet_passes,
    }
}

fn plan(
    claims: bool,
    quiet_passes: u32,
    orders: &[OrderStatusReport],
    fills: &[FillReport],
) -> RestorePlan {
    plan_restore(&scope(claims), &[restored_stop(quiet_passes)], orders, fills)
}

fn order_client_ids(refs: &[RestoredOrderRef]) -> Vec<&str> {
    refs.iter().map(|r| r.order_client_id.as_str()).collect()
}

#[test]
fn the_real_client_claims_on_the_demo_host_and_on_mainnet() {
    for mainnet in [false, true] {
        assert!(claim(mainnet), "mainnet {mainnet}: the client lists the conditional stops");
    }
}

/// The proof the conformance harness asks of bybit, run here against the venue's own recorded rows:
/// every trigger kind the capability row lets `submit` place is a LIVE report of that kind carrying the
/// order client id. A placeable trigger kind with no recorded row fails here, instead of passing the claim unproven.
#[test]
fn every_trigger_kind_bybit_can_place_is_a_live_report_of_that_kind() {
    let kinds: Vec<OrderType> = capabilities_for("bybit")
        .supported_order_kinds
        .iter()
        .copied()
        .filter(|kind| TRIGGER_KINDS.contains(kind))
        .map(OrderType::from_stored)
        .collect();
    assert!(kinds.contains(&OrderType::Stop), "bybit places its stop: {kinds:?}");
    for kind in &kinds {
        let rows = recorded_rows(kind).unwrap_or_else(|| {
            panic!("bybit places {kind}, but no venue answer for it is recorded")
        });
        for row in rows {
            let order_client_id =
                row["orderLinkId"].as_str().expect("a recorded order client id").to_string();
            let reports =
                parse_orders(&body(vec![row])).expect("a well-formed order/realtime body");
            let [report] = reports.as_slice() else { panic!("one row, one report: {reports:?}") };
            assert_eq!(report.order_client_id.as_deref(), Some(order_client_id.as_str()), "{kind}");
            assert!(!report.status.is_terminal(), "{kind}: a listing, not a terminal: {report:?}");
            assert_eq!(&report.order_type, kind, "the kind it was placed as: {report:?}");
        }
    }
}

/// The recorded stop-limit rows read back as the live `StopLimit` they were placed as, on the side placed,
/// whether or not the capability row lists the kind yet (the row flips after its demo smoke).
#[test]
fn the_recorded_stop_limits_list_back_as_live_stop_limits_on_either_side() {
    let rows = recorded_rows(&OrderType::StopLimit).expect("recorded");
    let reports = parse_orders(&body(rows)).expect("a well-formed order/realtime body");
    let sides: Vec<(i32, &OrderType, bool)> =
        reports.iter().map(|r| (r.side, &r.order_type, r.status.is_live())).collect();
    assert_eq!(sides, [(-1, &OrderType::StopLimit, true), (1, &OrderType::StopLimit, true)]);
}

struct NoWire;

impl RestTransportJson for NoWire {
    fn signed(
        &self,
        _base: &str,
        path: &str,
        _method: &str,
        _params: &[(&str, Value)],
        _signer: &dyn SignerJson,
    ) -> Result<Value, VenueApiError> {
        panic!("building an order sends nothing: {path}")
    }
}

/// What the adapter PLACES comes back from the venue as the kind it was placed as: `build_order_params`'
/// stop and stop-limit, for a SELL (closing a long, fires on a fall) and a BUY (closing a short, fires on a
/// rise), echoed the way the venue echoes a conditional (`orderType` and `triggerDirection` as sent,
/// `stopOrderType` `Stop`, `Untriggered`: recorded on both sides), parses to that kind on the side placed.
#[test]
fn a_stop_the_adapter_places_is_listed_back_as_a_stop_on_either_side() {
    let client = bybit_perp(&fixture(), NoWire);
    for (side, word, kind) in [-1_i32, 1].into_iter().flat_map(|side| {
        [(side, "stop", OrderType::Stop), (side, "stop_limit", OrderType::StopLimit)]
    }) {
        let request: vike_model::OrderRequest = serde_json::from_value(json!({
            "order_client_id": ORDER_CLIENT_ID, "venue": "bybit", "symbol": "BTCUSDT", "side": side,
            "qty": 0.01, "order_type": word, "trigger_price": 58000.0, "price": 57990.0,
            "reduce_only": true
        }))
        .unwrap();
        let params = client.build_order_params(&request);
        let sent = |k: &str| {
            params.iter().find(|(pk, _)| *pk == k).map(|(_, v)| v.clone()).unwrap_or(Value::Null)
        };
        let echo = json!({
            "symbol": sent("symbol"), "orderId": "9001", "orderLinkId": sent("orderLinkId"),
            "side": sent("side"), "orderType": sent("orderType"), "stopOrderType": "Stop",
            "triggerDirection": sent("triggerDirection"), "triggerPrice": sent("triggerPrice"),
            "reduceOnly": sent("reduceOnly"), "qty": sent("qty"), "cumExecQty": "0",
            "avgPrice": "", "orderStatus": "Untriggered", "updatedTime": "1"
        });
        let reports = parse_orders(&body(vec![echo])).expect("a well-formed order/realtime body");
        let [report] = reports.as_slice() else { panic!("side {side}: {reports:?}") };
        assert_eq!(report.order_type, kind, "side {side}: placed {params:?}");
        assert_eq!(report.status, OrderStatus::Accepted, "side {side}");
        assert_eq!(report.side, side, "side {side}");
        assert_eq!(report.order_client_id.as_deref(), Some(ORDER_CLIENT_ID), "side {side}");
    }
}

/// What the adapter places as a `take_profit` (the same conditional firing the PROFIT way: a BUY on a fall, a
/// SELL on a rise; a `Limit` with a price, a `Market` without), echoed in the shape the venue echoes a
/// conditional (`stopOrderType` `Stop`, `triggerDirection` as sent: documented, not yet recorded for this
/// kind), lists back as a live `TakeProfit` on the side placed, carrying the order client id. The recorded row the claim
/// needs (`recorded_rows`) comes from `bybit_take_profit_smoke`'s `CAPTURED` line before the row lists the kind.
#[test]
fn a_take_profit_the_adapter_places_is_listed_back_as_a_take_profit_on_either_side() {
    let client = bybit_perp(&fixture(), NoWire);
    for side in [-1_i32, 1] {
        for price in [Some(57990.0), None] {
            let request: vike_model::OrderRequest = serde_json::from_value(json!({
                "order_client_id": ORDER_CLIENT_ID, "venue": "bybit", "symbol": "BTCUSDT", "side": side,
                "qty": 0.01, "order_type": "take_profit", "trigger_price": 58000.0, "price": price
            }))
            .unwrap();
            let params = client.build_order_params(&request);
            let sent = |k: &str| {
                params
                    .iter()
                    .find(|(pk, _)| *pk == k)
                    .map(|(_, v)| v.clone())
                    .unwrap_or(Value::Null)
            };
            let echo = json!({
                "symbol": sent("symbol"), "orderId": "9002", "orderLinkId": sent("orderLinkId"),
                "side": sent("side"), "orderType": sent("orderType"), "stopOrderType": "Stop",
                "triggerDirection": sent("triggerDirection"), "triggerPrice": sent("triggerPrice"),
                "reduceOnly": sent("reduceOnly"), "qty": sent("qty"), "cumExecQty": "0",
                "avgPrice": "", "orderStatus": "Untriggered", "updatedTime": "1"
            });
            let reports =
                parse_orders(&body(vec![echo])).expect("a well-formed order/realtime body");
            let [report] = reports.as_slice() else { panic!("side {side}: {reports:?}") };
            assert_eq!(
                report.order_type,
                OrderType::TakeProfit,
                "side {side} {price:?}: {params:?}"
            );
            assert!(report.status.is_live(), "side {side} {price:?}");
            assert_eq!(report.side, side, "side {side} {price:?}");
            assert_eq!(
                report.order_client_id.as_deref(),
                Some(ORDER_CLIENT_ID),
                "side {side} {price:?}"
            );
        }
    }
}

#[test]
fn a_listed_stop_is_adopted_not_gone() {
    let orders = listed("Untriggered");
    let p = plan(claim(false), RESTORE_QUIET_PASSES, &orders, &[]);
    assert_eq!(p.adopt.len(), 1, "{p:?}");
    assert!(p.gone.is_empty() && p.unproven.is_empty() && p.filled_while_down.is_empty(), "{p:?}");
    // …and the word for a stop that just fired, still being created, is live too.
    let p = plan(claim(false), 0, &listed("Triggered"), &[]);
    assert_eq!(p.adopt.len(), 1, "a Triggered row is live: {p:?}");
}

/// **A stop that fired while the node was down is never declared gone by silence.** The stop is no
/// longer listed (it is not resting), and its market child's fill carries the order client id, so the planner puts
/// it in `filled_while_down` (the `MissingFill` path owns it) at every quiet-pass count, claim or not.
#[test]
fn a_stop_that_fired_while_down_is_filled_never_gone() {
    let fills = fired_stop_fills();
    for claims in [false, true] {
        for quiet in [0, 1, RESTORE_QUIET_PASSES, RESTORE_QUIET_PASSES + 1] {
            let p = plan(claims, quiet, &[], &fills);
            assert_eq!(
                p.filled_while_down,
                [ORDER_CLIENT_ID],
                "claim {claims}, quiet {quiet}: {p:?}"
            );
            assert!(
                p.gone.is_empty() && p.unproven.is_empty(),
                "claim {claims}, quiet {quiet}: {p:?}"
            );
        }
    }
}

/// **Silence is proof only for the claim, and only after the quiet passes.** With the real client's
/// claim, a stop the report does not list is held (`unproven`) until `RESTORE_QUIET_PASSES` passes have
/// counted, then gone; without the claim (the answer before this change) it is held for good.
#[test]
fn silence_makes_a_stop_gone_only_after_the_quiet_passes_and_only_for_the_claim() {
    let claims = claim(false);
    for quiet in 0..RESTORE_QUIET_PASSES {
        let p = plan(claims, quiet, &[], &[]);
        assert_eq!(order_client_ids(&p.unproven), [ORDER_CLIENT_ID], "quiet {quiet}: {p:?}");
        assert!(p.gone.is_empty(), "quiet {quiet}: {p:?}");
    }
    let p = plan(claims, RESTORE_QUIET_PASSES, &[], &[]);
    assert_eq!(
        order_client_ids(&p.gone),
        [ORDER_CLIENT_ID],
        "the claim turns counted silence into proof: {p:?}"
    );

    let p = plan(false, RESTORE_QUIET_PASSES + 10, &[], &[]);
    assert_eq!(
        order_client_ids(&p.unproven),
        [ORDER_CLIENT_ID],
        "without the claim silence never is: {p:?}"
    );
    assert!(p.gone.is_empty(), "{p:?}");
}

/// **A status word that ends the order is proof, whatever the count.** `Deactivated` (a conditional
/// pulled before it fired), `Cancelled` and `Rejected` make the stop gone; `Filled` is a fill.
#[test]
fn a_stop_the_venue_reports_ended_is_proof() {
    for word in ["Deactivated", "Cancelled", "Rejected"] {
        let p = plan(claim(false), 0, &listed(word), &[]);
        assert_eq!(order_client_ids(&p.gone), [ORDER_CLIENT_ID], "{word}: {p:?}");
        assert!(p.adopt.is_empty() && p.unproven.is_empty(), "{word}: {p:?}");
    }
    let p = plan(claim(false), 0, &listed("Filled"), &[]);
    assert_eq!(p.filled_while_down, [ORDER_CLIENT_ID], "{p:?}");
    assert!(p.gone.is_empty(), "{p:?}");
}
