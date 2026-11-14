//! A STOP the venue ends or fires on its own reaches the node (known-limitations "deribit (stops)",
//! A27): before, the user-data pump subscribed `user.trades` only, so a stop cancelled by someone
//! else, or fired into an order the venue then refused, left the engine's order `Accepted` for good.
//! Now the pump also subscribes `user.orders.any.any.raw` and `map_deribit_private` reads its rows.
//!
//! ⚠ **The row shapes below are the DOCUMENTED ones, not captures.** No `user.orders` frame had been
//! recorded when this was written (the instrument and trade rows in `native_stops` are testnet
//! captures; these are built from the same order object, with the fields the mapper reads:
//! `label`, `order_type`, `order_state`, `triggered`, `trigger_order_id`, `cancel_reason`,
//! `reject_reason`, `last_update_timestamp`). the 0.1.97 kit's Deribit stop drill (B10) records the raw
//! frames, and a capture that differs is a fixture to replace here, not a reason to loosen a test.
//! The first captures are in: [`FIRED_STOP_FRAME`] and [`REFUSED_FIRED_STOP_FRAME`] (testnet, 2026-11-08).
//!
//! Every test goes through a PUBLIC entry that existed before the change (`map_deribit_private`,
//! `map_deribit_history`, the real pump over the local venue), so the file compiles against the old
//! tree and the tests that describe the new behaviour fail there.

use std::sync::Arc;
use std::time::{Duration, Instant};

use serde_json::{Value, json};
use vike_bridge_core::exec_actor::submit_fence::SubmitFence;
use vike_deribit::execution::event_mapper::map_deribit_private;
use vike_deribit::execution::resync::map_deribit_history;
use vike_deribit::execution::user_data::spawn_deribit_user_data_with_resync;
use vike_execution::{Ingest, event_channel};
use vike_model::events::{Event, OrderAccepted, OrderSubmitted};
use vike_model::{OrderRecord, OrderStatus};

use crate::fake_deribit_websocket::{FakeDeribit, SYMBOL, Session};

const ORDER_CLIENT_ID: &str = "n0stp71";

/// One `user.orders.any.any.raw` push carrying `data` (an order object, or a list of them).
fn orders_frame(data: Value) -> Value {
    json!({"jsonrpc": "2.0", "method": "subscription", "params": {
        "channel": "user.orders.any.any.raw", "data": data}})
}

/// A held reduce-only stop, as `native_stops::held_stop_row` has it from the open-order read.
fn stop_row(order_state: &str, triggered: bool) -> Value {
    json!({
        "amount": 10.0, "api": true, "direction": "sell", "instrument_name": SYMBOL,
        "label": ORDER_CLIENT_ID, "last_update_timestamp": 1793016922999i64, "order_id": "SLTS-11047216",
        "order_state": order_state, "order_type": "stop_market", "price": "market_price",
        "reduce_only": true, "trigger": "last_price", "trigger_price": 80274.5,
        "triggered": triggered,
    })
}

/// The order a FIRED stop placed: a market order carrying the stop's label and naming the stop.
fn child_row(order_state: &str, extra: Value) -> Value {
    let mut row = json!({
        "amount": 10.0, "api": true, "direction": "sell", "instrument_name": SYMBOL,
        "label": ORDER_CLIENT_ID, "last_update_timestamp": 1793016943200i64, "order_id": "124740642635",
        "order_state": order_state, "order_type": "market", "reduce_only": true,
        "trigger_order_id": "SLTS-11047216", "triggered": true,
    });
    for (k, v) in extra.as_object().expect("an object").iter() {
        row[k] = v.clone();
    }
    row
}

fn map(frame: &Value) -> Vec<Event> {
    map_deribit_private(frame, "deribit", SYMBOL)
}

/// What an event is, with its order client id: `Canceled:<order client id>` and so on.
fn kinds(events: &[Event]) -> Vec<String> {
    events
        .iter()
        .map(|e| match e {
            Event::OrderCanceled(c) => format!("Canceled:{}", c.order_client_id),
            Event::OrderTriggered(t) => format!("Triggered:{}", t.order_client_id),
            Event::OrderRejected(r) => format!("Rejected:{}", r.order_client_id),
            other => format!("other:{other:?}"),
        })
        .collect()
}

fn canceled_reason(events: &[Event]) -> String {
    match events {
        [Event::OrderCanceled(c)] => c.reason.to_string(),
        other => panic!("one OrderCanceled wanted, got {other:?}"),
    }
}

/// THE BUG. A stop cancelled while it waited (by hand, in the venue's UI) is a `cancelled` row of the
/// stop itself; the measured venue keeps no trace of it anywhere else, so this row is the only word.
#[test]
fn a_stop_cancelled_at_the_venue_ends_the_order() {
    let mut row = stop_row("cancelled", false);
    row["cancel_reason"] = json!("user_request");
    let events = map(&orders_frame(row));
    assert_eq!(kinds(&events), [format!("Canceled:{ORDER_CLIENT_ID}")]);
    let reason = canceled_reason(&events);
    assert!(reason.contains("cancelled") && reason.contains("user_request"), "{reason}");
    if let Event::OrderCanceled(c) = &events[0] {
        assert_eq!(c.timestamp, 1793016922999, "the venue's own update time");
    }
}

/// A list of rows (the `100ms` / `agg2` intervals) maps like one object, row by row.
#[test]
fn a_list_of_rows_and_a_single_object_both_map() {
    let cancelled = stop_row("cancelled", false);
    let mut other = stop_row("cancelled", false);
    other["label"] = json!("n0stp72");
    let events = map(&orders_frame(json!([cancelled, other])));
    assert_eq!(
        kinds(&events),
        [format!("Canceled:{ORDER_CLIENT_ID}"), "Canceled:n0stp72".to_string()]
    );
}

/// A reduce-only stop that fires on a FLAT position: the venue refuses the order the trigger placed.
/// The stop was accepted long ago, so the end is `OrderCanceled` (the FSM refuses a reject after the
/// accept: see `a_reject_cannot_end_an_accepted_order`), carrying the venue's reason.
#[test]
fn the_order_a_fired_stop_placed_being_refused_ends_the_stop() {
    let row = child_row("rejected", json!({"reject_reason": "reduce_only"}));
    let events = map(&orders_frame(row));
    assert_eq!(kinds(&events), [format!("Canceled:{ORDER_CLIENT_ID}")]);
    let reason = canceled_reason(&events);
    assert!(reason.contains("rejected") && reason.contains("reduce_only"), "{reason}");
}

/// A stop-limit's limit leg RESTS under the stop's label after the trigger; cancelled elsewhere, that
/// ends the stop's order. The same goes for a trigger row refused outright.
#[test]
fn a_cancelled_limit_leg_and_a_refused_stop_row_end_the_order() {
    let leg =
        child_row("cancelled", json!({"order_type": "limit", "cancel_reason": "user_request"}));
    assert_eq!(kinds(&map(&orders_frame(leg))), [format!("Canceled:{ORDER_CLIENT_ID}")]);
    let mut refused = stop_row("rejected", false);
    refused["reject_reason"] = json!("post_only_reject");
    assert_eq!(kinds(&map(&orders_frame(refused))), [format!("Canceled:{ORDER_CLIENT_ID}")]);
}

/// The firing is announced: the stop's row says `triggered` (the state word), or `triggered: true` on a
/// row that has not ended. The fill, if any, books on the stop by `label` (`native_stops`).
#[test]
fn a_stop_that_fires_is_announced() {
    assert_eq!(
        kinds(&map(&orders_frame(stop_row("triggered", true)))),
        [format!("Triggered:{ORDER_CLIENT_ID}")]
    );
    assert_eq!(
        kinds(&map(&orders_frame(stop_row("open", true)))),
        [format!("Triggered:{ORDER_CLIENT_ID}")]
    );
}

/// WHAT MUST NOT END AN ORDER. Each of these is a row that is not the end of a stop; mapping any of
/// them to a terminal would cancel an order that is alive or about to fill.
#[test]
fn rows_that_are_not_the_end_of_a_stop_map_to_nothing() {
    // the stop waiting (its accept is the submit reply's), and the executed child (the fill is
    // `user.trades`'s)
    assert!(map(&orders_frame(stop_row("untriggered", false))).is_empty());
    assert!(map(&orders_frame(child_row("filled", json!({"filled_amount": 10.0})))).is_empty());
    assert!(map(&orders_frame(child_row("open", json!({"order_type": "limit"})))).is_empty());
    // a stop's own row closing as it FIRES (`triggered: true`) is not a cancel: if the venue does
    // that, a terminal here would cancel the order before its fill and the fill's `OrderFilled`
    // would be refused (UNMEASURED which way the venue goes: the drill records it)
    assert!(map(&orders_frame(stop_row("cancelled", true))).is_empty());
    // an ordinary order: its accept, cancel and reject are the order client's synchronous answers
    // and the A3 replay's, so its rows are not this mapper's
    let mut limit = stop_row("cancelled", false);
    limit["order_type"] = json!("limit");
    limit["cancel_reason"] = json!("user_request");
    assert!(map(&orders_frame(limit)).is_empty());
    // an order nobody labelled is not ours to end
    let mut foreign = stop_row("cancelled", false);
    foreign["label"] = json!("");
    assert!(map(&orders_frame(foreign)).is_empty());
    // not a user.orders channel / not a notification / not rows
    let mut trades = orders_frame(stop_row("cancelled", false));
    trades["params"]["channel"] = json!("user.portfolio.btc");
    assert!(map(&trades).is_empty());
    assert!(map(&orders_frame(json!("junk"))).is_empty());
    assert!(map(&orders_frame(json!(null))).is_empty());
    assert!(
        map(&json!({"jsonrpc": "2.0", "id": 3, "result": ["user.orders.any.any.raw"]})).is_empty()
    );
}

/// A limit the venue cancelled because an edit would have crossed ends the order: when the edit's reply is lost it is
/// the only word the node gets. The row shape is the documented one (`edit_post_only_reject`), not a capture.
#[test]
fn a_limit_cancelled_for_a_refused_edit_ends_the_order() {
    let mut row = stop_row("cancelled", false);
    row["order_type"] = json!("limit");
    row["cancel_reason"] = json!("edit_post_only_reject");
    let events = map(&orders_frame(row));
    assert_eq!(kinds(&events), [format!("Canceled:{ORDER_CLIENT_ID}")]);
    assert_eq!(canceled_reason(&events), "deribit cancelled the order: edit_post_only_reject");
}

/// CAPTURED on the testnet 2026-11-08 (`deribit_native_stops_on_the_testnet` step 5): a reduce-only
/// stop on a FLAT position fires; its own row says `triggered` and names ITSELF in `trigger_order_id`...
const FIRED_STOP_FRAME: &str = r#"{"jsonrpc":"2.0","method":"subscription","params":{"channel":"user.orders.any.any.raw","data":{"label":"n0stp0630821","price":"market_price","user_id":84789,"direction":"sell","time_in_force":"good_til_cancelled","instrument_name":"BTC-PERPETUAL","web":false,"api":true,"amount":10.0,"order_id":"SLTS-11058771","creation_timestamp":1794160551026,"filled_amount":0.0,"average_price":0.0,"trigger":"last_price","trigger_price":7.57e4,"mmp":false,"post_only":false,"replaced":false,"reduce_only":true,"trigger_offset":null,"triggered":true,"last_update_timestamp":1794160551026,"trigger_order_id":"SLTS-11058771","contracts":1.0,"is_liquidation":false,"order_state":"triggered","order_type":"stop_market","risk_reducing":false}}}"#;
/// ...and 6 ms later the venue refuses the order it would place, in ONE state word and no `reject_reason`.
const REFUSED_FIRED_STOP_FRAME: &str = r#"{"jsonrpc":"2.0","method":"subscription","params":{"channel":"user.orders.any.any.raw","data":{"label":"n0stp0630821","price":"market_price","user_id":84789,"direction":"sell","time_in_force":"good_til_cancelled","instrument_name":"BTC-PERPETUAL","web":false,"api":true,"amount":10.0,"order_id":"SLTS-11058771","creation_timestamp":1794160551026,"filled_amount":0.0,"average_price":0.0,"trigger":"last_price","trigger_price":7.57e4,"mmp":false,"post_only":false,"replaced":false,"reduce_only":true,"trigger_offset":null,"triggered":true,"last_update_timestamp":1794160551026,"trigger_order_id":"SLTS-11058771","contracts":1.0,"is_liquidation":false,"order_state":"rejected:invalid_reduce_only_order","order_type":"stop_market","risk_reducing":false}}}"#;
const CAPTURED_ORDER_CLIENT_ID: &str = "n0stp0630821";

/// THE BUG the capture found: `rejected:<reason>` read as no end at all, so the node kept the fired stop
/// `Triggered` for good. Now it ends, in the venue's word, live and in the replay, for every trigger
/// kind the bridge places; the documented `rejected` + `reject_reason` shape reads as before.
#[test]
fn a_fired_stop_the_venue_refused_in_one_state_word_ends_the_order() {
    let parse = |raw: &str| -> Value { serde_json::from_str(raw).expect("a captured frame") };
    let (fired, refused) = (map(&parse(FIRED_STOP_FRAME)), map(&parse(REFUSED_FIRED_STOP_FRAME)));
    assert_eq!(kinds(&fired), [format!("Triggered:{CAPTURED_ORDER_CLIENT_ID}")]);
    assert_eq!(kinds(&refused), [format!("Canceled:{CAPTURED_ORDER_CLIENT_ID}")]);
    assert_eq!(
        canceled_reason(&refused),
        "deribit rejected the order a stop fired: invalid_reduce_only_order"
    );
    let mut mo = accepted_stop_of(CAPTURED_ORDER_CLIENT_ID);
    assert_eq!(fold_all(&mut mo, &fired), [true]);
    assert_eq!(fold_all(&mut mo, &refused), [true]);
    assert_eq!(mo.status, OrderStatus::Canceled);

    let row = parse(REFUSED_FIRED_STOP_FRAME)["params"]["data"].clone();
    for kind in ["stop_market", "stop_limit", "take_market", "take_limit", "trailing_stop"] {
        let mut row = row.clone();
        row["order_type"] = json!(kind);
        let want = [format!("Canceled:{CAPTURED_ORDER_CLIENT_ID}")];
        assert_eq!(kinds(&map(&orders_frame(row.clone()))), want, "live, {kind}");
        assert_eq!(kinds(&replay(json!([row]))), want, "replay, {kind}");
    }

    let mut both = row;
    both["reject_reason"] = json!("reduce_only");
    assert_eq!(
        canceled_reason(&map(&orders_frame(both))),
        "deribit rejected the order a stop fired: reduce_only",
        "a reject_reason wins over the state word's"
    );
    let plain = child_row("rejected", json!({"reject_reason": "reduce_only"}));
    assert_eq!(
        canceled_reason(&map(&orders_frame(plain))),
        "deribit rejected the order a stop fired: reduce_only"
    );
}

/// An order the way the node holds a stop: Submitted, then the venue's `untriggered` answer.
fn accepted_stop() -> OrderRecord {
    accepted_stop_of(ORDER_CLIENT_ID)
}

fn accepted_stop_of(order_client_id: &str) -> OrderRecord {
    let mut mo = OrderRecord::new(crate::sell_stop(order_client_id, 80274.5, 1));
    mo.apply(&Event::OrderSubmitted(OrderSubmitted {
        order_client_id: order_client_id.into(),
        timestamp: 1,
    }))
    .expect("submitted");
    mo.apply(&Event::OrderAccepted(OrderAccepted {
        order_client_id: order_client_id.into(),
        venue_order_id: Some("SLTS-11047216".into()),
        timestamp: 2,
    }))
    .expect("accepted");
    assert_eq!(mo.status, OrderStatus::Accepted);
    mo
}

fn fold_all(mo: &mut OrderRecord, events: &[Event]) -> Vec<bool> {
    events.iter().map(|e| mo.apply(e).is_ok()).collect()
}

/// The row's events end a REAL order in the REAL order FSM, the case that fires first ends it, and
/// the same fact arriving twice -- the live row and the A3 replay of the same order (it shows in the
/// history when it was the child that ended) -- is applied ONCE: the second terminal is refused by the
/// FSM, which is the dedup this mapper relies on (the gatewayhub's `OnceMemory` keeps the `recent`
/// ring to one line).
#[test]
fn the_live_row_and_the_replay_of_one_end_fold_once() {
    let live = map(&orders_frame(child_row("cancelled", json!({"cancel_reason": "user_request"}))));
    let order_history = json!([child_row("cancelled", json!({}))]);
    let replay = map_deribit_history(&order_history, &json!([]), "deribit", SYMBOL);
    assert_eq!(kinds(&live), [format!("Canceled:{ORDER_CLIENT_ID}")]);
    assert_eq!(kinds(&replay), [format!("Canceled:{ORDER_CLIENT_ID}")]);

    let mut mo = accepted_stop();
    assert_eq!(fold_all(&mut mo, &live), [true]);
    assert_eq!(mo.status, OrderStatus::Canceled);
    assert_eq!(fold_all(&mut mo, &replay), [false], "the same end, a second time, is refused");
    assert_eq!(mo.status, OrderStatus::Canceled);

    // and the other order of arrival
    let mut mo = accepted_stop();
    assert_eq!(fold_all(&mut mo, &replay), [true]);
    assert_eq!(fold_all(&mut mo, &live), [false]);
}

/// WHY THE END IS `OrderCanceled` AND NOT `OrderRejected`: the FSM allows a reject only before the
/// accept, and a stop is always accepted first. A reject here would be refused and the stop would stay
/// `Accepted`, which is the bug.
#[test]
fn a_reject_cannot_end_an_accepted_order() {
    let mut mo = accepted_stop();
    let rejected = Event::OrderRejected(vike_model::events::OrderRejected {
        order_client_id: ORDER_CLIENT_ID.into(),
        reason: "refused".into(),
        timestamp: 3,
        cause: None,
    });
    assert!(mo.apply(&rejected).is_err());
    assert_eq!(mo.status, OrderStatus::Accepted);

    // a stop that FIRED and then ended without a fill: Triggered, then the refusal of its child
    let mut mo = accepted_stop();
    let refusal =
        map(&orders_frame(child_row("rejected", json!({"reject_reason": "reduce_only"}))));
    let fired = map(&orders_frame(stop_row("triggered", true)));
    assert_eq!(fold_all(&mut mo, &fired), [true]);
    assert_eq!(mo.status, OrderStatus::Triggered);
    assert_eq!(fold_all(&mut mo, &refusal), [true]);
    assert_eq!(mo.status, OrderStatus::Canceled);
}

/// The A3 replay says the same word as the live row for the order a stop fired: a REJECTED child in the
/// order history ends the stop (`OrderCanceled`); a rejected order that is no stop's child is still
/// the `OrderRejected` it always was.
#[test]
fn the_replay_ends_a_stop_whose_child_was_refused_and_keeps_every_other_reject() {
    let child = child_row("rejected", json!({"reject_reason": "reduce_only"}));
    let events = map_deribit_history(&json!([child]), &json!([]), "deribit", SYMBOL);
    assert_eq!(kinds(&events), [format!("Canceled:{ORDER_CLIENT_ID}")]);

    let plain = json!({"label": "c_rej", "order_id": "o11", "order_state": "rejected",
                       "last_update_timestamp": 9});
    let events = map_deribit_history(&json!([plain]), &json!([]), "deribit", SYMBOL);
    assert_eq!(kinds(&events), ["Rejected:c_rej".to_string()]);
}

/// A limit's row: `extra` names what it adds (a cancel reason, a `triggered` word).
fn limit_row(order_state: &str, extra: Value) -> Value {
    let mut row = stop_row(order_state, false);
    row["order_type"] = json!("limit");
    for (k, v) in extra.as_object().expect("an object").iter() {
        row[k] = v.clone();
    }
    row
}

fn replay(rows: Value) -> Vec<Event> {
    map_deribit_history(&rows, &json!([]), "deribit", SYMBOL)
}

/// THE ROW-BY-READER TABLE: one reading of an order row (`vike_deribit`'s `event_mapper`), so the live
/// `user.orders` stream and the A3 replay differ only where a reader has its own job: the live stream
/// leaves an ordinary order's end to the order client, and the replay does not announce a firing.
#[test]
fn the_live_stream_and_the_replay_read_one_row_one_way() {
    const EDIT: &str = "edit_post_only_reject";
    let fired = json!({"triggered": true});
    let cells = [
        ("a stop waiting", stop_row("untriggered", false), None, None),
        ("a stop fired, its row open", stop_row("open", true), Some("Triggered"), None),
        ("a stop fired, its row triggered", stop_row("triggered", true), Some("Triggered"), None),
        (
            "a stop cancelled waiting",
            stop_row("cancelled", false),
            Some("Canceled"),
            Some("Canceled"),
        ),
        // A reject is refused by the FSM after the accept, so this stop would rest on the node.
        ("a stop refused", stop_row("rejected", false), Some("Canceled"), Some("Canceled")),
        // A cancel here would make the fill's `OrderFilled` refused.
        ("a stop's row closing as it fires", stop_row("cancelled", true), None, None),
        ("a stop's row refused as it fires", stop_row("rejected", true), None, None),
        (
            "a fired child cancelled",
            child_row("cancelled", json!({})),
            Some("Canceled"),
            Some("Canceled"),
        ),
        (
            "a fired child refused",
            child_row("rejected", json!({})),
            Some("Canceled"),
            Some("Canceled"),
        ),
        ("a fired child filled", child_row("filled", json!({})), None, None),
        ("a fired child resting", child_row("open", json!({"order_type": "limit"})), None, None),
        ("a limit resting", limit_row("open", json!({})), None, None),
        ("a limit filled", limit_row("filled", json!({})), None, None),
        ("a limit cancelled", limit_row("cancelled", json!({})), None, Some("Canceled")),
        ("a limit rejected", limit_row("rejected", json!({})), None, Some("Rejected")),
        (
            "a limit rejected in one word",
            limit_row("rejected:x", json!({})),
            None,
            Some("Rejected"),
        ),
        // The CAPTURED shape: a fired stop's own row names itself in `trigger_order_id`.
        (
            "a fired stop's own row refused in one word",
            limit_row(
                "rejected:invalid_reduce_only_order",
                json!({"order_type": "stop_market", "triggered": true,
                       "trigger_order_id": "SLTS-11047216"}),
            ),
            Some("Canceled"),
            Some("Canceled"),
        ),
        (
            "a limit cancelled for a refused edit",
            limit_row("cancelled", json!({"cancel_reason": EDIT})),
            Some("Canceled"),
            Some("Canceled"),
        ),
        // The MEASURED shape of a crossing edit (`crates/bridges/deribit/CLAUDE.md`).
        (
            "a limit rejected for a refused edit",
            limit_row("rejected", json!({"cancel_reason": EDIT})),
            Some("Canceled"),
            Some("Canceled"),
        ),
        (
            "a limit saying it fired, cancelled",
            limit_row("cancelled", fired.clone()),
            None,
            Some("Canceled"),
        ),
        ("a limit saying it fired, rejected", limit_row("rejected", fired), None, Some("Rejected")),
    ];
    let word = |want: Option<&str>| -> Vec<String> {
        want.map(|k| format!("{k}:{ORDER_CLIENT_ID}")).into_iter().collect()
    };
    let mut wrong = Vec::new();
    let total = cells.len();
    for (name, row, live, replayed) in cells {
        let got_live = kinds(&map(&orders_frame(row.clone())));
        let got_replay = kinds(&replay(json!([row])));
        let (want_live, want_replay) = (word(live), word(replayed));
        if got_live != want_live {
            wrong.push(format!("{name}: live says {got_live:?}, wants {want_live:?}"));
        }
        if got_replay != want_replay {
            wrong.push(format!("{name}: replay says {got_replay:?}, wants {want_replay:?}"));
        }
    }
    assert!(wrong.is_empty(), "{} wrong of {total} rows:\n{}", wrong.len(), wrong.join("\n"));
}

/// The first half of the fix, in the real FSM: a stop the venue refused, and the measured shape of a limit
/// a crossing edit ended, each end on the node when only the reconnect's replay says so.
#[test]
fn an_order_the_venue_refused_does_not_stay_resting_after_a_reconnect() {
    let edit = limit_row("rejected", json!({"cancel_reason": "edit_post_only_reject"}));
    for row in [stop_row("rejected", false), edit] {
        let mut mo = accepted_stop();
        let applied = fold_all(&mut mo, &replay(json!([row])));
        assert_eq!((mo.status, applied), (OrderStatus::Canceled, vec![true]), "{row}");
    }
}

/// The second half: a stop's own row closing as it fires, read back after a reconnect, ends nothing, so
/// the fill that follows still ends the order `Filled`.
#[test]
fn a_firing_stop_does_not_read_cancelled_after_a_reconnect() {
    let mut mo = accepted_stop();
    fold_all(&mut mo, &replay(json!([stop_row("cancelled", true)])));
    let fill = map(&json!({"jsonrpc": "2.0", "method": "subscription", "params": {
        "channel": "user.trades.any.any.raw", "data": [{
            "label": ORDER_CLIENT_ID, "trade_id": "t-fired", "timestamp": 1793016943155i64, "state": "filled",
            "price": 80269.0, "direction": "sell", "instrument_name": SYMBOL, "amount": 10.0,
            "order_id": "124740642635", "fee": 0.001, "fee_currency": "BTC", "liquidity": "T"}]}}));
    let wrap: Vec<Event> = fill.into_iter().filter(|e| !matches!(e, Event::Fill(_))).collect();
    let applied = fold_all(&mut mo, &wrap);
    assert_eq!((mo.status, applied), (OrderStatus::Filled, vec![true]));
}

// --- the real pump -----------------------------------------------------------------------------

/// The `user.orders` push of a stop cancelled elsewhere, as the local venue sends it right after the
/// fills' subscribe ack (the last step of the pump's open).
const CANCELLED_STOP_FRAME: &str = r#"{"jsonrpc":"2.0","method":"subscription","params":{"channel":"user.orders.any.any.raw","data":{"label":"n0stp71","order_id":"SLTS-11047216","order_type":"stop_market","order_state":"cancelled","cancel_reason":"user_request","triggered":false,"instrument_name":"BTC-PERPETUAL","last_update_timestamp":1793016922999}}}"#;

/// A filled `user.trades` push, to prove the fills still stream although the stops' channel was refused.
const FILL_FRAME: &str = r#"{"jsonrpc":"2.0","method":"subscription","params":{"channel":"user.trades.any.any.raw","data":[{"label":"n0fill1","trade_id":"t-after-refusal","timestamp":1793016923000,"state":"filled","price":50000.0,"direction":"sell","instrument_name":"BTC-PERPETUAL","amount":10.0,"order_id":"1","fee":0.001,"fee_currency":"BTC","liquidity":"T"}]}}"#;

/// Spawn the REAL pump (with its resync supervisor, as `exec::run` wires it) against `venue` and
/// collect what the core's lane receives until `done` holds or the deadline passes.
fn run_pump(venue: &FakeDeribit, done: impl Fn(&[Event]) -> bool) -> Vec<Event> {
    let (events, mut rx) = event_channel(64);
    let fence = Arc::new(SubmitFence::new(events.clone()));
    let feed = spawn_deribit_user_data_with_resync(
        venue.url.clone(),
        "client-id".to_string(),
        "client-secret".to_string(),
        SYMBOL.to_string(),
        events,
        fence,
        Vec::new,
    );
    let mut received: Vec<Event> = Vec::new();
    let deadline = Instant::now() + Duration::from_secs(15);
    while !done(&received) && Instant::now() < deadline {
        match rx.try_recv() {
            Ok(Ingest::Event(event)) => received.push(event),
            Ok(_) => {}
            Err(_) => std::thread::sleep(Duration::from_millis(10)),
        }
    }
    feed.shutdown().expect("the pump stops cleanly");
    received
}

/// THE WIRING. The pump subscribes the stops' channel in a call of its own and the fills' LAST (the
/// fills' ack stays the final step of the open), and a `cancelled` stop row the venue pushes reaches
/// the core's lane as the stop's `OrderCanceled`.
#[test]
fn the_pump_hears_a_stop_cancelled_elsewhere() {
    let venue = FakeDeribit::spawn(vec![Session::FillStream {
        refuse_orders: false,
        frames: &[CANCELLED_STOP_FRAME],
    }]);
    let received = run_pump(&venue, |got| got.iter().any(|e| matches!(e, Event::OrderCanceled(_))));

    let canceled: Vec<&Event> =
        received.iter().filter(|e| matches!(e, Event::OrderCanceled(_))).collect();
    assert_eq!(canceled.len(), 1, "the stop's end reached the lane once: {received:?}");
    match canceled[0] {
        Event::OrderCanceled(c) => assert_eq!(c.order_client_id, ORDER_CLIENT_ID),
        _ => unreachable!(),
    }

    let channels: Vec<Value> =
        venue.params_of("private/subscribe").iter().map(|p| p["channels"].clone()).collect();
    assert_eq!(
        channels,
        vec![json!(["user.orders.any.any.raw"]), json!(["user.trades.any.any.raw"])],
        "the stops' channel in a call of its own, the fills' call last"
    );
}

/// THE FILLS COME FIRST. A venue that refuses the `user.orders` channel must cost the node nothing but
/// the stops' news: the pump stays up and the fills keep streaming. (One channel list in one call would
/// fail the whole subscribe as an auth error and stop the fill pump for good.)
#[test]
fn a_venue_that_refuses_the_stops_channel_does_not_cost_the_fills() {
    let venue = FakeDeribit::spawn(vec![Session::FillStream {
        refuse_orders: true,
        frames: &[FILL_FRAME],
    }]);
    let received = run_pump(&venue, |got| got.iter().any(|e| matches!(e, Event::Fill(_))));
    assert!(
        received.iter().any(|e| matches!(e, Event::Fill(f) if f.order_client_id == "n0fill1")),
        "the fill after the refused channel reached the lane: {received:?}"
    );
    assert_eq!(venue.connections(), 1, "the refusal did not make the pump reconnect");
}
