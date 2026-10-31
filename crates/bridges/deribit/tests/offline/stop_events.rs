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
//!
//! Every test goes through a PUBLIC entry that existed before the change (`map_deribit_private`,
//! `map_deribit_history`, the real pump over the local venue), so the file compiles against the old
//! tree and the tests that describe the new behaviour fail there.

use std::sync::Arc;
use std::time::{Duration, Instant};

use serde_json::{Value, json};
use vike_bridge_core::exec_actor::submit_fence::SubmitFence;
use vike_deribit::event_mapper::map_deribit_private;
use vike_deribit::history::map_deribit_history;
use vike_deribit::user_data::spawn_deribit_user_data_with_resync;
use vike_exec::{Ingest, event_channel};
use vike_model::events::{Event, OrderAccepted, OrderSubmitted};
use vike_model::{ManagedOrder, OrderRequest, OrderStatus, OrderType};

use crate::fake_deribit_ws::{FakeDeribit, SYMBOL, Session};

const COID: &str = "n0stp71";

/// One `user.orders.any.any.raw` push carrying `data` (an order object, or a list of them).
fn orders_frame(data: Value) -> Value {
    json!({"jsonrpc": "2.0", "method": "subscription", "params": {
        "channel": "user.orders.any.any.raw", "data": data}})
}

/// A held reduce-only stop, as `native_stops::held_stop_row` has it from the open-order read.
fn stop_row(order_state: &str, triggered: bool) -> Value {
    json!({
        "amount": 10.0, "api": true, "direction": "sell", "instrument_name": SYMBOL,
        "label": COID, "last_update_timestamp": 1793016922999i64, "order_id": "SLTS-11047216",
        "order_state": order_state, "order_type": "stop_market", "price": "market_price",
        "reduce_only": true, "trigger": "last_price", "trigger_price": 80274.5,
        "triggered": triggered,
    })
}

/// The order a FIRED stop placed: a market order carrying the stop's label and naming the stop.
fn child_row(order_state: &str, extra: Value) -> Value {
    let mut row = json!({
        "amount": 10.0, "api": true, "direction": "sell", "instrument_name": SYMBOL,
        "label": COID, "last_update_timestamp": 1793016943200i64, "order_id": "124740642635",
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

/// What an event is, with its coid: `Canceled:<coid>` and so on.
fn kinds(events: &[Event]) -> Vec<String> {
    events
        .iter()
        .map(|e| match e {
            Event::OrderCanceled(c) => format!("Canceled:{}", c.client_order_id),
            Event::OrderTriggered(t) => format!("Triggered:{}", t.client_order_id),
            Event::OrderRejected(r) => format!("Rejected:{}", r.client_order_id),
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
    assert_eq!(kinds(&events), [format!("Canceled:{COID}")]);
    let reason = canceled_reason(&events);
    assert!(reason.contains("cancelled") && reason.contains("user_request"), "{reason}");
    if let Event::OrderCanceled(c) = &events[0] {
        assert_eq!(c.ts, 1793016922999, "the venue's own update time");
    }
}

/// A list of rows (the `100ms` / `agg2` intervals) maps like one object, row by row.
#[test]
fn a_list_of_rows_and_a_single_object_both_map() {
    let cancelled = stop_row("cancelled", false);
    let mut other = stop_row("cancelled", false);
    other["label"] = json!("n0stp72");
    let events = map(&orders_frame(json!([cancelled, other])));
    assert_eq!(kinds(&events), [format!("Canceled:{COID}"), "Canceled:n0stp72".to_string()]);
}

/// A reduce-only stop that fires on a FLAT position: the venue refuses the order the trigger placed.
/// The stop was accepted long ago, so the end is `OrderCanceled` (the FSM refuses a reject after the
/// accept: see `a_reject_cannot_end_an_accepted_order`), carrying the venue's reason.
#[test]
fn the_order_a_fired_stop_placed_being_refused_ends_the_stop() {
    let row = child_row("rejected", json!({"reject_reason": "reduce_only"}));
    let events = map(&orders_frame(row));
    assert_eq!(kinds(&events), [format!("Canceled:{COID}")]);
    let reason = canceled_reason(&events);
    assert!(reason.contains("rejected") && reason.contains("reduce_only"), "{reason}");
}

/// A stop-limit's limit leg RESTS under the stop's label after the trigger; cancelled elsewhere, that
/// ends the stop's order. The same goes for a trigger row refused outright.
#[test]
fn a_cancelled_limit_leg_and_a_refused_stop_row_end_the_order() {
    let leg =
        child_row("cancelled", json!({"order_type": "limit", "cancel_reason": "user_request"}));
    assert_eq!(kinds(&map(&orders_frame(leg))), [format!("Canceled:{COID}")]);
    let mut refused = stop_row("rejected", false);
    refused["reject_reason"] = json!("post_only_reject");
    assert_eq!(kinds(&map(&orders_frame(refused))), [format!("Canceled:{COID}")]);
}

/// The firing is announced: the stop's row says `triggered` (the state word), or `triggered: true` on a
/// row that has not ended. The fill, if any, books on the stop by `label` (`native_stops`).
#[test]
fn a_stop_that_fires_is_announced() {
    assert_eq!(
        kinds(&map(&orders_frame(stop_row("triggered", true)))),
        [format!("Triggered:{COID}")]
    );
    assert_eq!(kinds(&map(&orders_frame(stop_row("open", true)))), [format!("Triggered:{COID}")]);
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

fn request() -> OrderRequest {
    OrderRequest {
        client_order_id: COID.to_string(),
        venue: "deribit".to_string(),
        symbol: SYMBOL.to_string(),
        side: -1,
        qty: 10.0,
        order_type: OrderType::Stop,
        trigger_price: Some(80274.5),
        reduce_only: true,
        ts: 1,
        ..Default::default()
    }
}

/// An order the way the node holds a stop: Submitted, then the venue's `untriggered` answer.
fn accepted_stop() -> ManagedOrder {
    let mut mo = ManagedOrder::new(request());
    mo.apply(&Event::OrderSubmitted(OrderSubmitted { client_order_id: COID.into(), ts: 1 }))
        .expect("submitted");
    mo.apply(&Event::OrderAccepted(OrderAccepted {
        client_order_id: COID.into(),
        venue_order_id: Some("SLTS-11047216".into()),
        ts: 2,
    }))
    .expect("accepted");
    assert_eq!(mo.status, OrderStatus::Accepted);
    mo
}

fn fold_all(mo: &mut ManagedOrder, events: &[Event]) -> Vec<bool> {
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
    assert_eq!(kinds(&live), [format!("Canceled:{COID}")]);
    assert_eq!(kinds(&replay), [format!("Canceled:{COID}")]);

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
        client_order_id: COID.into(),
        reason: "refused".into(),
        ts: 3,
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
    assert_eq!(kinds(&events), [format!("Canceled:{COID}")]);

    let plain = json!({"label": "c_rej", "order_id": "o11", "order_state": "rejected",
                       "last_update_timestamp": 9});
    let events = map_deribit_history(&json!([plain]), &json!([]), "deribit", SYMBOL);
    assert_eq!(kinds(&events), ["Rejected:c_rej".to_string()]);
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
        Event::OrderCanceled(c) => assert_eq!(c.client_order_id, COID),
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
        received.iter().any(|e| matches!(e, Event::Fill(f) if f.client_order_id == "n0fill1")),
        "the fill after the refused channel reached the lane: {received:?}"
    );
    assert_eq!(venue.connections(), 1, "the refusal did not make the pump reconnect");
}
