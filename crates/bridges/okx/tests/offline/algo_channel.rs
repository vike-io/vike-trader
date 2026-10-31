//! **The ALGO socket** (`orders-algo`, on `/ws/v5/business`), replayed from frames CAPTURED on the demo
//! account on 2026-10-24 (`tests/fixtures/algo/`, the account id stripped): a STOP's own state, which the
//! private `orders` socket never carries (it carries only the child order a fired stop spawns, filed on
//! the stop by `triggered_stop.rs`). Without this socket a stop that the venue cancelled (its position
//! closed) or refused stays `ACCEPTED` in the node for good; the `canceled_by_venue` frame is exactly
//! that: OKX cancelled a reduce-only stop 1.1 s after the drill flattened its position, nothing of ours
//! asked it to, and v0.1.86 never heard.
//!
//! Measured on the socket: `live`, `effective`, `canceled` (ours and the venue's) and `order_failed`
//! (`failCode` `51169`, with no child order at all). What stays UNMEASURED: `partially_failed` /
//! `partially_effective` / `pause` (mapped to nothing), and the socket's reconnect and resync (the
//! mapper over the history rows is pinned below; the thread wiring is not run offline).

use serde_json::json;
use vike_exec::recon::ReconClient;
use vike_model::OrderStatus;
use vike_model::events::Event;
use vike_okx::event_mapper::{map_okx_algo_frame, map_okx_perp};
use vike_okx::history::map_okx_algo_history;
use vike_okx::recon_client::{name_algo_children, parse_algo_children, parse_fills};
use vike_okx::ws_auth::build_subscribe_frame;

use crate::triggered_stop::{
    CHILD_CLORDID, CHILD_ORDID, CT_VAL, STOP, SYMBOL, engine_with_resting_stop, fixture, fold,
    kinds,
};

#[test]
fn the_subscribe_frame_names_the_algo_channel() {
    assert_eq!(
        build_subscribe_frame("orders-algo", "SWAP"),
        json!({"op": "subscribe", "args": [{"channel": "orders-algo", "instType": "SWAP"}]})
    );
}

/// The state table, one captured frame per row of it.
#[test]
fn each_measured_stop_state_maps_to_its_event_under_the_stops_id() {
    let table = [
        ("live (the REST ack already accepted it)", fixture!("ws_orders_algo_live.json"), vec![]),
        (
            "effective (it fired)",
            fixture!("ws_orders_algo_effective.json"),
            vec![format!("OrderTriggered:{STOP}")],
        ),
        (
            "canceled by us (cancel-algos)",
            fixture!("ws_orders_algo_canceled_by_us.json"),
            vec!["OrderCanceled:b7okxtsd".to_string()],
        ),
        (
            "canceled by the venue (its position closed)",
            fixture!("ws_orders_algo_canceled_by_venue.json"),
            vec!["OrderCanceled:b7okxtse".to_string()],
        ),
        (
            "effective, its child refused",
            fixture!("ws_orders_algo_effective_child_refused.json"),
            vec!["OrderTriggered:b7okxtsi".to_string()],
        ),
    ];
    for (name, frame, want) in table {
        assert_eq!(kinds(&map_okx_algo_frame(&frame)), want, "{name}");
    }
}

/// What is not a stop row maps to nothing: the acks and notices of the socket, the private channel's
/// frames (a wrong socket must not double-book), a row with no `algoClOrdId` (a stop placed by hand on
/// the exchange), and a state word nobody measured.
#[test]
fn everything_else_on_the_socket_maps_to_nothing() {
    let ack = json!({"event": "subscribe", "arg": {"channel": "orders-algo", "instType": "SWAP"}});
    let notice = json!({"event": "notice", "code": "64010", "msg": "port retired"});
    assert!(map_okx_algo_frame(&ack).is_empty());
    assert!(map_okx_algo_frame(&notice).is_empty());
    assert!(map_okx_algo_frame(&fixture!("ws_orders_child_filled.json")).is_empty());
    let mut by_hand = fixture!("ws_orders_algo_effective.json");
    by_hand["data"][0]["algoClOrdId"] = json!("");
    assert!(map_okx_algo_frame(&by_hand).is_empty(), "not a stop of ours");
    for word in ["partially_failed", "partially_effective", "pause", "something_new"] {
        let mut row = fixture!("ws_orders_algo_effective.json");
        row["data"][0]["state"] = json!(word);
        assert!(map_okx_algo_frame(&row).is_empty(), "{word} is not mapped");
    }
}

/// The trigger FAILED (a reduce-only stop on a flat position fires and the venue refuses the order it
/// places: no child order ever reaches the private socket): the frame carries `failCode` `51169`, and
/// the stop is over.
#[test]
fn an_order_failed_stop_is_cancelled_with_the_venues_fail_code() {
    let events = map_okx_algo_frame(&fixture!("ws_orders_algo_order_failed.json"));
    assert_eq!(kinds(&events), ["OrderCanceled:b7okxtsk"]);
    let Event::OrderCanceled(canceled) = &events[0] else { panic!("a cancel") };
    assert!(canceled.reason.contains("51169"), "{}", canceled.reason);
}

/// ...and through the engine: the stop the node held `ACCEPTED` forever ends `CANCELED`.
#[test]
fn a_stop_whose_trigger_failed_ends_canceled() {
    let mut engine = engine_with_resting_stop();
    let mut frame = fixture!("ws_orders_algo_order_failed.json");
    frame["data"][0]["algoClOrdId"] = json!(STOP);
    fold(&mut engine, &map_okx_algo_frame(&frame));
    assert_eq!(engine.registry[STOP].status, OrderStatus::Canceled);
}

// ---- through the real engine ----------------------------------------------------------------------

/// **The measured order of arrival** (private `live`, private `filled`, then the algo socket's
/// `effective`, all within 20 ms): the stop ends FILLED, and the late `effective` lands on a filled
/// order, where the FSM drops it.
#[test]
fn the_algo_effective_after_the_childs_fill_changes_nothing() {
    let mut engine = engine_with_resting_stop();
    let mut events = map_okx_perp(&fixture!("ws_orders_child_live.json"), "okx", SYMBOL, CT_VAL);
    events.extend(map_okx_perp(&fixture!("ws_orders_child_filled.json"), "okx", SYMBOL, CT_VAL));
    events.extend(map_okx_algo_frame(&fixture!("ws_orders_algo_effective.json")));
    fold(&mut engine, &events);
    assert_eq!(engine.registry[STOP].status, OrderStatus::Filled);
}

/// The other order (the algo socket first): `effective` -> TRIGGERED, then the child's fill -> FILLED.
#[test]
fn the_algo_effective_before_the_childs_fill_triggers_then_fills() {
    let mut engine = engine_with_resting_stop();
    fold(&mut engine, &map_okx_algo_frame(&fixture!("ws_orders_algo_effective.json")));
    assert_eq!(engine.registry[STOP].status, OrderStatus::Triggered);
    let fill = map_okx_perp(&fixture!("ws_orders_child_filled.json"), "okx", SYMBOL, CT_VAL);
    fold(&mut engine, &fill);
    assert_eq!(engine.registry[STOP].status, OrderStatus::Filled);
}

/// **Defect 1, the venue-side cancel**: OKX cancelled the stop when its position closed; the node held
/// it `ACCEPTED` forever. The algo socket's `canceled` frame ends it.
#[test]
fn a_stop_the_venue_cancels_ends_canceled() {
    let mut engine = engine_with_resting_stop();
    // the frame names `b7okxtse`; rename it to the engine's stop, the row is otherwise as captured
    let mut frame = fixture!("ws_orders_algo_canceled_by_venue.json");
    frame["data"][0]["algoClOrdId"] = json!(STOP);
    fold(&mut engine, &map_okx_algo_frame(&frame));
    assert_eq!(engine.registry[STOP].status, OrderStatus::Canceled);
}

/// The stop's child refused with no fill (the venue cancelled the CHILD, the stop is `effective`): the
/// private socket's `canceled` child row ends the stop, and the algo socket's `effective` after it is
/// dropped.
#[test]
fn a_stop_whose_child_the_venue_cancels_ends_canceled() {
    let mut engine = engine_with_resting_stop();
    let mut child = fixture!("ws_orders_child_canceled.json");
    child["data"][0]["algoClOrdId"] = json!(STOP);
    let mut effective = fixture!("ws_orders_algo_effective_child_refused.json");
    effective["data"][0]["algoClOrdId"] = json!(STOP);
    let mut events = map_okx_perp(&child, "okx", SYMBOL, CT_VAL);
    events.extend(map_okx_algo_frame(&effective));
    fold(&mut engine, &events);
    assert_eq!(engine.registry[STOP].status, OrderStatus::Canceled);
}

// ---- the resync of the algo socket (a gap) and reconcile's join --------------------------------------

/// The reconnect replay reads the ended stops: `canceled` and `order_failed` replay, `effective` does
/// not (its child's fills do), and a stop of someone else's does not.
#[test]
fn the_algo_history_replays_only_the_stops_that_ended_without_a_fill() {
    let rows = fixture!("rest_orders_algo_history_mixed.json")["data"].clone();
    let events = map_okx_algo_history(&rows);
    assert_eq!(
        kinds(&events),
        ["OrderCanceled:b7okxstopg2", "OrderCanceled:b7okxtse", "OrderCanceled:b7okxtsd"],
        "order_failed, canceled, canceled; the `effective` b7okxtsb replays nothing"
    );
    assert!(map_okx_algo_history(&json!({"not": "an array"})).is_empty());
}

#[test]
fn the_fired_stop_row_names_its_child_and_the_fill_gets_the_stops_id() {
    let history = fixture!("rest_orders_algo_history_effective.json")["data"].to_string();
    let children = parse_algo_children(&history).expect("rows");
    assert_eq!(children.get(CHILD_ORDID).map(String::as_str), Some(STOP));
    assert_eq!(children.len(), 1, "ordIdList and ordId name the same child once");

    let fills_body = fixture!("rest_fills.json")["data"].to_string();
    let mut fills = parse_fills(&fills_body, CT_VAL).expect("fills");
    assert_eq!(fills[0].client_order_id.as_deref(), Some(CHILD_CLORDID), "as the venue files it");
    name_algo_children(&mut fills, &children);
    assert_eq!(fills[0].client_order_id.as_deref(), Some(STOP));
    assert_eq!(fills[1].client_order_id.as_deref(), Some("b7okxtsa"), "not a stop's child: kept");
}

/// A stop placed by hand (no `algoClOrdId`) names nothing; a body that is not an array is an error,
/// never a panic.
#[test]
fn a_stop_with_no_id_of_ours_names_no_child() {
    let mut rows = fixture!("rest_orders_algo_history_effective.json")["data"].clone();
    rows[0]["algoClOrdId"] = json!("");
    assert!(parse_algo_children(&rows.to_string()).expect("rows").is_empty());
    assert!(parse_algo_children("{}").is_err());
}

/// The claim is MADE (decision 0192's three edits, with `claim_restart.rs` and the conformance row
/// proving it): okx's recon client lists the stops that rest, so a stop it does not list is absent.
#[test]
fn the_recon_client_claims_trigger_listing() {
    use vike_bridge_core::signer::OkxV5Signer;
    use vike_secrets::credentials::Credentials;
    struct Never;
    impl vike_okx::transport::OkxTransport for Never {
        fn signed(
            &self,
            _: &str,
            _: &str,
            _: &str,
            _: &[(&str, serde_json::Value)],
            _: &OkxV5Signer,
        ) -> Result<serde_json::Value, vike_bridge_core::transport::VenueApiError> {
            unreachable!("the claim is a constant")
        }
        fn public(
            &self,
            _: &str,
            _: &str,
            _: &[(&str, String)],
        ) -> Result<serde_json::Value, vike_bridge_core::transport::VenueApiError> {
            unreachable!("the claim is a constant")
        }
    }
    let creds = Credentials {
        api_key: "k".to_string().into(),
        api_secret: "s".to_string().into(),
        passphrase: Some("p".to_string().into()),
    };
    let client = vike_okx::OkxReconClient::new(
        OkxV5Signer::new(&creds, || 0),
        Never,
        "https://unused.invalid",
        SYMBOL,
        CT_VAL,
    );
    assert!(client.lists_trigger_orders());
}
