use super::inbound::update_position_map;
use super::outbound::SwapRecheck;
use super::reconcile::rebuild_position_book;
use super::reconcile::{Reply, read_reply};
use super::session::{
    GRANT_IS_DEAD, HOLD_ORDERS_FOR, ON_THE_SAME_SOCKET, OrderLane, PROBE_MESSAGE_ID,
    REAUTH_MESSAGE_ID, RECONNECT_WAIT, SessionLapse, SessionWord, WAITING_FOR_A_SESSION,
    expires_while_held, is_auth_error, reconnect_with_backoff, session_word,
};
use super::*;
use crate::transport::proto::{ProtoOaErrorRes, ProtoOaExecutionEvent, ProtoOaReconcileRes};

/// An order client id ↔ order-id map already holding `rows`.
fn order_ids(rows: &[(&str, i64)]) -> Arc<Mutex<OrderVenueIdRegistry<i64>>> {
    let map = Arc::new(Mutex::new(OrderVenueIdRegistry::<i64>::default()));
    for (order_client_id, order_id) in rows {
        map.lock().expect("orders").bind(order_client_id, *order_id);
    }
    map
}

/// One OPEN wire position in symbol 1, for the position-book provenance tests below.
fn wire_open(position_id: i64, volume: i64) -> crate::transport::proto::ProtoOaPosition {
    crate::transport::proto::ProtoOaPosition {
        position_id,
        position_status: crate::transport::proto::ProtoOaPositionStatus::PositionStatusOpen as i32,
        trade_data: crate::transport::proto::ProtoOaTradeData {
            symbol_id: 1,
            volume,
            trade_side: crate::transport::proto::ProtoOaTradeSide::Buy as i32,
            open_timestamp: Some(10),
            ..Default::default()
        },
        ..Default::default()
    }
}

/// ⚠ **A `RECONCILE_RES` for ANOTHER account is not this account's answer.** It used to be
/// folded in wholesale — a stranger's positions replacing ours and the result marked
/// AUTHORITATIVE, so a stranger's flat book manufactured this account's `Flat` and refused its
/// exit under `halt_admit = "verify"`. Delete the `res.ctid_trader_account_id != ctid` guard in
/// [`rebuild_position_book`] and this goes red, as does
/// `crates/bridges/ctrader/tests/offline/execution_halt.rs`'s
/// `a_reconcile_answer_for_another_account_is_not_this_accounts_evidence` end to end.
#[test]
fn a_reconcile_answer_for_another_ctid_is_discarded_and_clears_the_evidence() {
    let positions: PositionsByIdShared = Arc::new(Mutex::new(PositionsById::default()));

    // Our own answer lands and is authoritative.
    let mine = ProtoOaReconcileRes {
        ctid_trader_account_id: 99,
        position: vec![wire_open(7, 100_000)],
        ..Default::default()
    };
    rebuild_position_book(&mine, 99, &positions);
    {
        let book = positions.lock().expect("lock");
        assert!(book.is_fetched());
        assert_eq!(book.len(), 1);
    }

    // A stranger's (flat) answer must change NOTHING except to withdraw the evidence claim.
    let theirs = ProtoOaReconcileRes {
        ctid_trader_account_id: 12_345,
        position: vec![],
        ..Default::default()
    };
    rebuild_position_book(&theirs, 99, &positions);
    let book = positions.lock().expect("lock");
    assert_eq!(book.len(), 1, "our routing entries must survive a foreign answer, not be wiped");
    assert!(
        !book.is_fetched(),
        "we asked and what came back was not about us — whatever we believed is now of \
             unknown standing, so it may not refuse anything"
    );
}

/// ⚠ **The completeness discipline does not stop at the reconcile.** The evidence flag is set
/// ONCE and the book then drifts forwards on execution events for the life of the socket, so an
/// event carrying a position ref this build cannot CLASSIFY would otherwise remove a live row
/// while the book went on calling itself authoritative — and that manufactured absence refuses
/// the position's own exit. Delete the `invalidate()` in [`update_position_map`]'s hole arm and
/// this goes red.
#[test]
fn an_unclassifiable_position_ref_on_an_event_withdraws_the_evidence_claim() {
    let positions: PositionsByIdShared = Arc::new(Mutex::new(PositionsById::default()));
    let res = ProtoOaReconcileRes {
        ctid_trader_account_id: 99,
        position: vec![wire_open(7, 100_000), wire_open(8, 40_000)],
        ..Default::default()
    };
    rebuild_position_book(&res, 99, &positions);
    assert!(positions.lock().expect("lock").is_fetched());

    // A CLOSED position is KNOWLEDGE: drop the entry, keep the evidence claim.
    let mut closed = wire_open(8, 0);
    closed.position_status =
        crate::transport::proto::ProtoOaPositionStatus::PositionStatusClosed as i32;
    update_position_map(
        &ProtoOaExecutionEvent { position: Some(closed), ..Default::default() },
        &positions,
    );
    {
        let book = positions.lock().expect("lock");
        assert_eq!(book.len(), 1, "the closed position is gone");
        assert!(book.is_fetched(), "the venue TOLD us it closed — that is knowledge, not a hole");
    }

    // An ERROR position is a HOLE: drop the entry AND stop being evidence.
    let mut broken = wire_open(7, 100_000);
    broken.position_status =
        crate::transport::proto::ProtoOaPositionStatus::PositionStatusError as i32;
    update_position_map(
        &ProtoOaExecutionEvent { position: Some(broken), ..Default::default() },
        &positions,
    );
    let book = positions.lock().expect("lock");
    assert!(
        !book.is_fetched(),
        "a position ref this build cannot classify is a hole — the book must stop refusing \
             exits on the absence it just created"
    );
}

// ── bound exits: the orphan cancel, the close-fold guard, the reconcile adoption ───────────────

use super::inbound::{execution_event_to_canonical, update_bound_exits, update_order_map};
use super::outbound::{orphan_cancel_commands, restore_bound_exit_on_write_failure};
use super::reconcile::{adopt_bound_exits, forget_vanished_exits};
use crate::execution::positions::{CloseTracker, PositionTracked, StrandedClose};
use crate::transport::proto::{
    ProtoOaExecutionType, ProtoOaOrder, ProtoOaOrderStatus, ProtoOaOrderType, ProtoOaPosition,
    ProtoOaPositionStatus, ProtoOaTradeData, ProtoOaTradeSide,
};
use std::collections::{HashMap, HashSet, VecDeque};
use vike_bridge_core::exit::ExitKind;
use vike_model::events::Event;

/// A resting or market order of symbol 1 as an execution event's order ref carries it.
fn wire_order(
    order_client_id: &str,
    order_id: i64,
    order_type: ProtoOaOrderType,
    side: ProtoOaTradeSide,
    position_id: Option<i64>,
) -> ProtoOaOrder {
    ProtoOaOrder {
        order_id,
        trade_data: ProtoOaTradeData {
            symbol_id: 1,
            volume: 100_000,
            trade_side: side as i32,
            label: Some(order_client_id.to_string()),
            ..Default::default()
        },
        order_type: order_type as i32,
        order_status: ProtoOaOrderStatus::OrderStatusAccepted as i32,
        client_order_id: Some(order_client_id.to_string()),
        position_id,
        ..Default::default()
    }
}

fn execution_event(
    kind: ProtoOaExecutionType,
    order: Option<ProtoOaOrder>,
    position: Option<ProtoOaPosition>,
) -> ProtoOaExecutionEvent {
    ProtoOaExecutionEvent { execution_type: kind as i32, order, position, ..Default::default() }
}

/// The position ref a CLOSE leaves behind: the venue reports it CREATED with volume 0 (measured
/// 2026-10-26), not CLOSED.
fn wire_gone(position_id: i64) -> ProtoOaPosition {
    let mut p = wire_open(position_id, 0);
    p.position_status = ProtoOaPositionStatus::PositionStatusCreated as i32;
    p
}

/// What the actor does with one execution event, in its order.
fn apply(
    event: &ProtoOaExecutionEvent,
    positions: &PositionsByIdShared,
    orders: &Arc<Mutex<OrderVenueIdRegistry<i64>>>,
) {
    update_position_map(event, positions);
    update_bound_exits(event, positions);
    update_order_map(event, orders);
}

/// A hedged book holding the long 7, with a bound stop `sl` and take-profit `tp` whose accepts have
/// taught the order ids 501 and 502.
fn bracket_on_position_7() -> (PositionsByIdShared, Arc<Mutex<OrderVenueIdRegistry<i64>>>) {
    let positions: PositionsByIdShared = Arc::new(Mutex::new(PositionsById::default()));
    let orders = Arc::new(Mutex::new(OrderVenueIdRegistry::<i64>::default()));
    let res = ProtoOaReconcileRes {
        ctid_trader_account_id: 99,
        position: vec![wire_open(7, 100_000)],
        ..Default::default()
    };
    rebuild_position_book(&res, 99, &positions);
    {
        let mut book = positions.lock().expect("lock");
        let free = book.for_symbol(1);
        book.bind_exit_among("sl", -1, ExitKind::Stop, 100_000, &free).expect("sl binds");
        book.bind_exit_among("tp", -1, ExitKind::Limit, 100_000, &free).expect("tp binds");
    }
    for (order_client_id, id, ty) in
        [("sl", 501, ProtoOaOrderType::Stop), ("tp", 502, ProtoOaOrderType::Limit)]
    {
        let order = wire_order(order_client_id, id, ty, ProtoOaTradeSide::Sell, Some(7));
        apply(
            &execution_event(ProtoOaExecutionType::OrderAccepted, Some(order), None),
            &positions,
            &orders,
        );
    }
    (positions, orders)
}

fn cancels(commands: Vec<ConnectionCommand>) -> Vec<(i64, String)> {
    let mut out: Vec<_> = commands
        .into_iter()
        .map(|c| match c {
            ConnectionCommand::CancelOrder { order_id, order_client_id } => {
                (order_id, order_client_id)
            }
            _ => panic!("the bridge writes only cancels of its own accord"),
        })
        .collect();
    out.sort();
    out
}

/// ⚠ **The venue does not cancel a bound exit when its position closes another way, and a stale bound
/// stop that reaches the market OPENS a position (measured 2026-10-26).** So when ANY close of the
/// position is seen (here a market close, which is also what `close_all` and a flatten are), BOTH
/// exits are cancelled by the bridge, once each. Nothing is cancelled while the position is open.
#[test]
fn a_position_closed_by_any_means_gets_its_bound_exits_cancelled_once_each() {
    let (positions, orders) = bracket_on_position_7();
    assert!(orphan_cancel_commands(&positions, &orders).is_empty(), "the position is open");

    let close =
        wire_order("close-1", 900, ProtoOaOrderType::Market, ProtoOaTradeSide::Sell, Some(7));
    apply(
        &execution_event(ProtoOaExecutionType::OrderFilled, Some(close), Some(wire_gone(7))),
        &positions,
        &orders,
    );

    assert_eq!(
        cancels(orphan_cancel_commands(&positions, &orders)),
        vec![(501, "sl".to_string()), (502, "tp".to_string())]
    );
    assert!(orphan_cancel_commands(&positions, &orders).is_empty(), "written once, never again");
}

/// **(b) One exit fires and closes the position: the other is cancelled by the bridge, and the engine's
/// own OCO-sibling cancel racing it is not written a second time** (that half is
/// `execution_resting.rs`'s `the_engines_cancel_of_an_orphaned_exit_is_not_written_a_second_time`). The order of
/// the event itself is never cancelled: its terminal event ends its row.
#[test]
fn when_one_exit_fires_the_other_is_cancelled_and_the_one_that_fired_is_not() {
    let (positions, orders) = bracket_on_position_7();
    let mut fill = wire_order("sl", 501, ProtoOaOrderType::Stop, ProtoOaTradeSide::Sell, Some(7));
    fill.order_status = ProtoOaOrderStatus::OrderStatusFilled as i32;
    apply(
        &execution_event(ProtoOaExecutionType::OrderFilled, Some(fill), Some(wire_gone(7))),
        &positions,
        &orders,
    );

    assert_eq!(cancels(orphan_cancel_commands(&positions, &orders)), vec![(502, "tp".to_string())]);
    let book = positions.lock().expect("lock");
    assert_eq!(book.bound_exit("sl"), None, "the filled exit's row ends with its terminal event");
    assert!(book.cancel_in_flight("tp"));
}

/// **A cancel that never reached the venue is owed again.** The write of the bridge's own cancel of an
/// orphaned exit fails (the actor's failure branch), then the socket reconnects: the cancel is re-sent,
/// and not swallowed forever by the `cancel_sent` flag that was set before the write. Without the repair
/// the stop survives at the venue and can open a position later.
#[test]
fn a_cancel_whose_write_failed_or_whose_socket_died_is_sent_again_after_the_reconnect() {
    for how in ["the write failed", "the socket died after the write"] {
        let (positions, orders) = bracket_on_position_7();
        let close =
            wire_order("close-1", 900, ProtoOaOrderType::Market, ProtoOaTradeSide::Sell, Some(7));
        let gone =
            execution_event(ProtoOaExecutionType::OrderFilled, Some(close), Some(wire_gone(7)));
        apply(&gone, &positions, &orders);
        let first = cancels(orphan_cancel_commands(&positions, &orders));
        assert_eq!(first.len(), 2, "{how}: both exits are owed a cancel");
        assert!(orphan_cancel_commands(&positions, &orders).is_empty(), "{how}: written once");

        if how == "the write failed" {
            // What the actor does when `write_command` errs: the failure event the command built.
            let failed =
                Some(Event::OrderCancelRejected(vike_model::events::OrderCancelRejected {
                    order_client_id: "tp".into(),
                    reason: "cancel write failed; connection reset".into(),
                    timestamp: 0,
                    refusal_id: None,
                }));
            restore_bound_exit_on_write_failure(&failed, &positions);
        } else {
            // The reconnect loop invalidates the book the moment the socket dies (the actor's reconnect
            // macro; its wiring is `the_actor_owes_every_written_cancel_again_when_the_socket_dies`).
            let mut book = positions.lock().expect("lock");
            book.invalidate();
            book.socket_died();
        }
        // After the reconnect's reconcile still lists the orders, the cancel(s) go out again.
        let again = cancels(orphan_cancel_commands(&positions, &orders));
        let want: Vec<_> =
            if how == "the write failed" { vec![(502, "tp".to_string())] } else { first.clone() };
        assert_eq!(again, want, "{how}");
    }
}

/// Cancelling ONE exit of a bracket while the position stays open: its sibling survives (still bound,
/// still covering, not orphaned, not cancelled), and the cancelled one is replaced at once by a new stop.
#[test]
fn cancelling_one_exit_while_the_position_stays_open_leaves_its_sibling_alone() {
    let (positions, orders) = bracket_on_position_7();
    // The engine cancels the stop: the venue's OrderCancelled names the stop and the (still open) position.
    positions.lock().expect("lock").mark_cancelling("sl");
    let mut cancelled =
        wire_order("sl", 501, ProtoOaOrderType::Stop, ProtoOaTradeSide::Sell, Some(7));
    cancelled.order_status = ProtoOaOrderStatus::OrderStatusCancelled as i32;
    apply(
        &execution_event(
            ProtoOaExecutionType::OrderCancelled,
            Some(cancelled),
            Some(wire_open(7, 100_000)),
        ),
        &positions,
        &orders,
    );

    assert!(orphan_cancel_commands(&positions, &orders).is_empty(), "the take-profit must survive");
    let mut book = positions.lock().expect("lock");
    assert_eq!(
        book.bound_exit("sl"),
        None,
        "the cancelled stop's row ended with its terminal event"
    );
    let take_profit = book.bound_exit("tp").expect("the take-profit is still bound");
    assert!(!take_profit.orphaned && !take_profit.cancelling);
    let free = book.for_symbol(1);
    assert_eq!(
        book.bind_exit_among("sl2", -1, ExitKind::Stop, 100_000, &free),
        Ok((7, 100_000)),
        "a stop re-armed after the cancel binds"
    );
}

/// An exit that vanished while the socket was down (filled, cancelled, expired) loses its row at the
/// reconnect: it would otherwise count toward its position's cover for ever and a re-armed stop would be
/// refused as already covered, leaving the position naked. Rows of orders the map never knew (a `NewOrder` in
/// flight across the gap) are left alone.
#[test]
fn an_exit_that_vanished_during_a_reconnect_loses_its_row_and_a_new_stop_binds() {
    let (positions, _orders) = bracket_on_position_7();
    // A row for an order the map has never heard of: a `NewOrder` in flight across the gap.
    positions.lock().expect("lock").bind_exit("in-flight", 7, ExitKind::Limit, 1);
    let pre: HashSet<String> = ["sl", "tp"].into_iter().map(String::from).collect();
    let present: HashSet<String> = ["tp"].into_iter().map(String::from).collect();
    forget_vanished_exits(&pre, &present, &positions);
    let mut book = positions.lock().expect("lock");
    assert_eq!(book.bound_exit("sl"), None, "the stop that is no longer pending loses its row");
    assert!(book.bound_exit("tp").is_some(), "the take-profit still rests: it keeps its row");
    assert!(book.bound_exit("in-flight").is_some(), "an order the map never knew keeps its row");
    let free = book.for_symbol(1);
    assert_eq!(
        book.bind_exit_among("sl-again", -1, ExitKind::Stop, 100_000, &free),
        Ok((7, 100_000)),
        "the re-armed stop binds"
    );
}

/// A leg of an exit that spans positions that vanished while the socket was down (no event will ever come
/// for it) is retired with its row: the exit does not wait for it for ever, and when no leg is left it is gone.
#[test]
fn a_split_exit_leg_that_vanished_during_a_reconnect_is_retired_with_its_row() {
    let positions: PositionsByIdShared = Arc::new(Mutex::new(PositionsById::default()));
    let res = ProtoOaReconcileRes {
        ctid_trader_account_id: 99,
        position: vec![wire_open(7, 100_000), wire_open(8, 100_000)],
        ..Default::default()
    };
    rebuild_position_book(&res, 99, &positions);
    {
        let mut book = positions.lock().expect("lock");
        let free = book.for_symbol(1);
        book.split_exit_among("P", -1, ExitKind::Stop, 200_000, &free, Default::default())
            .expect("splits");
        book.mark_sent("PL1");
        book.mark_sent("PL2");
    }
    let pre: HashSet<String> = ["PL1", "PL2"].into_iter().map(String::from).collect();
    let present: HashSet<String> = ["PL2"].into_iter().map(String::from).collect();
    forget_vanished_exits(&pre, &present, &positions);
    assert_eq!(
        positions.lock().expect("lock").live_exit_legs("P"),
        Some(vec!["PL2".to_string()]),
        "the leg that is no longer pending no longer holds the exit open"
    );
    forget_vanished_exits(&pre, &HashSet::new(), &positions);
    assert_eq!(
        positions.lock().expect("lock").live_exit_legs("P"),
        None,
        "and with the last one the exit is over"
    );
}

/// ⚠ **An absence in a book that is not evidence is not a reason to cancel.** After an event carried a
/// position ref this build cannot classify (the entry is dropped and the book stops being evidence), the
/// next event about a LIVE bound exit must not orphan it for "its position is not in the book". A book the
/// venue answered for still does (`an_exit_accepted_after_its_position_vanished_...`).
#[test]
fn the_next_event_about_a_live_exit_does_not_cancel_it_after_an_unclassifiable_position_ref() {
    let (positions, orders) = bracket_on_position_7();
    let mut broken = wire_open(7, 100_000);
    broken.position_status = ProtoOaPositionStatus::PositionStatusError as i32;
    apply(
        &execution_event(ProtoOaExecutionType::OrderReplaced, None, Some(broken)),
        &positions,
        &orders,
    );
    assert!(!positions.lock().expect("lock").is_fetched(), "the claim to be evidence is withdrawn");
    assert!(positions.lock().expect("lock").position(7).is_none(), "and the entry is dropped");

    let live = wire_order("sl", 501, ProtoOaOrderType::Stop, ProtoOaTradeSide::Sell, Some(7));
    apply(
        &execution_event(ProtoOaExecutionType::OrderReplaced, Some(live), None),
        &positions,
        &orders,
    );
    assert!(
        orphan_cancel_commands(&positions, &orders).is_empty(),
        "a live exit must not be cancelled on an absence the book cannot vouch for"
    );
}

/// **A refused cancel is seen even when the position is already gone.** The position closed, the bridge wrote
/// its cancel of the orphaned take-profit, and the venue refuses it with an ORDER_CANCEL_REJECTED execution
/// event. That event must give the cancel up (an `error`, no retry loop) and let the engine's own cancel
/// through, and not be read as "an event about an exit whose position is missing: orphan it" (which left the
/// row orphaned with its cancel marked as written, the engine's cancel swallowed and the stop resting).
#[test]
fn a_refused_cancel_of_an_exit_whose_position_is_gone_is_given_up_and_the_engine_may_cancel() {
    let (positions, orders) = bracket_on_position_7();
    let close =
        wire_order("close-1", 900, ProtoOaOrderType::Market, ProtoOaTradeSide::Sell, Some(7));
    apply(
        &execution_event(ProtoOaExecutionType::OrderFilled, Some(close), Some(wire_gone(7))),
        &positions,
        &orders,
    );
    assert_eq!(cancels(orphan_cancel_commands(&positions, &orders)).len(), 2);

    let refused = wire_order("tp", 502, ProtoOaOrderType::Limit, ProtoOaTradeSide::Sell, Some(7));
    apply(
        &execution_event(ProtoOaExecutionType::OrderCancelRejected, Some(refused), None),
        &positions,
        &orders,
    );

    let book = positions.lock().expect("lock");
    assert!(book.position(7).is_none(), "the position is gone");
    let take_profit = book.bound_exit("tp").expect("still a row");
    assert!(
        !take_profit.orphaned && !take_profit.cancel_sent,
        "the refused cancel is given up: {take_profit:?}"
    );
    assert!(!book.cancel_in_flight("tp"), "so the engine's own cancel is no longer swallowed");
    assert_eq!(book.exits_to_cancel(), Vec::<String>::new(), "and nothing retries in a loop");
    assert!(book.bound_exit("sl").expect("sl").cancel_sent, "the other exit is untouched");
}

// ── the wiring of the bound-exit repairs, through the real entry points ────────────────────────────
//
// The tests above call the helpers. These drive `actor_loop`, `replay_after_reconnect` and
// `reconcile_after_reconnect` THEMSELVES over a real local socket, so deleting a helper's call from
// the actor or the reconcile goes red.

use std::net::{Shutdown, TcpListener, TcpStream};

use super::stream::{ConnectionSocket, EofStream};
use crate::transport::framing::{self, FrameReader};

/// A `Reader` over a connected local socket whose WRITE side is shut down: every write fails at once
/// (a broken pipe), the way a socket that died between the actor's read and its write does. The peer is
/// returned so the reads time out instead of seeing the socket close.
fn reader_whose_writes_fail() -> (Reader, TcpStream, SocketAddr) {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
    let addr = listener.local_addr().expect("addr");
    let client = TcpStream::connect(addr).expect("connect");
    let (peer, _) = listener.accept().expect("accept");
    client.set_read_timeout(Some(Duration::from_millis(100))).expect("timeout");
    client.shutdown(Shutdown::Write).expect("shut the write side");
    let (eof, _flag) = EofStream::new(ConnectionSocket::Plain(client));
    (FrameReader::new(eof), peer, addr)
}

/// Run the real actor loop over a reader whose writes fail, with `commands` queued and a `Shutdown` behind
/// them (the reconnect that the first failed write starts then ends the loop at once).
fn run_actor_over_a_dead_writer(
    commands: Vec<ConnectionCommand>,
    positions: &PositionsByIdShared,
    orders: &Arc<Mutex<OrderVenueIdRegistry<i64>>>,
) {
    let (reader, _peer, addr) = reader_whose_writes_fail();
    let (tx, rx) = mpsc::channel();
    for cmd in commands {
        tx.send(cmd).expect("queue");
    }
    tx.send(ConnectionCommand::Shutdown).expect("queue shutdown");
    let (events, _ingest) = vike_execution::event_channel(64);
    let closes: CloseState = Arc::new(Mutex::new(CloseTracker::default()));
    super::actor::actor_loop(
        reader,
        &rx,
        ConnectionConfig::for_test(addr, "cid", "secret", "token"),
        99,
        Arc::new(SymbolMap::default()),
        2,
        Arc::new(vike_data::NoopSink),
        Some(events),
        orders.clone(),
        positions.clone(),
        closes,
    );
}

/// A bound exit whose `NewOrder` never reached the venue (the actor's write failed) loses its row, so it
/// cannot go on counting toward its position's cover. Goes red if `restore_bound_exit_on_write_failure`
/// is deleted from the actor's failure branch.
#[test]
fn the_actor_drops_the_row_of_a_new_order_whose_write_failed() {
    let positions: PositionsByIdShared = Arc::new(Mutex::new(PositionsById::default()));
    let orders = Arc::new(Mutex::new(OrderVenueIdRegistry::<i64>::default()));
    {
        let mut book = positions.lock().expect("lock");
        book.replace_all(
            [(7, PositionTracked { symbol_id: 1, side: 1, volume: 100_000, open_timestamp: 1 })],
            1,
        );
        let free = book.for_symbol(1);
        book.bind_exit_among("sl-new", -1, ExitKind::Stop, 100_000, &free).expect("binds");
    }
    let order = crate::transport::proto::ProtoOaNewOrderReq {
        client_order_id: Some("sl-new".into()),
        ..Default::default()
    };
    run_actor_over_a_dead_writer(vec![ConnectionCommand::NewOrder(order)], &positions, &orders);
    assert_eq!(
        positions.lock().expect("lock").bound_exit("sl-new"),
        None,
        "an exit that never reached the venue must not keep covering its position"
    );
}

/// A socket that dies makes every cancel written on it owed again, through the actor's own reconnect step
/// (not only the one command whose write failed): two orphaned exits, the actor writes their cancels, the
/// first write fails, and afterwards BOTH are owed again. Goes red if `socket_died` is deleted from the
/// reconnect macro.
#[test]
fn the_actor_owes_every_written_cancel_again_when_the_socket_dies() {
    let (positions, orders) = bracket_on_position_7();
    let close =
        wire_order("close-1", 900, ProtoOaOrderType::Market, ProtoOaTradeSide::Sell, Some(7));
    apply(
        &execution_event(ProtoOaExecutionType::OrderFilled, Some(close), Some(wire_gone(7))),
        &positions,
        &orders,
    );
    assert_eq!(positions.lock().expect("lock").exits_to_cancel().len(), 2, "both owed");

    run_actor_over_a_dead_writer(Vec::new(), &positions, &orders);

    let owed = positions.lock().expect("lock").exits_to_cancel();
    assert_eq!(
        owed.len(),
        2,
        "the actor marked both written, one write failed, the socket died: {owed:?}"
    );
}

/// The step the actor takes when a socket dies drops its own queue of cancels (written for the old socket,
/// rebuilt from the book by the next orphan pass), owes the written ones again and stops trusting the book.
/// Goes red if `own_commands.clear()` leaves `forget_the_dead_socket`, or if `socket_died` or `invalidate` does.
#[test]
fn a_dead_socket_drops_the_actors_own_queue_and_owes_its_cancels_again() {
    let (positions, orders) = bracket_on_position_7();
    let close =
        wire_order("close-1", 900, ProtoOaOrderType::Market, ProtoOaTradeSide::Sell, Some(7));
    apply(
        &execution_event(ProtoOaExecutionType::OrderFilled, Some(close), Some(wire_gone(7))),
        &positions,
        &orders,
    );
    let mut queue: VecDeque<ConnectionCommand> = orphan_cancel_commands(&positions, &orders).into();
    assert_eq!(queue.len(), 2, "both cancels queued, both marked written");
    assert!(positions.lock().expect("lock").exits_to_cancel().is_empty());
    assert!(positions.lock().expect("lock").is_fetched(), "evidence before the socket dies");

    let closes: CloseState = Arc::new(Mutex::new(CloseTracker::default()));
    let stranded = super::actor::forget_the_dead_socket(&positions, &closes, &mut queue);

    assert!(stranded.is_empty(), "no close was on the wire");
    assert!(queue.is_empty(), "a queue that predates the reconnect must not be written after it");
    let book = positions.lock().expect("lock");
    assert_eq!(book.exits_to_cancel().len(), 2, "both written cancels are owed again");
    assert!(!book.is_fetched(), "a dead socket's book is not evidence");
}

/// A close on the wire when its socket dies has no answer coming: the step forgets it, so its position can be closed
/// again, and hands it to the reconnect's reconcile. A close still queued is the replay's and stays. Goes red if
/// `take_written` leaves `forget_the_dead_socket`, or if the actor stops marking a written close.
#[test]
fn a_dead_socket_forgets_the_closes_written_on_it_and_keeps_the_queued_ones() {
    let positions: PositionsByIdShared = Arc::new(Mutex::new(PositionsById::default()));
    let closes: CloseState = Arc::new(Mutex::new(CloseTracker::default()));
    {
        let mut t = closes.lock().expect("lock");
        t.register("written-1", &[(7, 100)]);
        t.register("queued-1", &[(8, 100)]);
        t.mark_written("written-1", 1);
    }
    let stranded = super::actor::forget_the_dead_socket(&positions, &closes, &mut VecDeque::new());

    assert_eq!(stranded.len(), 1, "{stranded:?}");
    assert_eq!(stranded[0].order_client_id, "written-1");
    assert_eq!(stranded[0].positions, vec![7]);
    assert!(!stranded[0].progressed);
    let t = closes.lock().expect("lock");
    assert!(!t.is_closing(7), "the position can be closed again");
    assert!(t.is_closing(8), "a close the socket never carried is still the replay's");
}

/// **A stranded close is ended only when the venue lists every one of its positions WHOLE.** A position listed with
/// less than it held at the socket's death had something executed on it unheard (a leg closing PART of a position
/// leaves it listed, smaller), and one leg's position gone with another's listed is the same: either close may have
/// filled, so it gets no terminal (outcome unknown). It used to be rejected as soon as ANY of its positions was listed.
#[test]
fn a_stranded_close_ends_only_when_the_venue_lists_its_positions_whole() {
    let tracked = |volume| PositionTracked { symbol_id: 1, side: 1, volume, open_timestamp: 1 };
    let positions: PositionsByIdShared = Arc::new(Mutex::new(PositionsById::default()));
    // What the reconcile answer rebuilt: 7 whole, 8 partly closed, 9 gone (10 is listed whole).
    positions
        .lock()
        .expect("lock")
        .replace_all([(7, tracked(100_000)), (8, tracked(60_000)), (10, tracked(5_000))], 1);
    let held_at_death: HashMap<i64, i64> =
        [(7, 100_000), (8, 100_000), (9, 100_000), (10, 5_000)].into();
    let close = |order_client_id: &str, positions: Vec<i64>| StrandedClose {
        order_client_id: order_client_id.into(),
        positions,
        progressed: false,
        written_ms: 1,
    };
    let (events, mut rx) = vike_execution::event_channel(64);
    super::reconcile::end_stranded_closes(
        vec![close("whole-1", vec![7]), close("part-1", vec![8]), close("one-gone-1", vec![9, 10])],
        &held_at_death,
        &positions,
        Some(&events),
    );
    let mut ended = Vec::new();
    while let Ok(vike_execution::Ingest::Event(event)) = rx.try_recv() {
        if let Event::OrderRejected(r) = event {
            assert_eq!(r.refusal_id.as_deref(), Some(vike_model::OrderRefusalId::NotSent.word()));
            ended.push(r.order_client_id);
        }
    }
    assert_eq!(ended, ["whole-1"], "only the close that executed nothing is over");
}

/// A cancel whose write failed during the REPLAY after a reconnect is owed again (the replay is not the
/// actor's drain loop and has no reconnect after it). Goes red if `restore_bound_exit_on_write_failure` is
/// deleted from `replay_after_reconnect`.
#[test]
fn a_cancel_that_fails_to_write_in_the_replay_is_owed_again() {
    let (positions, orders) = bracket_on_position_7();
    let close =
        wire_order("close-1", 900, ProtoOaOrderType::Market, ProtoOaTradeSide::Sell, Some(7));
    apply(
        &execution_event(ProtoOaExecutionType::OrderFilled, Some(close), Some(wire_gone(7))),
        &positions,
        &orders,
    );
    assert_eq!(cancels(orphan_cancel_commands(&positions, &orders)).len(), 2, "both written");

    let (mut reader, _peer, _addr) = reader_whose_writes_fail();
    let closes: CloseState = Arc::new(Mutex::new(CloseTracker::default()));
    super::reconcile::replay_after_reconnect(
        &mut reader,
        99,
        &SymbolMap::default(),
        2,
        &vike_data::NoopSink,
        &mut HashSet::new(),
        &mut HashSet::new(),
        vec![ConnectionCommand::CancelOrder { order_id: 502, order_client_id: "tp".into() }],
        Vec::new(),
        None,
        &orders,
        &positions,
        &closes,
    );
    assert_eq!(
        positions.lock().expect("lock").exits_to_cancel(),
        vec!["tp".to_string()],
        "the cancel of tp never left; sl's did, and stays written"
    );
}

/// **A command whose write failed is refused as never sent** (decision 0303): a submit, a cancel, an amend and a close
/// with no leg filled each carry `order.not_sent`.
#[test]
fn a_failed_write_is_refused_as_not_sent() {
    let closes: CloseState = Arc::new(Mutex::new(CloseTracker::default()));
    closes.lock().expect("closes").register("c-close", &[(7, 100)]);
    let commands = [
        new_order("c-new"),
        ConnectionCommand::CancelOrder { order_id: 5, order_client_id: "c-cancel".into() },
        ConnectionCommand::AmendOrder {
            order_client_id: "c-amend".into(),
            req: Default::default(),
        },
        ConnectionCommand::ClosePosition {
            position_id: 7,
            volume: 100,
            order_client_id: "c-close".into(),
        },
    ];
    for cmd in commands {
        let refused = super::outbound::write_failure_event(&cmd, &closes);
        let (order_client_id, refusal_id) = match refused {
            Some(Event::OrderRejected(r)) => (r.order_client_id, r.refusal_id),
            Some(Event::OrderCancelRejected(r)) => (r.order_client_id, r.refusal_id),
            Some(Event::OrderModifyRejected(r)) => (r.order_client_id, r.refusal_id),
            other => panic!("a refusal, got {other:?}"),
        };
        assert_eq!(
            refusal_id.as_deref(),
            Some(vike_model::OrderRefusalId::NotSent.word()),
            "{order_client_id}"
        );
    }
}

/// **What a failed frame write left is decided on the step and the error's kind** (decisions 0301, 0303): NOT SENT only
/// when no byte was handed over or the connection is gone (the venue holds a cut frame at most, which it cannot read),
/// OUTCOME UNKNOWN when the frame was handed to a stream that may still carry it. The TLS case is "taken whole, the
/// flush failed": rustls's `write` takes the frame and drops the socket's error, and the rest of the record stays queued
/// for the next read or write of the stream. Goes red if a failed flush is read as NOT SENT again.
#[test]
fn a_failed_frame_write_is_not_sent_only_when_the_venue_cannot_read_it() {
    use std::io::ErrorKind::{
        self, BrokenPipe, ConnectionReset, Interrupted, TimedOut, WouldBlock,
    };
    use vike_utility::errors::FailureOutcome::{self, NotSent, OutcomeUnknown};

    /// Each `write` takes what its step says (the whole buffer once the steps run out); `flush` answers `flush`.
    struct Scripted {
        steps: VecDeque<Result<usize, ErrorKind>>,
        flush: Option<ErrorKind>,
    }
    impl io::Write for Scripted {
        fn write(&mut self, buffer: &[u8]) -> io::Result<usize> {
            match self.steps.pop_front() {
                Some(Ok(taken)) => Ok(taken.min(buffer.len())),
                Some(Err(kind)) => Err(kind.into()),
                None => Ok(buffer.len()),
            }
        }
        fn flush(&mut self) -> io::Result<()> {
            self.flush.map_or(Ok(()), |kind| Err(kind.into()))
        }
    }

    type Case = (
        &'static str,
        Vec<Result<usize, ErrorKind>>,
        Option<ErrorKind>,
        Result<(), FailureOutcome>,
    );
    let cases: [Case; 9] = [
        ("written whole", vec![], None, Ok(())),
        ("an interrupted write goes again", vec![Err(Interrupted)], None, Ok(())),
        (
            "refused before a byte: what rustls holds cannot go out",
            vec![Err(WouldBlock)],
            None,
            Err(NotSent),
        ),
        ("refused before a byte, the connection gone", vec![Err(BrokenPipe)], None, Err(NotSent)),
        ("taken whole, the flush interrupted", vec![], Some(Interrupted), Err(OutcomeUnknown)),
        ("taken whole, the flush timed out", vec![], Some(TimedOut), Err(OutcomeUnknown)),
        (
            "taken whole, the connection reset at the flush",
            vec![],
            Some(ConnectionReset),
            Err(NotSent),
        ),
        ("a cut frame on a connection gone", vec![Ok(3), Err(BrokenPipe)], None, Err(NotSent)),
        (
            "a cut frame on a connection that may carry the rest",
            vec![Ok(3), Err(WouldBlock)],
            None,
            Err(OutcomeUnknown),
        ),
    ];
    let frame = framing::encode(pt::NEW_ORDER_REQ, &[1, 2, 3, 4, 5, 6], "c-new");
    for (case, steps, flush, want) in cases {
        let mut stream = Scripted { steps: steps.into(), flush };
        let got =
            super::stream::write_frame(&mut stream, &frame).map_err(|failure| failure.outcome);
        assert_eq!(got, want, "{case}");
    }
}

/// **A write of unknown outcome is booked as written, and only one NOT SENT is refused** (decision 0303): a submit that
/// may have landed keeps its bound exit's row and is asked about by the reconnect (`ask_about_unanswered`), a close that
/// may have landed is stranded for the reconnect to read back, and neither gets an event; NOT SENT drops the row, forgets
/// the close and ends each with its `order.not_sent` terminal. Goes red if `book_write` refuses an unknown write.
#[test]
fn a_write_of_unknown_outcome_is_booked_as_written_and_only_one_not_sent_is_refused() {
    use vike_utility::errors::FailureOutcome::{NotSent, OutcomeUnknown};

    for outcome in [NotSent, OutcomeUnknown] {
        let positions: PositionsByIdShared = Arc::new(Mutex::new(PositionsById::default()));
        {
            let mut book = positions.lock().expect("lock");
            book.replace_all(
                [(
                    7,
                    PositionTracked { symbol_id: 1, side: 1, volume: 100_000, open_timestamp: 1 },
                )],
                1,
            );
            let free = book.for_symbol(1);
            book.bind_exit_among("sl-new", -1, ExitKind::Stop, 100_000, &free).expect("binds");
        }
        let closes: CloseState = Arc::new(Mutex::new(CloseTracker::default()));
        closes.lock().expect("closes").register("c-close", &[(8, 100)]);
        let (events, mut rx) = vike_execution::event_channel(64);
        let close = ConnectionCommand::ClosePosition {
            position_id: 8,
            volume: 100,
            order_client_id: "c-close".into(),
        };
        for cmd in [new_order("sl-new"), close] {
            super::outbound::book_write(
                Err(outcome),
                super::outbound::write_failure_event(&cmd, &closes),
                &super::outbound::new_order_order_client_id(&cmd),
                &super::outbound::close_command_order_client_id(&cmd),
                &positions,
                &closes,
                Some(&events),
            );
        }
        let mut refused = Vec::new();
        while let Ok(vike_execution::Ingest::Event(event)) = rx.try_recv() {
            match event {
                Event::OrderRejected(r) => {
                    assert_eq!(
                        r.refusal_id.as_deref(),
                        Some(vike_model::OrderRefusalId::NotSent.word())
                    );
                    refused.push(r.order_client_id);
                }
                other => panic!("{outcome:?}: only a terminal, got {other:?}"),
            }
        }
        let mut book = positions.lock().expect("lock");
        let asked: Vec<String> =
            book.take_written_orders_not_in(&HashSet::new()).into_keys().collect();
        let mut tracker = closes.lock().expect("closes");
        let stranded: Vec<String> =
            tracker.take_written().into_iter().map(|close| close.order_client_id).collect();
        if outcome == NotSent {
            assert_eq!(refused, ["sl-new", "c-close"], "each ends with its terminal");
            assert_eq!(book.bound_exit("sl-new"), None, "the exit that never left stops covering");
            assert!(!tracker.is_closing(8), "the close that never left is forgotten");
            assert!(asked.is_empty() && stranded.is_empty(), "nothing is asked about");
        } else {
            assert!(refused.is_empty(), "an order that may have landed is never refused");
            assert!(
                book.bound_exit("sl-new").is_some_and(|exit| exit.sent),
                "it is a written exit"
            );
            assert_eq!(asked, ["sl-new"], "the reconnect asks the venue about the submit");
            assert_eq!(stranded, ["c-close"], "and reads the close back");
        }
    }
}

/// A key subscribed while the socket was down is replayed by the NEXT reconnect too, and a key released while
/// it was down is not (they used to be written once and never tracked: the first went silent at the second
/// reconnect, the second came back for ever). Goes red if `track_subscription` leaves `replay_after_reconnect`.
#[test]
fn a_key_taken_or_released_during_an_outage_is_tracked_for_the_next_reconnect() {
    use crate::transport::proto::ProtoOaTrendbarPeriod::M1;

    let mut spot_subs = HashSet::from([2_i64]);
    let mut trendbar_subs = HashSet::from([(2_i64, M1)]);
    let (mut reader, _peer, _addr) = reader_whose_writes_fail();
    super::reconcile::replay_after_reconnect(
        &mut reader,
        99,
        &SymbolMap::default(),
        2,
        &vike_data::NoopSink,
        &mut spot_subs,
        &mut trendbar_subs,
        vec![
            ConnectionCommand::SubscribeSpots { symbol_id: 1 },
            ConnectionCommand::SubscribeTrendbar { symbol_id: 1, period: M1 },
            ConnectionCommand::UnsubscribeTrendbar { symbol_id: 2, period: M1 },
            ConnectionCommand::UnsubscribeSpots { symbol_id: 2 },
        ],
        Vec::new(),
        None,
        &Arc::new(Mutex::new(OrderVenueIdRegistry::default())),
        &Arc::new(Mutex::new(PositionsById::default())),
        &Arc::new(Mutex::new(CloseTracker::default())),
    );
    assert_eq!(spot_subs, HashSet::from([1]));
    assert_eq!(trendbar_subs, HashSet::from([(1, M1)]));
}

/// After a reconnect the REAL reconcile entry point drops the rows of written orders that are not pending
/// any more (including one whose accept the order map never saw) and keeps the rest. Goes red if
/// `forget_vanished_exits` is deleted from `reconcile_after_reconnect`.
#[test]
fn the_reconnect_reconcile_drops_the_rows_of_exits_that_are_not_pending_any_more() {
    use std::io::Write as _;

    let positions: PositionsByIdShared = Arc::new(Mutex::new(PositionsById::default()));
    let orders = order_ids(&[("gone-known", 1), ("still-pending", 2)]);
    {
        let mut book = positions.lock().expect("lock");
        for (order_client_id, sent) in [
            ("gone-known", true),
            ("never-known", true),
            ("still-pending", true),
            ("queued", false),
        ] {
            book.bind_exit(order_client_id, 7, ExitKind::Stop, 100_000);
            if sent {
                book.mark_sent(order_client_id);
            }
        }
    }
    // The venue: one pending order, "still-pending".
    let (mut reader, server_side, _addr) = reader_whose_reads_are_scripted();
    let answer = ProtoOaReconcileRes {
        ctid_trader_account_id: 99,
        order: vec![wire_order(
            "still-pending",
            2,
            ProtoOaOrderType::Stop,
            ProtoOaTradeSide::Sell,
            Some(7),
        )],
        ..Default::default()
    };
    let frame = framing::encode(pt::RECONCILE_RES, &answer.encode_to_vec(), "");
    (&server_side).write_all(&frame).expect("script the answer");

    let closes: CloseState = Arc::new(Mutex::new(CloseTracker::default()));
    super::reconcile::reconcile_after_reconnect(
        &mut reader,
        99,
        &SymbolMap::default(),
        2,
        &vike_data::NoopSink,
        &orders,
        &positions,
        &closes,
        Vec::new(),
        None,
    );
    let book = positions.lock().expect("lock");
    assert_eq!(
        book.bound_exit("gone-known"),
        None,
        "it was live before the gap and is not pending now"
    );
    assert_eq!(
        book.bound_exit("never-known"),
        None,
        "written, never accepted: it went with the socket"
    );
    assert!(book.bound_exit("still-pending").is_some(), "it still rests");
    assert!(book.bound_exit("queued").is_some(), "not written yet: it is still coming");
}

/// A `Reader` whose writes succeed (into the socket buffer) and whose reads are fed by the returned peer.
fn reader_whose_reads_are_scripted() -> (Reader, TcpStream, SocketAddr) {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
    let addr = listener.local_addr().expect("addr");
    let client = TcpStream::connect(addr).expect("connect");
    let (peer, _) = listener.accept().expect("accept");
    client.set_read_timeout(Some(Duration::from_millis(200))).expect("timeout");
    let (eof, _flag) = EofStream::new(ConnectionSocket::Plain(client));
    (FrameReader::new(eof), peer, addr)
}

/// The venue's refusal of a cancel puts the exit back in its position's cover (and gives the bridge's
/// own cancel up), both as an execution event and as the cancelling mark.
#[test]
fn a_refused_cancel_puts_the_exit_back_in_its_positions_cover() {
    let (positions, orders) = bracket_on_position_7();
    positions.lock().expect("lock").mark_cancelling("sl");
    let refused = wire_order("sl", 501, ProtoOaOrderType::Stop, ProtoOaTradeSide::Sell, Some(7));
    apply(
        &execution_event(ProtoOaExecutionType::OrderCancelRejected, Some(refused), None),
        &positions,
        &orders,
    );
    let mut book = positions.lock().expect("lock");
    assert!(!book.bound_exit("sl").expect("still bound").cancelling);
    let free = book.for_symbol(1);
    assert_eq!(
        book.bind_exit_among("sl2", -1, ExitKind::Stop, 100_000, &free),
        Err(crate::execution::positions::ExitRefusal::Covered)
    );
}

/// An amend of a bound exit's quantity changes the room its position has: the venue's `ORDER_REPLACED`
/// carries the new volume, and the next exit of that kind is capped to what is really left.
#[test]
fn an_amended_exit_volume_changes_the_room_its_position_has() {
    let (positions, orders) = bracket_on_position_7();
    let mut replaced =
        wire_order("sl", 501, ProtoOaOrderType::Stop, ProtoOaTradeSide::Sell, Some(7));
    replaced.trade_data.volume = 40_000;
    apply(
        &execution_event(ProtoOaExecutionType::OrderReplaced, Some(replaced), None),
        &positions,
        &orders,
    );
    let mut book = positions.lock().expect("lock");
    assert_eq!(book.bound_exit("sl").map(|b| b.volume), Some(40_000));
    let free = book.for_symbol(1);
    assert_eq!(
        book.bind_exit_among("sl2", -1, ExitKind::Stop, 100_000, &free),
        Ok((7, 60_000)),
        "the room is what the amended stop leaves"
    );
}

/// An exit the venue accepts AFTER its position vanished (it closed between the submit and the accept)
/// is orphaned on arrival and cancelled as soon as its order id is known; an exit whose accept has not
/// arrived is waited for, not cancelled blind.
#[test]
fn an_exit_accepted_after_its_position_vanished_is_cancelled_on_arrival() {
    let positions: PositionsByIdShared = Arc::new(Mutex::new(PositionsById::default()));
    let orders = Arc::new(Mutex::new(OrderVenueIdRegistry::<i64>::default()));
    {
        let mut book = positions.lock().expect("lock");
        // A book the venue has answered for (evidence), holding the long.
        book.replace_all(
            [(7, PositionTracked { symbol_id: 1, side: 1, volume: 100_000, open_timestamp: 1 })],
            1,
        );
        let free = book.for_symbol(1);
        book.bind_exit_among("late", -1, ExitKind::Stop, 100_000, &free).expect("binds");
        book.remove(7); // the position closed before the venue answered
    }
    assert!(orphan_cancel_commands(&positions, &orders).is_empty(), "nothing orphaned yet");
    let order = wire_order("late", 777, ProtoOaOrderType::Stop, ProtoOaTradeSide::Sell, Some(7));
    apply(
        &execution_event(ProtoOaExecutionType::OrderAccepted, Some(order), None),
        &positions,
        &orders,
    );
    assert_eq!(
        cancels(orphan_cancel_commands(&positions, &orders)),
        vec![(777, "late".to_string())]
    );
}

/// ⚠ **An event belongs to a market close only if it carries NO order client id or the close's own.** A
/// resting exit bound to the same position references the same `positionId`, and its cancel (or fill)
/// during a `market-exit` must reach the engine under ITS order client id, not terminalize the close's. Delete the
/// order-client-id test in `execution_event_to_canonical` and the first assert goes red.
#[test]
fn a_bound_exits_events_are_not_folded_into_a_market_close_in_flight() {
    let in_flight = || -> CloseState {
        let closes: CloseState = Arc::new(Mutex::new(CloseTracker::default()));
        closes.lock().expect("lock").register("mx", &[(7, 100_000)]);
        closes
    };
    let closes = in_flight();
    let symbols = SymbolMap::default();

    // The bound stop's cancel (what the bridge writes for an orphan, or the engine's OCO cancel).
    let cancelled = execution_event(
        ProtoOaExecutionType::OrderCancelled,
        Some(wire_order("sl", 501, ProtoOaOrderType::Stop, ProtoOaTradeSide::Sell, Some(7))),
        None,
    );
    let out = execution_event_to_canonical(&cancelled, &symbols, 2, &closes);
    assert!(
        matches!(&out[..], [Event::OrderCanceled(c)] if c.order_client_id == "sl"),
        "the exit's cancel is the EXIT's: {out:?}"
    );
    assert!(closes.lock().expect("lock").is_tracked("mx"), "and the close is still in flight");

    // The close's own events carry its order client id (the envelope id) or none: both fold, as before.
    for order_client_id in [Some("mx"), None] {
        let mut order =
            wire_order("ignored", 900, ProtoOaOrderType::Market, ProtoOaTradeSide::Sell, Some(7));
        order.client_order_id = order_client_id.map(str::to_string);
        let accepted = execution_event(ProtoOaExecutionType::OrderAccepted, Some(order), None);
        let out = execution_event_to_canonical(&accepted, &symbols, 2, &in_flight());
        assert!(
            matches!(&out[..], [Event::OrderAccepted(a)] if a.order_client_id == "mx"),
            "{order_client_id:?}: the close's own accept is the close's: {out:?}"
        );
    }
}

/// **A stop placed by hand is never adopted, so the bridge never cancels it** (the owner's ruling: a foreign order is
/// cancelled by hand only). Every row faces an open position; only the one carrying the order client id in both fields, as the node
/// writes it and the venue echoes it, is the node's.
#[test]
fn a_stop_placed_by_hand_is_not_adopted_whatever_its_fields_say() {
    let hand = |id: i64, order_client_id: Option<&str>, label: Option<&str>| {
        let mut order =
            wire_order("x", id, ProtoOaOrderType::Stop, ProtoOaTradeSide::Sell, Some(7));
        order.client_order_id = order_client_id.map(str::to_string);
        order.trade_data.label = label.map(str::to_string);
        order
    };
    let res = ProtoOaReconcileRes {
        ctid_trader_account_id: 99,
        position: vec![wire_open(7, 100_000)],
        order: vec![
            wire_order("ours", 700, ProtoOaOrderType::Stop, ProtoOaTradeSide::Sell, Some(7)),
            hand(701, None, Some("typed-label")),
            hand(702, Some("platform-request-id"), Some("typed-label")),
            hand(703, Some("platform-request-id"), None),
            hand(704, Some(""), Some("")),
            hand(705, None, None),
        ],
        ..Default::default()
    };
    let positions: PositionsByIdShared = Arc::new(Mutex::new(PositionsById::default()));
    rebuild_position_book(&res, 99, &positions);
    adopt_bound_exits(&res, 99, &SymbolMap::default(), &positions);
    let book = positions.lock().expect("lock");
    assert!(book.bound_exit("ours").is_some(), "the node's own exit is adopted");
    for foreign in ["typed-label", "platform-request-id", ""] {
        assert_eq!(book.bound_exit(foreign), None, "{foreign:?}");
    }
}

/// The reconcile ADOPTS a resting stop or limit of the node's that sits on an OPEN position and faces it (a
/// bound exit of an earlier run), so the bridge can cancel it when the position later closes; a
/// plain pending order (its position is a new, empty one), a protection order and a same-side order are
/// not adopted, and a NETTED account adopts nothing. An answer read in full that no longer lists the
/// position then orphans what was adopted.
#[test]
fn the_reconcile_adopts_exits_on_open_positions_and_a_flat_answer_orphans_them() {
    // `closing_order` is set on the stop and left unset on the limit: the venue's flag marks an order that
    // closes a position, which a bound exit is, so adoption must not depend on it either way.
    let mut adopt_stop_loss =
        wire_order("adopt-sl", 601, ProtoOaOrderType::Stop, ProtoOaTradeSide::Sell, Some(7));
    adopt_stop_loss.closing_order = Some(true);
    let adopt_take_profit =
        wire_order("adopt-tp", 602, ProtoOaOrderType::Limit, ProtoOaTradeSide::Sell, Some(7));
    let plain = wire_order("plain", 603, ProtoOaOrderType::Limit, ProtoOaTradeSide::Buy, Some(99));
    let mut protection = wire_order(
        "prot",
        604,
        ProtoOaOrderType::StopLossTakeProfit,
        ProtoOaTradeSide::Sell,
        Some(7),
    );
    protection.closing_order = Some(true);
    let same_side =
        wire_order("adds", 605, ProtoOaOrderType::Limit, ProtoOaTradeSide::Buy, Some(7));
    let res = ProtoOaReconcileRes {
        ctid_trader_account_id: 99,
        position: vec![wire_open(7, 100_000)],
        order: vec![adopt_stop_loss, adopt_take_profit, plain, protection, same_side],
        ..Default::default()
    };

    let netted = SymbolMap::from_lists(&SymbolLists {
        account_type: Some(crate::transport::proto::ProtoOaAccountType::Netted as i32),
        ..Default::default()
    });
    let netted_book: PositionsByIdShared = Arc::new(Mutex::new(PositionsById::default()));
    rebuild_position_book(&res, 99, &netted_book);
    adopt_bound_exits(&res, 99, &netted, &netted_book);
    assert_eq!(netted_book.lock().expect("lock").bound_exit("adopt-sl"), None, "netted: untouched");

    let positions: PositionsByIdShared = Arc::new(Mutex::new(PositionsById::default()));
    rebuild_position_book(&res, 99, &positions);
    adopt_bound_exits(&res, 99, &SymbolMap::default(), &positions);
    {
        let book = positions.lock().expect("lock");
        let stop_loss = book.bound_exit("adopt-sl").expect("the stop is adopted");
        assert_eq!(
            (stop_loss.position_id, stop_loss.kind, stop_loss.volume),
            (7, ExitKind::Stop, 100_000)
        );
        assert_eq!(book.bound_exit("adopt-tp").map(|b| b.kind), Some(ExitKind::Limit));
        for not_adopted in ["plain", "prot", "adds"] {
            assert_eq!(book.bound_exit(not_adopted), None, "{not_adopted}");
        }
    }

    // The position then closes while the bridge is away: the next answer, read in full, lists none.
    let orders = order_ids(&[("adopt-sl", 601), ("adopt-tp", 602)]);
    let flat = ProtoOaReconcileRes { ctid_trader_account_id: 99, ..Default::default() };
    rebuild_position_book(&flat, 99, &positions);
    assert_eq!(
        cancels(orphan_cancel_commands(&positions, &orders)),
        vec![(601, "adopt-sl".to_string()), (602, "adopt-tp".to_string())]
    );
}

/// A RESTART adopts the node's resting STOP-LIMIT bound to an open position it faces, exactly as it adopts a
/// stop, and as the position's STOP: it covers the position (a new stop is refused as covered), and when an
/// answer read in full no longer lists the position the bridge cancels it. Unadopted, it would rest past its
/// position and OPEN one under the reused id when the market reached it. A stop-limit on a position that is not
/// open, or on the position's own side, is left alone as a stop is.
#[test]
fn a_restart_adopts_a_bound_stop_limit_as_the_positions_stop_and_a_flat_answer_orphans_it() {
    let res = ProtoOaReconcileRes {
        ctid_trader_account_id: 99,
        position: vec![wire_open(7, 100_000)],
        order: vec![
            wire_order(
                "adopt-sl",
                611,
                ProtoOaOrderType::StopLimit,
                ProtoOaTradeSide::Sell,
                Some(7),
            ),
            wire_order("plain", 612, ProtoOaOrderType::StopLimit, ProtoOaTradeSide::Sell, Some(99)),
            wire_order("adds", 613, ProtoOaOrderType::StopLimit, ProtoOaTradeSide::Buy, Some(7)),
        ],
        ..Default::default()
    };
    let positions: PositionsByIdShared = Arc::new(Mutex::new(PositionsById::default()));
    rebuild_position_book(&res, 99, &positions);
    adopt_bound_exits(&res, 99, &SymbolMap::default(), &positions);
    {
        let mut book = positions.lock().expect("lock");
        let stop_loss = book.bound_exit("adopt-sl").expect("the stop-limit is adopted");
        assert_eq!(
            (stop_loss.position_id, stop_loss.kind, stop_loss.volume),
            (7, ExitKind::Stop, 100_000)
        );
        for not_adopted in ["plain", "adds"] {
            assert_eq!(book.bound_exit(not_adopted), None, "{not_adopted}");
        }
        let free = book.for_symbol(1);
        assert_eq!(
            book.bind_exit_among("sl-2", -1, ExitKind::Stop, 100_000, &free),
            Err(crate::execution::positions::ExitRefusal::Covered),
            "the adopted stop-limit covers the position as its stop"
        );
    }

    let orders = order_ids(&[("adopt-sl", 611)]);
    let flat = ProtoOaReconcileRes { ctid_trader_account_id: 99, ..Default::default() };
    rebuild_position_book(&flat, 99, &positions);
    assert_eq!(
        cancels(orphan_cancel_commands(&positions, &orders)),
        vec![(611, "adopt-sl".to_string())]
    );
}

#[test]
fn connection_config_debug_redacts_secrets() {
    let config = ConnectionConfig::new(
        "demo.ctraderapi.com",
        5035,
        "my-client-id",
        "super-secret-client-secret",
        "super-secret-access-token",
    );
    let debug = format!("{config:?}");
    assert!(!debug.contains("super-secret-client-secret"), "leaked client_secret: {debug}");
    assert!(!debug.contains("super-secret-access-token"), "leaked access_token: {debug}");
    assert!(debug.contains("my-client-id"), "client_id should stay visible: {debug}");
    assert!(debug.contains("<redacted>"));
    assert!(debug.contains("refresh_token: None"), "unset refresh_token stays None: {debug}");
}

#[test]
fn connection_config_debug_redacts_refresh_token_when_set() {
    let mut config = ConnectionConfig::new(
        "demo.ctraderapi.com",
        5035,
        "my-client-id",
        "super-secret-client-secret",
        "super-secret-access-token",
    );
    config.refresh_token = Some(Secret::new("super-secret-refresh-token".to_string()));
    let debug = format!("{config:?}");
    assert!(!debug.contains("super-secret-refresh-token"), "leaked refresh_token: {debug}");
    assert!(debug.contains("refresh_token: Some(Secret(<redacted>))"), "{debug}");
}

/// Build an `ERROR_RES`-typed `ProtoMessage` carrying `error_code`, for [`is_auth_error`]
/// unit tests below. The full network round-trip (`transport::oauth::refresh` against a real cTrader
/// OAuth endpoint) is NOT exercised here — it can't run offline — and stays covered only by
/// the live smoke test (`tests/ctrader_demo_smoke.rs`); this covers just the pure classifier.
fn error_res_message(error_code: &str) -> ProtoMessage {
    let err = ProtoOaErrorRes { error_code: error_code.to_string(), ..Default::default() };
    ProtoMessage {
        payload_type: pt::ERROR_RES,
        payload: Some(err.encode_to_vec()),
        client_msg_id: None,
    }
}

#[test]
fn is_auth_error_true_for_the_venues_names_of_a_refused_grant() {
    for code in ["OA_AUTH_TOKEN_EXPIRED", "CH_ACCESS_TOKEN_INVALID", "CH_CLIENT_AUTH_FAILURE"] {
        assert!(is_auth_error(&error_res_message(code)), "{code}");
    }
}

/// Exact names of the venue's two enums, never a substring: `CH_CLIENT_ALREADY_AUTHENTICATED` (a second
/// application auth, benign) carries `AUTH`, and a name the enums do not hold is no refused grant.
#[test]
fn is_auth_error_false_for_unrelated_error_code() {
    for code in [
        "SYMBOL_NOT_FOUND",
        "ORDER_REJECTED",
        "CH_CLIENT_ALREADY_AUTHENTICATED",
        "ACCESS_TOKEN_INVALID",
        "ch_access_token_invalid",
    ] {
        assert!(!is_auth_error(&error_res_message(code)), "{code}");
    }
}

#[test]
fn is_auth_error_false_for_non_error_res_payload_type() {
    // Only `ERROR_RES` frames are ever classified — any other payload type is never an auth
    // error regardless of what bytes happen to be in its payload.
    let mut msg = error_res_message("OA_AUTH_TOKEN_EXPIRED");
    msg.payload_type = pt::HEARTBEAT_EVENT;
    assert!(!is_auth_error(&msg));
}

#[test]
fn is_transient_retries_network_faults() {
    // A failed connect / reset / EOF-mid-handshake and a no-response timeout are the blips the
    // bounded connect-retry rides out.
    assert!(
        ConnectionError::Io(io::Error::new(io::ErrorKind::ConnectionReset, "reset")).is_transient()
    );
    assert!(
        ConnectionError::Io(io::Error::new(io::ErrorKind::UnexpectedEof, "closed mid-handshake"))
            .is_transient()
    );
    assert!(ConnectionError::Timeout(pt::APPLICATION_AUTH_RES).is_transient());
}

#[test]
fn is_transient_fails_fast_on_permanent_faults() {
    // Auth rejection is the crux permanent case — a bad credential never becomes good by
    // waiting, so the retry must NOT burn attempts on it.
    assert!(
        !ConnectionError::Venue {
            error_code: "CH_CLIENT_AUTH_FAILURE".into(),
            description: "invalid client credentials".into(),
        }
        .is_transient()
    );
    assert!(!ConnectionError::NoAccounts.is_transient());
    assert!(!ConnectionError::Decode("schema mismatch".into()).is_transient());
    assert!(!ConnectionError::Tls("rustls config".into()).is_transient());
}

/// A start's failure is asked again exactly when a later start can pass it (`ErrorKind::is_terminal` false): a
/// connect (or the token refresh it spent) that never got its answer, the venue's own "not now", a code the venue's table
/// does not name (never final), the FX week's close.
/// A refused grant and anything this bridge cannot read park.
#[test]
fn a_failure_a_later_start_can_pass_is_the_only_kind_that_is_not_terminal() {
    let venue = |code: &str| ConnectionError::Venue {
        error_code: code.to_string(),
        description: String::new(),
    };
    let refused = ConnectionError::Io(io::Error::new(io::ErrorKind::ConnectionRefused, "refused"));
    for (error, kind) in [
        (refused, ErrorKind::Network),
        (ConnectionError::TokenRefreshUnreachable(ErrorKind::Network), ErrorKind::Network),
        (ConnectionError::TokenRefreshUnreachable(ErrorKind::Timeout), ErrorKind::Timeout),
        (ConnectionError::TokenRefreshUnreachable(ErrorKind::ServerError), ErrorKind::ServerError),
        (ConnectionError::Timeout(pt::APPLICATION_AUTH_RES), ErrorKind::Timeout),
        (venue("SERVER_IS_UNDER_MAINTENANCE"), ErrorKind::VenueMaintenance),
        (venue("CH_SERVER_NOT_REACHABLE"), ErrorKind::ServerError),
        (venue("BLOCKED_PAYLOAD_TYPE"), ErrorKind::RateLimited),
        (venue("CONNECTIONS_LIMIT_EXCEEDED"), ErrorKind::RateLimited),
        (venue("SOME_BROKER_CODE"), ErrorKind::UnmappedCode),
        (ConnectionError::FxMarketClosed("closed".into()), ErrorKind::VenueMaintenance),
    ] {
        assert_eq!(error.kind(), kind, "{error}");
        assert!(!kind.is_terminal(), "{error}");
    }
    for error in [
        venue("CH_ACCESS_TOKEN_INVALID"),
        venue("CH_CLIENT_AUTH_FAILURE"),
        venue("RET_ACCOUNT_DISABLED"),
        venue("CH_CLIENT_ALREADY_AUTHENTICATED"),
        ConnectionError::NoAccounts,
        ConnectionError::Decode("schema mismatch".into()),
    ] {
        assert!(error.kind().is_terminal(), "{error} must park");
    }
    assert_eq!(venue("OA_AUTH_TOKEN_EXPIRED").kind(), ErrorKind::Auth);
}

#[test]
fn connect_retry_default_is_bounded_and_short() {
    // The default policy must stay bounded (so a dead venue ends the start) and cheap (a few
    // seconds total), or the "eventually give up, and the account does not run" contract regresses.
    let r = ConnectRetry::default();
    assert!(r.max_attempts >= 1);
    assert!(r.max_attempts <= 6, "keep the initial connect bounded");
    assert!(r.initial_backoff <= r.max_backoff);
    assert!(r.max_backoff <= Duration::from_secs(5), "cap the per-retry wait");
}

/// The three timings this connection runs on, as EFFECTIVE values. All three are the exec lane of
/// `venue_timings::CTRADER`: the actor's heartbeat tick (`HEARTBEAT_INTERVAL`, also the age the
/// recon client revives at), the wait `read_until` allows for EACH expected response (read inline
/// there) and the mid-session reconnect wait (`RECONNECT_WAIT`). `ConnectRetry`, `SEED_TIMEOUT` and
/// `RECONCILE_TIMEOUT` are not modelled in the table and are not pinned here. A retune of any of
/// the three is a deliberate edit to the literal.
#[test]
fn the_connection_runs_on_the_declared_timings() {
    assert_eq!(HEARTBEAT_INTERVAL, Duration::from_secs(10), "actor heartbeat tick");
    assert_eq!(
        venue_timings::CTRADER.exec().response_timeout(),
        Duration::from_secs(15),
        "the wait for each handshake response"
    );
    assert_eq!(RECONNECT_WAIT.initial, Duration::from_secs(1), "first reconnect wait (B-13)");
    assert_eq!(RECONNECT_WAIT.max, Duration::from_secs(5), "reconnect ceiling");
    assert_eq!(
        venue_timings::CTRADER.exec().dial_timeout(),
        Duration::from_secs(10),
        "the dial window of an ask with no ceiling (B-13)"
    );
}

/// What production reads and ticks at, whatever a test narrows: the two constructors differ only in the read.
#[test]
fn production_reads_the_long_read_and_an_execution_actor_alone_polls() {
    let prod = ConnectionConfig::new("venue", 5035, "cid", "secret", "token");
    assert_eq!(READ_TIMEOUT, Duration::from_secs(1), "a TLS handshake flight has this long");
    assert_eq!(
        EXECUTION_READ_POLL,
        Duration::from_millis(10),
        "the wake-up cost was measured at this"
    );
    assert_eq!(prod.read_timeout, READ_TIMEOUT);
    assert_eq!(prod.heartbeat_every, HEARTBEAT_INTERVAL);
    assert_eq!(super::actor::actor_read_timeout(&prod, true), EXECUTION_READ_POLL);
    assert_eq!(super::actor::actor_read_timeout(&prod, false), READ_TIMEOUT, "no new wake-ups");

    let addr: SocketAddr = "127.0.0.1:5035".parse().expect("addr");
    assert_eq!(ConnectionConfig::for_test(addr, "c", "s", "t").heartbeat_every, HEARTBEAT_INTERVAL);
}

/// **The dropped socket's reconnect waits, pinned verbatim before its doubling moved onto the one backoff** (connection
/// plan B3): the `backoff_ms` each failed attempt states, the wait it slept before it, 1 s doubling to the row's 5 s
/// ceiling (`RECONNECT_WAIT`), each step spread by up to half again under the ceiling (a row's ladder,
/// `vike_venues::reconnect`'s module doc). Real waits, 12 to 15.5 s in all: the loop sleeps the row's own numbers.
#[test]
fn the_reconnects_waits_are_pinned_verbatim() {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
    let addr = listener.local_addr().expect("addr");
    let (tx, rx) = mpsc::channel::<ConnectionCommand>();
    // Every dial is accepted and dropped at once (a transient failure); after the fourth the actor is told to stop.
    let peer = std::thread::spawn(move || {
        for _ in 0..4 {
            drop(listener.accept().expect("a dial"));
        }
        tx.send(ConnectionCommand::Shutdown).expect("the loop listens");
    });
    let mut config = ConnectionConfig::for_test(addr, "cid", "secret", "token");
    let (positions, closes): (PositionsByIdShared, CloseState) = Default::default();
    let lane = OrderLane { events: None, positions: &positions, closes: &closes };
    let mut lapse = SessionLapse::new(&config, false, 99);
    let mut pending = Vec::new();
    let (reopened, events) = vike_log::capture::captured(|| {
        reconnect_with_backoff(&mut config, 99, &rx, &mut pending, &mut lapse, &lane).is_some()
    });
    peer.join().expect("the peer");
    assert!(!reopened, "told to stop, never reopened");
    let waits: Vec<u64> = events
        .iter()
        .filter(|e| e.message == "reconnect attempt failed; retrying")
        .filter_map(|e| e.field("backoff_ms"))
        .map(|ms| ms.parse().expect("a whole number of milliseconds"))
        .collect();
    assert_eq!(waits.len(), 4, "four failed attempts: {waits:?}");
    for (wait, (step, most)) in
        waits.iter().zip([(1000, 1500), (2000, 3000), (4000, 5000), (5000, 5000)])
    {
        assert!((step..=most).contains(wait), "{wait} ms, for the step {step} ms: {waits:?}");
    }
}

/// **No handshake while the FX market is closed on the config's clock** (owner ruling 2026-11-13): a Saturday clock
/// refuses the login in the gate's words before anything is dialled (nothing listens at the address), a Wednesday
/// clock and no clock at all hold nothing.
#[test]
fn a_saturday_clock_refuses_the_handshake_before_anything_is_dialled() {
    let at = |stamp: &str| vike_model::time::parse_utc_timestamp_ms(stamp).expect("an instant");
    let addr: SocketAddr = "127.0.0.1:9".parse().expect("addr");
    let mut config = ConnectionConfig::for_test(addr, "cid", "secret", "token");
    assert_eq!(config.fx_closed_words(), None, "no clock, no gate");
    config.fx_clock = Some(Arc::new(move || at("2026-11-11T12:00:00Z")));
    assert_eq!(config.fx_closed_words(), None, "a Wednesday");
    config.fx_clock = Some(Arc::new(move || at("2026-11-14T12:00:00Z")));
    match open_and_handshake(&config, None).err() {
        Some(ConnectionError::FxMarketClosed(words)) => {
            assert_eq!(words, "FX market closed: login at 2026-11-15T21:00:00Z");
        }
        other => panic!("refused before the dial: {other:?}"),
    }
}

/// **A reconnect attempt that would land in the closed FX week is held** (`vike_venues::session::fx_login_held_pause`,
/// connection plan B3): two seconds before Friday's close the attempt a 5 s pause leads to is held and a 1 s one is
/// not; on the Saturday any is; two seconds before Sunday's open a 5 s one lands after it and is not; no clock holds
/// nothing. The words name the open.
#[test]
fn a_reconnect_attempt_that_would_land_in_the_closed_week_is_held() {
    let at = |stamp: &str| vike_model::time::parse_utc_timestamp_ms(stamp).expect("an instant");
    let addr: SocketAddr = "127.0.0.1:9".parse().expect("addr");
    let mut config = ConnectionConfig::for_test(addr, "cid", "secret", "token");
    let s = Duration::from_secs;
    assert_eq!(config.fx_held_words(s(5)), None, "no clock, no gate");
    let words = Some("FX market closed: login at 2026-11-15T21:00:00Z".to_string());
    config.fx_clock = Some(Arc::new(move || at("2026-11-13T21:59:58Z")));
    assert_eq!((config.fx_held_words(s(1)), config.fx_held_words(s(5))), (None, words.clone()));
    config.fx_clock = Some(Arc::new(move || at("2026-11-14T12:00:00Z")));
    assert_eq!(config.fx_held_words(s(1)), words);
    config.fx_clock = Some(Arc::new(move || at("2026-11-15T20:59:58Z")));
    assert_eq!(config.fx_held_words(s(5)), None, "it lands after the open");
}

/// Run the real actor loop as an exec socket for `hold` over a healthy peer, feeding it `commands` one by one with `gap`
/// between them, and return every frame the venue received. It is a real socket whose reads the actor itself sets.
fn frames_an_actor_writes(
    config_tune: impl FnOnce(&mut ConnectionConfig),
    commands: Vec<ConnectionCommand>,
    gap: impl Fn(usize) -> Duration,
    hold: Duration,
) -> Vec<ProtoMessage> {
    let (reader, peer, addr) = reader_whose_reads_are_scripted();
    let mut config = ConnectionConfig::for_test(addr, "cid", "secret", "token");
    config_tune(&mut config);
    let (tx, rx) = mpsc::channel();
    let venue = std::thread::spawn(move || {
        let (eof, _flag) = EofStream::new(peer);
        let mut venue = FrameReader::new(eof);
        let mut frames = Vec::new();
        while let Ok(frame) = venue.next_frame() {
            match frame {
                Some(msg) => frames.push(msg),
                None if venue.get_mut().eof.load(Ordering::SeqCst) => break,
                None => {}
            }
        }
        frames
    });
    let actor = std::thread::spawn(move || {
        let (events, _ingest) = vike_execution::event_channel(1024);
        super::actor::actor_loop(
            reader,
            &rx,
            config,
            99,
            Arc::new(SymbolMap::default()),
            2,
            Arc::new(vike_data::NoopSink),
            Some(events),
            Arc::new(Mutex::new(OrderVenueIdRegistry::default())),
            Arc::new(Mutex::new(PositionsById::default())),
            Arc::new(Mutex::new(CloseTracker::default())),
        );
    });
    for (i, cmd) in commands.into_iter().enumerate() {
        std::thread::sleep(gap(i));
        tx.send(cmd).expect("queue");
    }
    std::thread::sleep(hold);
    tx.send(ConnectionCommand::Shutdown).expect("queue shutdown");
    actor.join().expect("actor");
    venue.join().expect("venue")
}

/// **Each order goes out once and in the order it was sent**, whether it finds the read just begun, half over or just timed
/// out, and in bursts: the gaps walk across the poll.
#[test]
fn commands_reach_the_wire_once_each_and_in_order_across_the_short_read() {
    let sent: Vec<String> = (0..120).map(|i| format!("c-{i}")).collect();
    let commands = sent.iter().map(|order_client_id| new_order(order_client_id)).collect();
    let frames = frames_an_actor_writes(
        |_| {},
        commands,
        |i| Duration::from_millis(u64::try_from(i % 7).unwrap_or(0) * 3),
        Duration::from_millis(50),
    );
    let on_the_wire: Vec<String> = frames
        .iter()
        .filter(|msg| msg.payload_type == pt::NEW_ORDER_REQ)
        .filter_map(|msg| msg.client_msg_id.clone())
        .collect();
    assert_eq!(on_the_wire, sent);
}

/// **A hundred reads a second are not a hundred heartbeats.** The heartbeat and the probe keep their own clocks, and a wake-up
/// that found nothing due writes nothing.
#[test]
fn the_short_read_does_not_quicken_the_heartbeat_or_the_probe() {
    let frames = frames_an_actor_writes(
        |config| {
            config.heartbeat_every = Duration::from_millis(200);
            config.auth_probe_every = Duration::from_millis(300);
        },
        Vec::new(),
        |_| Duration::ZERO,
        Duration::from_millis(1250),
    );
    let count = |payload| frames.iter().filter(|msg| msg.payload_type == payload).count();
    let (heartbeats, probes) = (count(pt::HEARTBEAT_EVENT), count(pt::TRADER_REQ));
    assert!((3..=6).contains(&heartbeats), "{heartbeats} heartbeats in 1250 ms at one per 200 ms");
    assert!((2..=4).contains(&probes), "{probes} probes in 1250 ms at one per 300 ms");
    assert_eq!(frames.len(), heartbeats + probes, "a wake-up with nothing due writes nothing");
}

// ── an exit that spans positions: two legs on the wire, ONE order for the engine ────────────────────

/// The execution event of a leg, delivered as the actor delivers it: through `on_inbound`, so the event
/// the core receives is whatever the split-exit fold made of it.
fn deliver(
    kind: ProtoOaExecutionType,
    order: ProtoOaOrder,
    deal: Option<crate::transport::proto::ProtoOaDeal>,
    position: Option<ProtoOaPosition>,
    events: &vike_execution::EventSender,
    positions: &PositionsByIdShared,
    orders: &Arc<Mutex<OrderVenueIdRegistry<i64>>>,
) {
    let event = ProtoOaExecutionEvent {
        execution_type: kind as i32,
        order: Some(order),
        deal,
        position,
        ..Default::default()
    };
    let msg = ProtoMessage {
        payload_type: pt::EXECUTION_EVENT,
        payload: Some(event.encode_to_vec()),
        client_msg_id: None,
    };
    let closes: CloseState = Arc::new(Mutex::new(CloseTracker::default()));
    super::inbound::on_inbound(
        &msg,
        &SymbolMap::default(),
        2,
        &vike_data::NoopSink,
        Some(events),
        orders,
        positions,
        &closes,
    );
}

/// The `(kind, order client id)` of every event waiting on the lane.
fn lane(
    rx: &mut tokio::sync::mpsc::Receiver<vike_execution::Ingest>,
) -> Vec<(&'static str, String)> {
    let mut out = Vec::new();
    while let Ok(ingest) = rx.try_recv() {
        let vike_execution::Ingest::Event(event) = ingest else { continue };
        let (kind, order_client_id) = match &event {
            Event::Fill(e) => ("Fill", e.order_client_id.clone()),
            Event::OrderAccepted(e) => ("Accepted", e.order_client_id.clone()),
            Event::OrderPartiallyFilled(e) => ("Partial", e.order_client_id.clone()),
            Event::OrderFilled(e) => ("Filled", e.order_client_id.clone()),
            Event::OrderCanceled(e) => ("Canceled", e.order_client_id.clone()),
            Event::OrderRejected(e) => ("Rejected", e.order_client_id.clone()),
            Event::OrderModified(e) => ("Modified", e.order_client_id.clone()),
            _ => ("other", String::new()),
        };
        out.push((kind, order_client_id));
    }
    out
}

/// A closing deal of `volume` on `position_id` at 1.05.
fn closing_deal(
    deal_id: i64,
    order_id: i64,
    position_id: i64,
) -> crate::transport::proto::ProtoOaDeal {
    crate::transport::proto::ProtoOaDeal {
        deal_id,
        order_id,
        position_id,
        volume: 100_000,
        filled_volume: 100_000,
        symbol_id: 1,
        execution_timestamp: 1_784_000_000_000,
        execution_price: Some(1.05),
        trade_side: ProtoOaTradeSide::Sell as i32,
        ..Default::default()
    }
}

/// **The engine placed ONE order and must see ONE lifecycle for it.** A net-sized stop over the longs 7 and 8 rests
/// as two bound orders (`PL1`, `PL2`); their execution events reach the core as the parent `P`'s: one accept, a bare
/// fill and a partial fill for the first leg, a bare fill and the terminal fill for the second. Goes red if the
/// event sites stop folding (`emit_event`): the core would then receive events for order client ids it never placed.
#[test]
fn the_events_of_an_exit_that_spans_positions_reach_the_core_as_one_orders_lifecycle() {
    let positions: PositionsByIdShared = Arc::new(Mutex::new(PositionsById::default()));
    let orders = Arc::new(Mutex::new(OrderVenueIdRegistry::<i64>::default()));
    let res = ProtoOaReconcileRes {
        ctid_trader_account_id: 99,
        position: vec![wire_open(7, 100_000), wire_open(8, 100_000)],
        ..Default::default()
    };
    rebuild_position_book(&res, 99, &positions);
    {
        let mut book = positions.lock().expect("lock");
        let free = book.for_symbol(1);
        let legs = book
            .split_exit_among("P", -1, ExitKind::Stop, 200_000, &free, Default::default())
            .expect("the exit splits over the two positions");
        assert_eq!(
            legs.iter()
                .map(|l| (l.order_client_id.as_str(), l.position_id, l.volume))
                .collect::<Vec<_>>(),
            vec![("PL1", 7, 100_000), ("PL2", 8, 100_000)]
        );
    }
    let (events, mut rx) = vike_execution::event_channel(64);
    let leg = |order_client_id: &str, id: i64, position_id: i64| {
        wire_order(
            order_client_id,
            id,
            ProtoOaOrderType::Stop,
            ProtoOaTradeSide::Sell,
            Some(position_id),
        )
    };

    for (order_client_id, id, position_id) in [("PL1", 601, 7), ("PL2", 602, 8)] {
        let accept = ProtoOaExecutionType::OrderAccepted;
        deliver(
            accept,
            leg(order_client_id, id, position_id),
            None,
            None,
            &events,
            &positions,
            &orders,
        );
    }
    assert_eq!(lane(&mut rx), vec![("Accepted", "P".to_string())], "one accept for the exit");

    let mut fired = leg("PL1", 601, 7);
    fired.order_status = ProtoOaOrderStatus::OrderStatusFilled as i32;
    let filled = ProtoOaExecutionType::OrderFilled;
    let deal = Some(closing_deal(901, 601, 7));
    deliver(filled, fired, deal, Some(wire_gone(7)), &events, &positions, &orders);
    assert_eq!(
        lane(&mut rx),
        vec![("Fill", "P".to_string()), ("Partial", "P".to_string())],
        "the first leg fired: a fill and a partial fill of the exit"
    );

    let mut fired = leg("PL2", 602, 8);
    fired.order_status = ProtoOaOrderStatus::OrderStatusFilled as i32;
    let deal = Some(closing_deal(902, 602, 8));
    deliver(filled, fired, deal, Some(wire_gone(8)), &events, &positions, &orders);
    assert_eq!(
        lane(&mut rx),
        vec![("Fill", "P".to_string()), ("Filled", "P".to_string())],
        "the last leg fired: the exit is filled"
    );
    let book = positions.lock().expect("lock");
    assert_eq!(book.live_exit_legs("P"), None, "the group ended with its last leg");
    assert_eq!(
        (book.bound_exit("PL1"), book.bound_exit("PL2")),
        (None, None),
        "and so did the rows"
    );
}

/// Cancelling the exit cancels both legs; the engine hears ONE `OrderCanceled`, when the last leg is gone, and a leg
/// the venue cancelled alone (its position closed another way) while a sibling still rests narrows the exit to the size
/// the venue still backs (`OrderModified`, `crate::execution::exit_legs`).
#[test]
fn the_cancel_of_an_exit_that_spans_positions_is_one_cancel_after_the_last_leg() {
    let positions: PositionsByIdShared = Arc::new(Mutex::new(PositionsById::default()));
    let orders = Arc::new(Mutex::new(OrderVenueIdRegistry::<i64>::default()));
    let res = ProtoOaReconcileRes {
        ctid_trader_account_id: 99,
        position: vec![wire_open(7, 100_000), wire_open(8, 100_000)],
        ..Default::default()
    };
    rebuild_position_book(&res, 99, &positions);
    {
        let mut book = positions.lock().expect("lock");
        let free = book.for_symbol(1);
        book.split_exit_among("P", -1, ExitKind::Stop, 200_000, &free, Default::default())
            .expect("splits");
    }
    let (events, mut rx) = vike_execution::event_channel(64);
    let leg = |order_client_id: &str, id: i64, position_id: i64| {
        wire_order(
            order_client_id,
            id,
            ProtoOaOrderType::Stop,
            ProtoOaTradeSide::Sell,
            Some(position_id),
        )
    };
    for (order_client_id, id, position_id) in [("PL1", 601, 7), ("PL2", 602, 8)] {
        let accept = ProtoOaExecutionType::OrderAccepted;
        deliver(
            accept,
            leg(order_client_id, id, position_id),
            None,
            None,
            &events,
            &positions,
            &orders,
        );
    }
    let _ = lane(&mut rx);
    let cancelled = ProtoOaExecutionType::OrderCancelled;
    deliver(cancelled, leg("PL1", 601, 7), None, None, &events, &positions, &orders);
    assert_eq!(
        lane(&mut rx),
        vec![("Modified", "P".to_string())],
        "a sibling still rests: the exit is not over, it is narrowed"
    );
    deliver(cancelled, leg("PL2", 602, 8), None, None, &events, &positions, &orders);
    assert_eq!(lane(&mut rx), vec![("Canceled", "P".to_string())]);
}

// ── a symbol the broker changed: noted by the dispatcher, read again by the actor ───────────────────

/// `ProtoOASymbolChangedEvent` is NOTED on the map the dispatcher was given (the actor takes the ids between two
/// frames and re-reads their trading state): whether it arrives in the main loop or buffered inside another round
/// trip, it goes through `on_inbound`, so it cannot be lost. A repeated id is noted once and a second take is empty.
#[test]
fn a_symbol_changed_event_is_noted_for_the_actor_to_read_again() {
    let symbols = SymbolMap::default();
    let positions: PositionsByIdShared = Arc::new(Mutex::new(PositionsById::default()));
    let orders = Arc::new(Mutex::new(OrderVenueIdRegistry::<i64>::default()));
    let closes: CloseState = Arc::new(Mutex::new(CloseTracker::default()));
    let event = crate::transport::proto::ProtoOaSymbolChangedEvent {
        ctid_trader_account_id: 99,
        symbol_id: vec![1, 3, 1],
        ..Default::default()
    };
    let msg = ProtoMessage {
        payload_type: pt::SYMBOL_CHANGED_EVENT,
        payload: Some(event.encode_to_vec()),
        client_msg_id: None,
    };
    super::inbound::on_inbound(
        &msg,
        &symbols,
        2,
        &vike_data::NoopSink,
        None,
        &orders,
        &positions,
        &closes,
    );
    assert_eq!(symbols.take_changed(), vec![1, 3]);
    assert!(symbols.take_changed().is_empty(), "taken once");
}

/// **An order that ends leaves a data socket's list too.** That socket learns the pending orders at each reconnect and
/// the orders end on the exec socket, so a cancel used to leave its order listed, and every later reconnect of the data
/// socket warned of it as a gap (the CI box, 2026-11-09, `slp1109a`, cancelled minutes before). Each end removes it; an order
/// that has not ended is not added (the data socket keeps no order flow of its own).
#[test]
fn an_order_that_ends_leaves_the_list_of_a_socket_without_order_flow() {
    let positions: PositionsByIdShared = Arc::new(Mutex::new(PositionsById::default()));
    let closes: CloseState = Arc::new(Mutex::new(CloseTracker::default()));
    let event = |kind: ProtoOaExecutionType, order_client_id: &str| {
        let event_value = ProtoOaExecutionEvent {
            execution_type: kind as i32,
            order: Some(crate::transport::proto::ProtoOaOrder {
                order_id: 4242,
                client_order_id: Some(order_client_id.to_string()),
                ..Default::default()
            }),
            ..Default::default()
        };
        ProtoMessage {
            payload_type: pt::EXECUTION_EVENT,
            payload: Some(event_value.encode_to_vec()),
            client_msg_id: None,
        }
    };
    for end in [
        ProtoOaExecutionType::OrderCancelled,
        ProtoOaExecutionType::OrderFilled,
        ProtoOaExecutionType::OrderRejected,
        ProtoOaExecutionType::OrderExpired,
    ] {
        let orders = order_ids(&[("slp1109a", 4242)]);
        for msg in [event(ProtoOaExecutionType::OrderAccepted, "other"), event(end, "slp1109a")] {
            super::inbound::on_inbound(
                &msg,
                &SymbolMap::default(),
                2,
                &vike_data::NoopSink,
                None,
                &orders,
                &positions,
                &closes,
            );
        }
        assert!(
            orders.lock().expect("orders").order_client_ids().next().is_none(),
            "{end:?} ends it, and nothing is added"
        );
    }
}

// ── the swap a closing deal realized (A26) ──────────────────────────────────────────────────────────

/// Symbol 1 is EURUSD and the trader's deposit asset 101 is USD, as the handshake reads them.
fn symbols_with_a_usd_deposit() -> SymbolMap {
    SymbolMap::from_lists(&crate::instrument::symbols::SymbolLists {
        light: vec![crate::transport::proto::ProtoOaLightSymbol {
            symbol_id: 1,
            symbol_name: Some("EURUSD".to_string()),
            ..Default::default()
        }],
        assets: vec![crate::transport::proto::ProtoOaAsset {
            asset_id: 101,
            name: "USD".to_string(),
            ..Default::default()
        }],
        deposit_asset_id: Some(101),
        ..Default::default()
    })
}

/// The `ORDER_FILLED` of a closing market order: the deal carries a close detail with `swap` raw integers at
/// two money digits.
fn closing_fill_with_swap(deal_id: i64, swap: i64) -> ProtoOaExecutionEvent {
    let mut deal = closing_deal(deal_id, 900, 7);
    deal.close_position_detail = Some(crate::transport::proto::ProtoOaClosePositionDetail {
        entry_price: 1.04,
        gross_profit: 100,
        swap,
        commission: -12,
        balance: 1_000_000,
        money_digits: Some(2),
        ..Default::default()
    });
    ProtoOaExecutionEvent {
        execution_type: ProtoOaExecutionType::OrderFilled as i32,
        order: Some(wire_order(
            "mx",
            900,
            ProtoOaOrderType::Market,
            ProtoOaTradeSide::Sell,
            Some(7),
        )),
        deal: Some(deal),
        ..Default::default()
    }
}

fn funding_of(events: &[Event]) -> Vec<&vike_model::events::FundingEvent> {
    events
        .iter()
        .filter_map(|e| match e {
            Event::Funding(f) => Some(f),
            _ => None,
        })
        .collect()
}

/// A position that paid 1.37 USD of swap over its life closes: the engine hears the fill and then a funding event
/// of -1.37 under the deal's symbol in the deposit currency; the same deal delivered again books nothing more.
#[test]
fn the_swap_a_closing_deal_realized_is_booked_once_in_the_deposit_currency() {
    let symbols = symbols_with_a_usd_deposit();
    let closes: CloseState = Arc::new(Mutex::new(CloseTracker::default()));
    let event = closing_fill_with_swap(801, -137);

    let out = execution_event_to_canonical(&event, &symbols, 2, &closes);
    assert!(matches!(&out[0], Event::Fill(f) if f.trade_id.as_str() == "801"), "{out:?}");
    let funding = funding_of(&out);
    assert_eq!(funding.len(), 1, "{out:?}");
    assert_eq!((funding[0].symbol.as_str(), funding[0].amount), ("EURUSD", -1.37));
    assert_eq!(funding[0].currency.as_ref().map(|c| c.as_str()), Some("USD"));
    assert_eq!(funding[0].venue.as_str(), "ctrader");
    assert_eq!(funding[0].timestamp, 1_784_000_000_000);
    assert!(matches!(out.last(), Some(Event::Funding(_))), "after the fill: {out:?}");

    let again = execution_event_to_canonical(&event, &symbols, 2, &closes);
    assert!(funding_of(&again).is_empty(), "the same deal is booked once: {again:?}");
    let other =
        execution_event_to_canonical(&closing_fill_with_swap(802, 25), &symbols, 2, &closes);
    assert_eq!(
        funding_of(&other)[0].amount,
        0.25,
        "a credited swap is positive; another deal books"
    );
}

/// No swap, no close detail, no fill and no symbol each book nothing.
#[test]
fn a_deal_with_no_swap_to_book_books_none() {
    let symbols = symbols_with_a_usd_deposit();
    let closes: CloseState = Arc::new(Mutex::new(CloseTracker::default()));
    let zero = execution_event_to_canonical(&closing_fill_with_swap(811, 0), &symbols, 2, &closes);
    assert!(funding_of(&zero).is_empty(), "a zero swap: {zero:?}");

    let mut opening = closing_fill_with_swap(812, -137);
    opening.deal.as_mut().expect("a deal").close_position_detail = None;
    let out = execution_event_to_canonical(&opening, &symbols, 2, &closes);
    assert!(funding_of(&out).is_empty(), "an opening deal has no detail: {out:?}");

    let accepted = ProtoOaExecutionEvent {
        execution_type: ProtoOaExecutionType::OrderAccepted as i32,
        ..closing_fill_with_swap(813, -137)
    };
    let out = execution_event_to_canonical(&accepted, &symbols, 2, &closes);
    assert!(funding_of(&out).is_empty(), "no fill came out of this event: {out:?}");

    let event = closing_fill_with_swap(814, -137);
    let unnamed = execution_event_to_canonical(&event, &SymbolMap::default(), 2, &closes);
    assert!(funding_of(&unnamed).is_empty(), "a symbol the list lacks routes nowhere: {unnamed:?}");
}

/// A handshake that read no trader states no deposit currency: the event is booked unstated, which an
/// account-quoted engine takes as stated and a foreign-quoted one leaves out and counts.
#[test]
fn an_unknown_deposit_currency_leaves_the_currency_unstated() {
    let symbols = SymbolMap::from_lists(&crate::instrument::symbols::SymbolLists {
        light: vec![crate::transport::proto::ProtoOaLightSymbol {
            symbol_id: 1,
            symbol_name: Some("EURUSD".to_string()),
            ..Default::default()
        }],
        ..Default::default()
    });
    let closes: CloseState = Arc::new(Mutex::new(CloseTracker::default()));
    let out = execution_event_to_canonical(&closing_fill_with_swap(821, -50), &symbols, 2, &closes);
    let funding = funding_of(&out);
    assert_eq!((funding.len(), funding[0].amount, funding[0].currency.is_none()), (1, -0.5, true));
}

// ── what a frame says about the account session ──────────────────────────────

fn refusal(payload_type: u32, error_code: &str, description: &str) -> ProtoMessage {
    let payload = if payload_type == pt::ORDER_ERROR_EVENT {
        crate::transport::proto::ProtoOaOrderErrorEvent {
            error_code: error_code.to_string(),
            description: Some(description.to_string()),
            ..Default::default()
        }
        .encode_to_vec()
    } else {
        ProtoOaErrorRes {
            error_code: error_code.to_string(),
            description: Some(description.to_string()),
            ..Default::default()
        }
        .encode_to_vec()
    };
    ProtoMessage { payload_type, payload: Some(payload), client_msg_id: None }
}

/// The measured words first: `INVALID_REQUEST`, "Trading account is not authorized" (the CI box, 2026-11-06), which the
/// code rule alone read as a plain refusal. A token word is the renewal's, a session word a new session's, and a
/// refusal that is about neither is nobody's: `INVALID_REQUEST` is the proto's generic error.
#[test]
fn what_a_frame_says_about_the_session() {
    use SessionWord::{SessionGone, TokenRefused};
    let measured = "Trading account is not authorized";
    for (name, frame, want) in [
        ("measured", refusal(pt::ERROR_RES, "INVALID_REQUEST", measured), Some(SessionGone)),
        (
            "as an event",
            refusal(pt::ORDER_ERROR_EVENT, "INVALID_REQUEST", measured),
            Some(SessionGone),
        ),
        ("account", refusal(pt::ERROR_RES, "ACCOUNT_NOT_AUTHORIZED", ""), Some(SessionGone)),
        ("client", refusal(pt::ERROR_RES, "CH_CLIENT_NOT_AUTHENTICATED", ""), Some(SessionGone)),
        (
            "expired",
            refusal(pt::ERROR_RES, "CH_ACCESS_TOKEN_INVALID", "expired"),
            Some(TokenRefused),
        ),
        ("oa", refusal(pt::ERROR_RES, "OA_AUTH_TOKEN_EXPIRED", ""), Some(TokenRefused)),
        ("generic", refusal(pt::ERROR_RES, "INVALID_REQUEST", "volume is off the grid"), None),
        ("the code alone", refusal(pt::ERROR_RES, "INVALID_REQUEST", ""), None),
        ("money", refusal(pt::ERROR_RES, "NOT_ENOUGH_MONEY", "insufficient funds"), None),
        ("event", refusal(pt::ORDER_ERROR_EVENT, "TRADING_BAD_VOLUME", "off the grid"), None),
        ("other frame", refusal(pt::EXECUTION_EVENT, "CH_ACCESS_TOKEN_INVALID", measured), None),
    ] {
        assert_eq!(session_word(&frame), want, "{name}");
    }
    let event = |payload_type| ProtoMessage { payload_type, payload: None, client_msg_id: None };
    assert_eq!(session_word(&event(pt::ACCOUNT_DISCONNECT_EVENT)), Some(SessionGone));
    assert_eq!(session_word(&event(pt::ACCOUNTS_TOKEN_INVALIDATED_EVENT)), Some(TokenRefused));
    assert_eq!(session_word(&event(pt::HEARTBEAT_EVENT)), None);
    assert_eq!(session_word(&event(pt::SPOT_EVENT)), None);

    // The payload the probe and the reconcile read were answered on a logged-out socket (the demo, 2026-11-06 09:17 UTC),
    // spelled out on the wire: field 3 `errorCode`, field 4 `description`.
    let mut payload = vec![0x1a, 15];
    payload.extend_from_slice(b"INVALID_REQUEST");
    payload.extend_from_slice(&[0x22, 33]);
    payload.extend_from_slice(b"Trading account is not authorized");
    let frame =
        ProtoMessage { payload_type: pt::ERROR_RES, payload: Some(payload), client_msg_id: None };
    assert_eq!(session_word(&frame), Some(SessionGone));
}

/// **An order's refusal that lands inside another round trip is that order's.** Every request read there carries an
/// empty id, so an `ERROR_RES` with an id is not its answer: it used to end the round trip as "refused" and be dropped,
/// and the order vanished. It waits for the caller's dispatch; the round trip's own refusal still ends it.
#[test]
fn an_orders_refusal_inside_a_round_trip_is_kept_for_the_order() {
    use std::io::Write as _;

    let (mut reader, mut peer, _addr) = reader_whose_reads_are_scripted();
    let refused =
        ProtoOaErrorRes { error_code: "NOT_ENOUGH_MONEY".to_string(), ..Default::default() };
    peer.write_all(&framing::encode(pt::ERROR_RES, &refused.encode_to_vec(), "c-1"))
        .expect("write");
    peer.write_all(&framing::encode(pt::ERROR_RES, &refused.encode_to_vec(), "")).expect("write");

    let mut buffered = Vec::new();
    let answer = read_reply(&mut reader, &mut buffered, Duration::from_secs(5), pt::RECONCILE_RES);
    assert!(matches!(answer, Reply::Refused), "its own refusal ends the round trip");
    assert_eq!(buffered.len(), 1, "the order's was kept");
    assert_eq!(buffered[0].client_msg_id.as_deref(), Some("c-1"));
}

/// **The re-check of a fresh-stream swap**: an unsubscribe followed at once by a subscribe of the symbol is asked for
/// twice more, whatever the venue answered. A plain subscribe, a subscribe of another symbol and a subscribe long after
/// its release are no swap, and nothing is due before its time.
#[test]
fn a_swap_is_asked_for_again_after_the_queue_had_time_and_nothing_else_is() {
    let t0 = Instant::now();
    let after = |secs| t0 + Duration::from_secs(secs);
    let mut swap = SwapRecheck::default();

    swap.note(&ConnectionCommand::SubscribeSpots { symbol_id: 1 }, t0);
    assert!(swap.is_idle(), "a plain subscribe is no swap");
    swap.note(&ConnectionCommand::UnsubscribeSpots { symbol_id: 1 }, t0);
    swap.note(&ConnectionCommand::SubscribeSpots { symbol_id: 2 }, t0);
    assert!(swap.is_idle(), "another symbol is no swap");

    swap.note(&ConnectionCommand::SubscribeSpots { symbol_id: 1 }, t0);
    assert!(!swap.is_idle());
    assert_eq!(swap.take_due(after(4)), Vec::<i64>::new(), "not before its time");
    assert_eq!(swap.take_due(after(5)), [1]);
    assert_eq!(swap.take_due(after(6)), Vec::<i64>::new(), "once each");
    assert_eq!(swap.take_due(after(30)), [1]);
    assert!(swap.is_idle());

    swap.note(&ConnectionCommand::UnsubscribeSpots { symbol_id: 3 }, t0);
    swap.note(&ConnectionCommand::SubscribeSpots { symbol_id: 3 }, after(5));
    assert!(swap.is_idle(), "a subscribe long after its release is a new stream, not a swap");
}

/// **The answers already on the socket are read before it closes**, in the order they came: two orders written together on
/// a lapsed session get a refusal each, and the first one is what starts the reconnect. A re-auth's refusal is nobody's
/// order. Reddens on the read going, and on the re-auth's id reaching the engine as an order client id.
#[test]
fn the_answers_already_on_the_socket_are_read_before_it_closes() {
    use std::io::Write as _;

    let (mut reader, server_side, _addr) = reader_whose_reads_are_scripted();
    let refusal = |id: &str| {
        let err = ProtoOaErrorRes {
            error_code: "INVALID_REQUEST".to_string(),
            description: Some("Trading account is not authorized".to_string()),
            ..Default::default()
        };
        framing::encode(pt::ERROR_RES, &err.encode_to_vec(), id)
    };
    for id in ["c-stop", "aa-refresh", "c-take-profit"] {
        (&server_side).write_all(&refusal(id)).expect("script the answer");
    }
    let (events, mut rx) = vike_execution::event_channel(64);
    let positions: PositionsByIdShared = Arc::new(Mutex::new(PositionsById::default()));
    let orders = Arc::new(Mutex::new(OrderVenueIdRegistry::<i64>::default()));
    let closes: CloseState = Arc::new(Mutex::new(CloseTracker::default()));

    super::actor::read_what_is_left(
        &mut reader,
        &SymbolMap::default(),
        2,
        &vike_data::NoopSink,
        Some(&events),
        &orders,
        &positions,
        &closes,
    );

    let mut refused = Vec::new();
    while let Ok(ingest) = rx.try_recv() {
        if let vike_execution::Ingest::Event(Event::OrderRejected(r)) = ingest {
            refused.push(r.order_client_id);
        }
    }
    assert_eq!(refused, ["c-stop", "c-take-profit"]);
}

/// **A refusal of the bridge's OWN request is no order's reject**, in words that end no session too: the re-auth's and
/// the probe's ids name no order client id. The re-auth's used to reach the engine as an `OrderRejected` of an order client id it never placed.
/// An order's refusal beside them is still that order's.
#[test]
fn a_refusal_of_the_bridges_own_request_rejects_no_order() {
    let (events, mut rx) = vike_execution::event_channel(64);
    let positions: PositionsByIdShared = Arc::new(Mutex::new(PositionsById::default()));
    let orders = Arc::new(Mutex::new(OrderVenueIdRegistry::<i64>::default()));
    let closes: CloseState = Arc::new(Mutex::new(CloseTracker::default()));
    let err = ProtoOaErrorRes {
        error_code: "INVALID_REQUEST".to_string(),
        description: Some("refused".to_string()),
        ..Default::default()
    };
    for id in [REAUTH_MESSAGE_ID, PROBE_MESSAGE_ID, "c-1"] {
        let msg = ProtoMessage {
            payload_type: pt::ERROR_RES,
            payload: Some(err.encode_to_vec()),
            client_msg_id: Some(id.to_string()),
        };
        assert_eq!(session_word(&msg), None, "{id}: words that end no session");
        super::inbound::on_inbound(
            &msg,
            &SymbolMap::default(),
            2,
            &vike_data::NoopSink,
            Some(&events),
            &orders,
            &positions,
            &closes,
        );
    }
    let mut told = Vec::new();
    while let Ok(ingest) = rx.try_recv() {
        if let vike_execution::Ingest::Event(event) = ingest {
            told.push(event);
        }
    }
    assert!(
        matches!(told.as_slice(), [Event::OrderRejected(r)] if r.order_client_id == "c-1"),
        "only the order's refusal reached the engine: {told:?}"
    );
}

// ── what the venue's refusal of an order request ends (`crate::transport::error_codes`) ──────

/// The broker's words for an order it will not take, MEASURED on the hedged demo account on 2026-10-27, each an
/// `ORDER_ERROR_EVENT`, in
/// `crates/bridges/ctrader/CLAUDE.md`'s "What the broker answered on the hedged demo account, 2026-10-27 and 2026-10-28 (v0.1.96)".
/// The samples of these four rows of `crate::transport::error_codes::TAXONOMY`.
const MEASURED_REFUSALS: [(&str, &str); 4] = [
    ("MARKET_CLOSED", "Trading is not available: Market is closed."),
    ("TRADING_DISABLED", "Trading is disabled"),
    ("TRADING_BAD_VOLUME", "must be multiple of volume step"),
    ("TRADING_BAD_STOPS", "New SL for BUY pending order should be < entry price"),
];

/// An `ERROR_RES` or an `ORDER_ERROR_EVENT` refusing the request `client_msg_id` names (`order_id` on the event only).
fn order_refusal_frame(
    payload_type: u32,
    client_msg_id: &str,
    order_id: Option<i64>,
    error_code: &str,
    description: &str,
) -> ProtoMessage {
    let payload = if payload_type == pt::ORDER_ERROR_EVENT {
        crate::transport::proto::ProtoOaOrderErrorEvent {
            error_code: error_code.to_string(),
            order_id,
            description: Some(description.to_string()),
            ..Default::default()
        }
        .encode_to_vec()
    } else {
        ProtoOaErrorRes {
            error_code: error_code.to_string(),
            description: Some(description.to_string()),
            ..Default::default()
        }
        .encode_to_vec()
    };
    ProtoMessage {
        payload_type,
        payload: Some(payload),
        client_msg_id: Some(client_msg_id.to_string()),
    }
}

/// Dispatch `frames` through the inbound path over `orders` and `closes`: what reached the engine.
fn what_the_refusals_tell_the_engine(
    frames: &[ProtoMessage],
    orders: &Arc<Mutex<OrderVenueIdRegistry<i64>>>,
    closes: &CloseState,
) -> Vec<Event> {
    let (events, mut rx) = vike_execution::event_channel(64);
    let positions: PositionsByIdShared = Arc::new(Mutex::new(PositionsById::default()));
    for msg in frames {
        super::inbound::on_inbound(
            msg,
            &SymbolMap::default(),
            2,
            &vike_data::NoopSink,
            Some(&events),
            orders,
            &positions,
            closes,
        );
    }
    let mut told = Vec::new();
    while let Ok(ingest) = rx.try_recv() {
        if let vike_execution::Ingest::Event(event) = ingest {
            told.push(event);
        }
    }
    told
}

/// A tracker holding `close-1` on position 7 with nothing filled, and `close-2` on position 8 half filled.
fn two_closes_in_flight() -> CloseState {
    let closes: CloseState = Arc::new(Mutex::new(CloseTracker::default()));
    {
        let mut tracker = closes.lock().expect("closes");
        tracker.register("close-1", &[(7, 100)]);
        tracker.register("close-2", &[(8, 100)]);
        tracker.on_event("close-2", 50);
    }
    closes
}

/// **A refused close ends through its tracker, as an event as much as an `ERROR_RES`** (CT-2). The `ORDER_ERROR_EVENT`
/// arm was not given the tracker: a refused close was a plain `OrderRejected`, the illegal `PartiallyFilled -> Rejected`
/// after a filled leg, and the tracker kept the position, so every later reduce of it was refused "already closing in
/// flight" for the socket's life. A refused submit is still its terminal reject. Every measured refusal, both frames.
#[test]
fn a_refused_close_ends_through_its_tracker_and_a_refused_submit_is_rejected() {
    for (code, words) in MEASURED_REFUSALS {
        for payload_type in [pt::ORDER_ERROR_EVENT, pt::ERROR_RES] {
            let closes = two_closes_in_flight();
            let frames = ["c-1", "close-1", "close-2"]
                .map(|id| order_refusal_frame(payload_type, id, None, code, words));
            let told = what_the_refusals_tell_the_engine(&frames, &order_ids(&[]), &closes);
            let reason = format!("{code}: {words}");
            let refused = Some(vike_model::OrderRefusalId::VenueRefused.word());
            assert!(
                matches!(told.as_slice(), [
                    Event::OrderRejected(submit),
                    Event::OrderRejected(close),
                    Event::OrderCanceled(partly_filled),
                ] if submit.order_client_id == "c-1"
                    && submit.reason == reason
                    && submit.refusal_id.as_deref() == refused
                    && close.order_client_id == "close-1"
                    && close.reason == reason
                    && close.refusal_id.as_deref() == refused
                    && partly_filled.order_client_id == "close-2"
                    && partly_filled.reason == reason
                    && partly_filled.refusal_id.as_deref() == refused),
                "{code} as {payload_type}: {told:?}"
            );
            let tracker = closes.lock().expect("closes");
            assert!(
                !tracker.is_closing(7) && !tracker.is_closing(8),
                "{code} as {payload_type}: both positions may be closed again"
            );
        }
    }
}

/// **A code that leaves the write open ends nothing** (CT-1, decision 0303): `UNKNOWN_ERROR` "Generic error",
/// `CANT_ROUTE_REQUEST` "Connection to Server is lost", or a custom code may answer a request the venue went on to carry
/// out. Each used to be the terminal `OrderRejected` of a submit that may have landed. The submit and the close stay in
/// flight (the close in its tracker); a refused cancel and amend keep their non-terminal advisories, which say unknown.
#[test]
fn a_code_that_leaves_the_write_open_rejects_no_order_and_ends_no_close() {
    let unknown = Some(vike_model::OrderRefusalId::OutcomeUnknown.word());
    let amend = format!("{}c-resting", super::outbound::AMEND_MESSAGE_PREFIX);
    for code in ["UNKNOWN_ERROR", "CANT_ROUTE_REQUEST", "SOME_BROKER_CODE"] {
        for payload_type in [pt::ERROR_RES, pt::ORDER_ERROR_EVENT] {
            let closes = two_closes_in_flight();
            let frames = [
                order_refusal_frame(payload_type, "c-1", None, code, "words"),
                order_refusal_frame(payload_type, "close-1", None, code, "words"),
                order_refusal_frame(payload_type, "c-resting", Some(501), code, "words"),
                order_refusal_frame(payload_type, &amend, Some(501), code, "words"),
            ];
            let orders = order_ids(&[("c-resting", 501)]);
            let told = what_the_refusals_tell_the_engine(&frames, &orders, &closes);
            assert!(
                matches!(told.as_slice(), [
                    Event::OrderCancelRejected(cancel),
                    Event::OrderModifyRejected(modify),
                ] if cancel.order_client_id == "c-resting"
                    && cancel.refusal_id.as_deref() == unknown
                    && modify.order_client_id == "c-resting"
                    && modify.refusal_id.as_deref() == unknown
                    && modify.reason.starts_with("modify outcome unknown")),
                "{code} as {payload_type}: {told:?}"
            );
            let tracker = closes.lock().expect("closes");
            assert!(tracker.is_closing(7), "{code} as {payload_type}: the close stays in flight");
        }
    }
}

/// Run the real actor loop to its end over `reader` with `commands` queued: what it told the engine, as `(kind, order client id, reason)`.
fn what_an_ending_actor_tells_the_engine(
    reader: Reader,
    addr: SocketAddr,
    commands: Vec<ConnectionCommand>,
) -> Vec<(&'static str, String, String)> {
    let (tx, rx) = mpsc::channel();
    for cmd in commands {
        tx.send(cmd).expect("queue");
    }
    let (events, mut ingest) = vike_execution::event_channel(64);
    super::actor::actor_loop(
        reader,
        &rx,
        ConnectionConfig::for_test(addr, "cid", "secret", "token"),
        99,
        Arc::new(SymbolMap::default()),
        2,
        Arc::new(vike_data::NoopSink),
        Some(events),
        Arc::new(Mutex::new(OrderVenueIdRegistry::default())),
        Arc::new(Mutex::new(PositionsById::default())),
        Arc::new(Mutex::new(CloseTracker::default())),
    );
    let mut told = Vec::new();
    while let Ok(ingest) = ingest.try_recv() {
        let vike_execution::Ingest::Event(event) = ingest else { continue };
        match event {
            Event::OrderRejected(r) => {
                told.push(("rejected", r.order_client_id, r.reason.to_string()))
            }
            Event::OrderModifyRejected(r) => {
                told.push(("amend refused", r.order_client_id, r.reason.to_string()))
            }
            Event::OrderCancelRejected(r) => {
                told.push(("cancel refused", r.order_client_id, r.reason.to_string()))
            }
            _ => {}
        }
    }
    told
}

fn new_order(order_client_id: &str) -> ConnectionCommand {
    ConnectionCommand::NewOrder(crate::transport::proto::ProtoOaNewOrderReq {
        client_order_id: Some(order_client_id.into()),
        ..Default::default()
    })
}

/// **A `Shutdown` that finds the actor inside its reconnect ends every order command by name**, held or still queued behind
/// it: they used to go down with the actor and no event, which breaks "no order may silently vanish". A refused cancel and
/// amend say the order rests. Reddens on `Held::look`'s `refuse_all` going, and on its `refuse_the_queue` going.
#[test]
fn a_shutdown_inside_a_reconnect_refuses_every_order_command_by_name() {
    let (reader, _peer, addr) = reader_whose_writes_fail();
    let told = what_an_ending_actor_tells_the_engine(
        reader,
        addr,
        vec![
            new_order("c-first"), // its write fails, which starts the reconnect that holds the rest
            new_order("c-held"),
            ConnectionCommand::ClosePosition {
                position_id: 7,
                volume: 100,
                order_client_id: "c-close".into(),
            },
            ConnectionCommand::AmendOrder {
                order_client_id: "c-amend".into(),
                req: Default::default(),
            },
            ConnectionCommand::CancelOrder { order_id: 5, order_client_id: "c-cancel".into() },
            ConnectionCommand::Shutdown,
            new_order("c-behind"),
        ],
    );
    let kinds: Vec<(&str, &str)> =
        told.iter().map(|(kind, order_client_id, _)| (*kind, order_client_id.as_str())).collect();
    assert_eq!(
        kinds,
        [
            ("rejected", "c-first"),
            ("rejected", "c-held"),
            ("rejected", "c-close"),
            ("amend refused", "c-amend"),
            ("cancel refused", "c-cancel"),
            ("rejected", "c-behind"),
        ]
    );
    for (_, order_client_id, why) in &told[1..] {
        assert!(
            why.contains("was not sent") && why.contains("still rests"),
            "{order_client_id}: {why}"
        );
    }
}

/// **The other way an actor ends on a `Shutdown`, between two reads on a working socket**: what was sent behind it is
/// refused too. Reddens on the drain loop's `refuse_the_queue` going.
#[test]
fn a_shutdown_on_a_working_socket_refuses_what_was_sent_behind_it() {
    let (reader, _peer, addr) = reader_whose_reads_are_scripted();
    let told = what_an_ending_actor_tells_the_engine(
        reader,
        addr,
        vec![
            ConnectionCommand::Shutdown,
            new_order("c-behind"),
            ConnectionCommand::CancelOrder { order_id: 5, order_client_id: "c-cancel".into() },
        ],
    );
    let kinds: Vec<(&str, &str)> =
        told.iter().map(|(kind, order_client_id, _)| (*kind, order_client_id.as_str())).collect();
    assert_eq!(kinds, [("rejected", "c-behind"), ("cancel refused", "c-cancel")]);
}

/// **An actor whose loop panicked answers what it owes** (decision 0302): the cancel it was writing is of unknown
/// outcome, every order command queued behind it (or sent later) is refused as never sent, a positions read is dropped
/// (its asker reads `false`), the account's link reads failed, and a `Shutdown` ends the thread.
#[test]
fn a_dead_actor_answers_the_cancel_it_was_writing_as_unknown_and_every_later_order_command_unsent()
{
    use vike_bridge_core::exec_actor::{DEAD_THREAD_REASON, DIED_SENDING_REASON, Sending, sending};
    let route_key = "ctrader#DEAD-ACTOR";
    let (lane, mut ingest) = vike_execution::event_channel(16);
    let events = lane.routed(route_key);
    let positions: PositionsByIdShared = Arc::new(Mutex::new(PositionsById::default()));
    let closes: CloseState = Arc::new(Mutex::new(CloseTracker::default()));
    let (tx, rx) = mpsc::channel();
    let (reply, asked) = mpsc::channel();
    for cmd in [
        new_order("c-new"),
        ConnectionCommand::ReadPositions { reply },
        ConnectionCommand::AmendOrder {
            order_client_id: "c-amend".into(),
            req: Default::default(),
        },
        ConnectionCommand::Shutdown,
    ] {
        tx.send(cmd).expect("queue");
    }
    // The panic the actor's thread caught, inside the write of a cancel (`actor_loop` marks it the same way).
    let died = std::panic::catch_unwind(|| {
        sending(Sending::Cancels(vec!["c-cancel".into()]), || -> bool {
            panic!("a bug in the encoder")
        })
    });
    assert!(died.is_err());
    let order_lane = OrderLane { events: Some(&events), positions: &positions, closes: &closes };
    super::actor::answer_dead_actor(&rx, &order_lane, &std::sync::atomic::AtomicBool::new(false));

    let mut told = Vec::new();
    while let Ok(ingest) = ingest.try_recv() {
        let vike_execution::Ingest::Event(event) = ingest else { continue };
        told.push(match event {
            Event::OrderCancelRejected(r) => {
                ("cancel refused", r.order_client_id, r.reason.to_string(), r.refusal_id)
            }
            Event::OrderRejected(r) => {
                ("rejected", r.order_client_id, r.reason.to_string(), r.refusal_id)
            }
            Event::OrderModifyRejected(r) => {
                ("amend refused", r.order_client_id, r.reason.to_string(), r.refusal_id)
            }
            other => panic!("only answers, got {other:?}"),
        });
    }
    let (unknown, unsent) = (DIED_SENDING_REASON.to_string(), DEAD_THREAD_REASON.to_string());
    let id =
        |refusal: vike_model::OrderRefusalId| vike_model::events::refusal_id_of(refusal.word());
    let (outcome_unknown, not_sent) =
        (id(vike_model::OrderRefusalId::OutcomeUnknown), id(vike_model::OrderRefusalId::NotSent));
    assert_eq!(
        told,
        [
            ("cancel refused", "c-cancel".to_string(), unknown, outcome_unknown),
            ("rejected", "c-new".to_string(), unsent.clone(), not_sent.clone()),
            ("amend refused", "c-amend".to_string(), unsent, not_sent),
        ]
    );
    assert!(asked.recv().is_err(), "the positions read was dropped: its asker reads `false`");
    assert_eq!(
        vike_bridge_core::venue_link::venue_link(route_key).state().connection_state(),
        vike_model::feed_status::DataClientConnectionState::Error,
        "the account's link reads failed"
    );
}

/// **What a held command may become, per kind** (`expires_while_held`'s doc argues each): an order and an amend stop being
/// worth sending at the bound; a close and a cancel never do, and what carries no order is not an order's to refuse. Reddens
/// on any kind moving.
#[test]
fn only_an_order_and_an_amend_expire_while_held() {
    let amend =
        ConnectionCommand::AmendOrder { order_client_id: "c".into(), req: Default::default() };
    assert!(expires_while_held(&ConnectionCommand::NewOrder(Default::default())));
    assert!(expires_while_held(&amend));

    let (reply, _answer) = mpsc::channel();
    for kept in [
        ConnectionCommand::ClosePosition {
            position_id: 7,
            volume: 100,
            order_client_id: "c".into(),
        },
        ConnectionCommand::CancelOrder { order_id: 1, order_client_id: "c".into() },
        ConnectionCommand::SubscribeSpots { symbol_id: 1 },
        ConnectionCommand::UnsubscribeSpots { symbol_id: 1 },
        ConnectionCommand::ReadPositions { reply },
    ] {
        assert!(!expires_while_held(&kept));
    }
}

/// `trade health`'s words name the bound the actor keeps, and the words of a state are not another state's.
#[test]
fn the_words_about_orders_name_the_bound_and_keep_their_states_apart() {
    let bound = format!("{} s", HOLD_ORDERS_FOR.as_secs());
    assert!(WAITING_FOR_A_SESSION.contains(&bound), "{WAITING_FOR_A_SESSION} vs {bound}");
    assert!(!WAITING_FOR_A_SESSION.contains("every order"));
    assert!(GRANT_IS_DEAD.contains("refused by name") && !GRANT_IS_DEAD.contains("waits"));
    assert!(ON_THE_SAME_SOCKET.contains("go out") && !ON_THE_SAME_SOCKET.contains("waits"));
}

// ── how the actor lets go of a socket: a dead one is ABORTED, one it leaves while the way works is CLOSED ────────────

/// [`reader_whose_reads_are_scripted`] plus a second handle on the actor's own socket, to read its options after the actor
/// has let go of its handle (the socket lives while either handle does).
fn reader_watched() -> (Reader, TcpStream, TcpStream, SocketAddr) {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
    let addr = listener.local_addr().expect("addr");
    let client = TcpStream::connect(addr).expect("connect");
    let (peer, _) = listener.accept().expect("accept");
    let watch = client.try_clone().expect("a second handle on the socket");
    let (eof, _flag) = EofStream::new(ConnectionSocket::Plain(client));
    (FrameReader::new(eof), watch, peer, addr)
}

/// The real exec actor on `reader`, called dead after 300 ms with no answer. Nothing listens at `addr` any more, so its
/// reconnect is refused and it stays inside it until the returned sender's `Shutdown`.
fn an_actor_on_a_mute_venue(
    reader: Reader,
    addr: SocketAddr,
) -> (mpsc::Sender<ConnectionCommand>, std::thread::JoinHandle<()>) {
    let mut config = ConnectionConfig::for_test(addr, "cid", "secret", "token");
    config.read_idle_after = Duration::from_millis(300);
    let (tx, rx) = mpsc::channel();
    let actor = std::thread::spawn(move || {
        let (events, _ingest) = vike_execution::event_channel(64);
        super::actor::actor_loop(
            reader,
            &rx,
            config,
            99,
            Arc::new(SymbolMap::default()),
            2,
            Arc::new(vike_data::NoopSink),
            Some(events),
            Arc::new(Mutex::new(OrderVenueIdRegistry::default())),
            Arc::new(Mutex::new(PositionsById::default())),
            Arc::new(Mutex::new(CloseTracker::default())),
        );
    });
    (tx, actor)
}

/// Read the venue's end until the actor's end goes: `Ok` for a FIN (a graceful close), the error otherwise.
fn the_venue_reads_to_the_end(peer: &mut TcpStream) -> std::io::Result<()> {
    use std::io::Read as _;
    peer.set_read_timeout(Some(Duration::from_millis(100))).expect("timeout");
    let deadline = Instant::now() + Duration::from_secs(10);
    let mut buffer = [0_u8; 4096];
    while Instant::now() < deadline {
        match peer.read(&mut buffer) {
            Ok(0) => return Ok(()),
            Ok(_) => {}
            Err(e)
                if matches!(
                    e.kind(),
                    std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                ) => {}
            Err(e) => return Err(e),
        }
    }
    panic!("the actor never let go of the socket");
}

/// **A socket called dead at the idle bound is aborted: `SO_LINGER` 0, so the kernel drops what it holds unsent** (an
/// order written just before the death) instead of sending it when the path heals, after the reconnect ended it by name.
/// Read back through `socket2` on the actor's own socket. Reddens if the idle death closes gracefully.
#[test]
fn a_socket_called_dead_is_aborted_with_what_it_holds_unsent() {
    let (reader, watch, _peer, addr) = reader_watched();
    let linger = || socket2::SockRef::from(&watch).linger().expect("read SO_LINGER");
    assert_eq!(linger(), None, "a socket starts with the graceful close");
    let (tx, actor) = an_actor_on_a_mute_venue(reader, addr);
    let deadline = Instant::now() + Duration::from_secs(10);
    while linger().is_none() && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(20));
    }
    let after = linger();
    tx.send(ConnectionCommand::Shutdown).expect("stop");
    actor.join().expect("actor");
    assert_eq!(after, Some(Duration::ZERO));
}

/// **And it is let go AT ONCE, not when the reconnect has found another**: the venue sees the reset while the reconnect is
/// still refused, so the old socket does not go on retransmitting through the outage. A graceful close would be a FIN here,
/// and a socket kept until the reconnect succeeds would end nothing before the deadline.
#[test]
fn a_socket_called_dead_is_reset_before_the_reconnect_finds_another() {
    let (reader, mut peer, addr) = reader_whose_reads_are_scripted();
    let (tx, actor) = an_actor_on_a_mute_venue(reader, addr);
    let ended = the_venue_reads_to_the_end(&mut peer);
    tx.send(ConnectionCommand::Shutdown).expect("stop");
    actor.join().expect("actor");
    assert_eq!(ended.map_err(|e| e.kind()), Err(std::io::ErrorKind::ConnectionReset));
}

/// **A socket left while the way to the venue works closes gracefully**: the venue ended the account session, its refusal
/// arrived, so nothing waits for a path to heal. A FIN, and no `SO_LINGER` set. Reddens if every reconnect aborts.
#[test]
fn a_socket_whose_session_ended_closes_gracefully() {
    use std::io::Write as _;

    let (reader, watch, mut peer, addr) = reader_watched();
    let err = ProtoOaErrorRes {
        error_code: "INVALID_REQUEST".to_string(),
        description: Some("Trading account is not authorized".to_string()),
        ..Default::default()
    };
    peer.write_all(&framing::encode(pt::ERROR_RES, &err.encode_to_vec(), ""))
        .expect("the venue ends the session");
    let (tx, actor) = an_actor_on_a_mute_venue(reader, addr);
    let ended = the_venue_reads_to_the_end(&mut peer);
    let linger = socket2::SockRef::from(&watch).linger().expect("read SO_LINGER");
    tx.send(ConnectionCommand::Shutdown).expect("stop");
    actor.join().expect("actor");
    assert!(ended.is_ok(), "a FIN, not a reset: {ended:?}");
    assert_eq!(linger, None);
}

/// **A login hands its whole list to the kept list's slot** (`crate::catalog::hold`, decision 0062's decision 3): the
/// handshake already read every symbol, so the gatewayhub's catalog answers the operator's refresh from it and asks the
/// venue nothing. Before, only an order's symbol facts filled the slot, and a running account that had not traded left
/// `ctrader.json` unwritten (SYM-LISTS, 2026-11-16). The slot is process-wide and every handshake of this binary pushes a
/// fake's list, each with `EURUSD`, so the row is read, not the whole list.
#[test]
fn a_handshake_hands_its_whole_list_to_the_held_slot() {
    let server = crate::test_support::FakeCtrader::start_scripted();
    let config = ConnectionConfig::for_test(server.addr(), "cid", "secret", "token");
    let (_reader, _ctid, map, _digits) = open_and_handshake(&config, None).expect("handshake");
    assert!(map.id_of("EURUSD").is_some(), "the fake lists EURUSD");
    let held = vike_catalog::held_list::held("ctrader").expect("the login's list is held");
    assert!(
        held.iter().any(|row| row.venue == "ctrader" && row.raw_symbol == "EURUSD"),
        "{held:?}"
    );
}
