//! **The Bybit claim `lists_trigger_orders` and what it rests on** (decision 0192; offline: no
//! network, nothing `#[ignore]`d). The claim is the restore planner's licence to read a restored STOP
//! the open-order report does not list as gone, once the quiet passes have counted; this suite pins
//! the prerequisites it stands on and the planner's verdicts for bybit's own rows.
//!
//! * the real client answers the claim, on the demo host and on mainnet;
//! * the one trigger kind bybit's capability row lets `submit` place is the stop, and the venue's
//!   recorded answer for it (`recon_paging.rs`'s `RECORDED_STOP_ROW`) is a LIVE `Stop` carrying the
//!   coid; what the adapter places on either side lists back as a `Stop` (the ownership file records
//!   `Stop` for it, and the harness asks the same of the parser);
//! * the planner, fed bybit's parsed rows: a listed stop is adopted; a stop that FIRED while the node
//!   was down is `filled_while_down` by its coid and never gone, whatever the quiet-pass count; silence
//!   alone makes a stop gone only after [`RESTORE_QUIET_PASSES`] counted passes AND only for the claim;
//!   a status word that ends the order is proof.
//!
//! What is NOT pinned here, because no offline test can: that the venue echoes `stopOrderType: Stop`
//! for a BUY-side conditional (only the SELL was recorded, on the demo host, 2026-10-24), and that a
//! fired stop's `execution/list` row carries the coid (measured on the demo account by the drill,
//! recorded in the commit message; the fill row below is the documented shape).

use serde_json::{Value, json};
use vike_bridge_core::signer::BybitV5Signer;
use vike_bridge_core::transport::VenueApiError;
use vike_bybit::recon_client::{parse_fills, parse_orders};
use vike_bybit::transport::BybitTransport;
use vike_exec::recon::{
    RESTORE_QUIET_PASSES, RestorePlan, RestoreScope, RestoredOrderRef, plan_restore,
};
use vike_model::{FillReport, OrderStatus, OrderStatusReport, OrderType};
use vike_secrets::credentials::Credentials;
use vike_venues::venue_caps::{TRIGGER_KINDS, caps_for};

use super::r6_bybit_parity::{bybit_perp, fixture};
use super::recon_paging::RECORDED_STOP_ROW;

const COID: &str = "b6bybitstop";

fn creds() -> Credentials {
    Credentials {
        api_key: "k".to_string().into(),
        api_secret: "s".to_string().into(),
        passphrase: None,
    }
}

/// The claim of the REAL client, built the way the daemon builds it.
fn claim(mainnet: bool) -> bool {
    vike_bybit::recon_client(&creds(), "BTCUSDT", mainnet)
        .expect("the factory builds offline")
        .lists_trigger_orders()
}

fn body(rows: Vec<Value>) -> String {
    json!({ "list": rows }).to_string()
}

/// The recorded demo answer for a resting stop, with `status` and `coid` swapped in.
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
        "symbol": "BTCUSDT", "execId": "e-fired", "orderId": "574d70ee-5014-4a90-afc9-aa47d3e5e42d",
        "orderLinkId": COID, "side": "Sell", "execQty": "0.001", "execPrice": "60690.5",
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
        coid: COID.to_string(),
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

fn coids(refs: &[RestoredOrderRef]) -> Vec<&str> {
    refs.iter().map(|r| r.coid.as_str()).collect()
}

#[test]
fn the_real_client_claims_on_the_demo_host_and_on_mainnet() {
    for mainnet in [false, true] {
        assert!(claim(mainnet), "mainnet {mainnet}: the client lists the conditional stops");
    }
}

/// The proof the conformance harness asks of bybit, run here against the venue's own recorded row:
/// every trigger kind the capability row lets `submit` place is a LIVE report of that kind carrying the
/// coid. The kinds are asserted as a list so that a new placeable trigger kind fails here until its own
/// row exists, instead of passing the claim unproven.
#[test]
fn every_trigger_kind_bybit_can_place_is_a_live_report_of_that_kind() {
    let kinds: Vec<OrderType> = caps_for("bybit")
        .supported_order_kinds
        .iter()
        .copied()
        .filter(|kind| TRIGGER_KINDS.contains(kind))
        .map(OrderType::from_stored)
        .collect();
    assert_eq!(kinds, [OrderType::Stop], "bybit places its stop as one conditional Market order");

    let reports = listed("Untriggered");
    let [report] = reports.as_slice() else { panic!("one row, one report: {reports:?}") };
    assert_eq!(report.client_order_id.as_deref(), Some(COID));
    assert!(!report.status.is_terminal(), "a listing, not a terminal report: {report:?}");
    assert_eq!(report.order_type, OrderType::Stop, "the kind the stop was placed as");
    assert_eq!(report.side, -1);
}

struct NoWire;

impl BybitTransport for NoWire {
    fn signed(
        &self,
        _base: &str,
        path: &str,
        _method: &str,
        _params: &[(&str, Value)],
        _signer: &BybitV5Signer,
    ) -> Result<Value, VenueApiError> {
        panic!("building an order sends nothing: {path}")
    }
}

/// What the adapter PLACES comes back from the venue as a `Stop`: `build_order_params`' stop, for a
/// SELL (closing a long, fires on a fall) and a BUY (closing a short, fires on a rise), echoed the way
/// the venue echoes a conditional (`orderType` and `triggerDirection` as sent, `stopOrderType` `Stop`,
/// `Untriggered`), parses to a `Stop` on the side that was placed. The echo of the word is the recorded
/// SELL's; the BUY's is the documented symmetry (see the module doc).
#[test]
fn a_stop_the_adapter_places_is_listed_back_as_a_stop_on_either_side() {
    let client = bybit_perp(&fixture(), NoWire);
    for side in [-1_i32, 1] {
        let request: vike_model::OrderRequest = serde_json::from_value(json!({
            "client_order_id": COID, "venue": "bybit", "symbol": "BTCUSDT", "side": side,
            "qty": 0.01, "order_type": "stop", "trigger_price": 58000.0, "reduce_only": true
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
        assert_eq!(report.order_type, OrderType::Stop, "side {side}: placed {params:?}");
        assert_eq!(report.status, OrderStatus::Accepted, "side {side}");
        assert_eq!(report.side, side, "side {side}");
        assert_eq!(report.client_order_id.as_deref(), Some(COID), "side {side}");
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
/// longer listed (it is not resting), and its market child's fill carries the coid, so the planner puts
/// it in `filled_while_down` (the `MissingFill` path owns it) at every quiet-pass count, claim or not.
#[test]
fn a_stop_that_fired_while_down_is_filled_never_gone() {
    let fills = fired_stop_fills();
    for claims in [false, true] {
        for quiet in [0, 1, RESTORE_QUIET_PASSES, RESTORE_QUIET_PASSES + 1] {
            let p = plan(claims, quiet, &[], &fills);
            assert_eq!(p.filled_while_down, [COID], "claim {claims}, quiet {quiet}: {p:?}");
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
        assert_eq!(coids(&p.unproven), [COID], "quiet {quiet}: {p:?}");
        assert!(p.gone.is_empty(), "quiet {quiet}: {p:?}");
    }
    let p = plan(claims, RESTORE_QUIET_PASSES, &[], &[]);
    assert_eq!(coids(&p.gone), [COID], "the claim turns counted silence into proof: {p:?}");

    let p = plan(false, RESTORE_QUIET_PASSES + 10, &[], &[]);
    assert_eq!(coids(&p.unproven), [COID], "without the claim silence never is: {p:?}");
    assert!(p.gone.is_empty(), "{p:?}");
}

/// **A status word that ends the order is proof, whatever the count.** `Deactivated` (a conditional
/// pulled before it fired), `Cancelled` and `Rejected` make the stop gone; `Filled` is a fill.
#[test]
fn a_stop_the_venue_reports_ended_is_proof() {
    for word in ["Deactivated", "Cancelled", "Rejected"] {
        let p = plan(claim(false), 0, &listed(word), &[]);
        assert_eq!(coids(&p.gone), [COID], "{word}: {p:?}");
        assert!(p.adopt.is_empty() && p.unproven.is_empty(), "{word}: {p:?}");
    }
    let p = plan(claim(false), 0, &listed("Filled"), &[]);
    assert_eq!(p.filled_while_down, [COID], "{p:?}");
    assert!(p.gone.is_empty(), "{p:?}");
}
