use super::inbound::update_position_map;
use super::reconcile::rebuild_position_book;
use super::session::{RECONNECT_WAIT, is_auth_error};
use super::*;
use crate::proto::{ProtoOaErrorRes, ProtoOaExecutionEvent, ProtoOaReconcileRes};

/// One OPEN wire position in symbol 1, for the position-book provenance tests below.
fn wire_open(position_id: i64, volume: i64) -> crate::proto::ProtoOaPosition {
    crate::proto::ProtoOaPosition {
        position_id,
        position_status: crate::proto::ProtoOaPositionStatus::PositionStatusOpen as i32,
        trade_data: crate::proto::ProtoOaTradeData {
            symbol_id: 1,
            volume,
            trade_side: crate::proto::ProtoOaTradeSide::Buy as i32,
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
/// `crates/bridges/ctrader/tests/exec_halt.rs`'s
/// `a_reconcile_answer_for_another_account_is_not_this_accounts_evidence` end to end.
#[test]
fn a_reconcile_answer_for_another_ctid_is_discarded_and_clears_the_evidence() {
    let positions: PositionMap = Arc::new(Mutex::new(PositionBook::default()));

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
    let positions: PositionMap = Arc::new(Mutex::new(PositionBook::default()));
    let res = ProtoOaReconcileRes {
        ctid_trader_account_id: 99,
        position: vec![wire_open(7, 100_000), wire_open(8, 40_000)],
        ..Default::default()
    };
    rebuild_position_book(&res, 99, &positions);
    assert!(positions.lock().expect("lock").is_fetched());

    // A CLOSED position is KNOWLEDGE: drop the entry, keep the evidence claim.
    let mut closed = wire_open(8, 0);
    closed.position_status = crate::proto::ProtoOaPositionStatus::PositionStatusClosed as i32;
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
    broken.position_status = crate::proto::ProtoOaPositionStatus::PositionStatusError as i32;
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

use super::inbound::{exec_event_to_canonical, update_bound_exits, update_order_map};
use super::outbound::{orphan_cancel_commands, restore_bound_exit_on_write_failure};
use super::reconcile::{adopt_bound_exits, forget_vanished_exits};
use crate::positions::{CloseTracker, ExitKind, TrackedPosition};
use crate::proto::{
    ProtoOaExecutionType, ProtoOaOrder, ProtoOaOrderStatus, ProtoOaOrderType, ProtoOaPosition,
    ProtoOaPositionStatus, ProtoOaTradeData, ProtoOaTradeSide,
};
use std::collections::{HashSet, VecDeque};
use vike_model::events::Event;

/// A resting or market order of symbol 1 as an execution event's order ref carries it.
fn wire_order(
    coid: &str,
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
            ..Default::default()
        },
        order_type: order_type as i32,
        order_status: ProtoOaOrderStatus::OrderStatusAccepted as i32,
        client_order_id: Some(coid.to_string()),
        position_id,
        ..Default::default()
    }
}

fn exec_ev(
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
fn apply(ev: &ProtoOaExecutionEvent, positions: &PositionMap, orders: &OrderIdMap) {
    update_position_map(ev, positions);
    update_bound_exits(ev, positions);
    update_order_map(ev, orders);
}

/// A hedged book holding the long 7, with a bound stop `sl` and take-profit `tp` whose accepts have
/// taught the order ids 501 and 502.
fn bracket_on_position_7() -> (PositionMap, OrderIdMap) {
    let positions: PositionMap = Arc::new(Mutex::new(PositionBook::default()));
    let orders: OrderIdMap = Arc::new(Mutex::new(HashMap::new()));
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
    for (coid, id, ty) in
        [("sl", 501, ProtoOaOrderType::Stop), ("tp", 502, ProtoOaOrderType::Limit)]
    {
        let order = wire_order(coid, id, ty, ProtoOaTradeSide::Sell, Some(7));
        apply(
            &exec_ev(ProtoOaExecutionType::OrderAccepted, Some(order), None),
            &positions,
            &orders,
        );
    }
    (positions, orders)
}

fn cancels(commands: Vec<Command>) -> Vec<(i64, String)> {
    let mut out: Vec<_> = commands
        .into_iter()
        .map(|c| match c {
            Command::CancelOrder { order_id, client_order_id } => (order_id, client_order_id),
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
        &exec_ev(ProtoOaExecutionType::OrderFilled, Some(close), Some(wire_gone(7))),
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
/// `exec_resting.rs`'s `the_engines_cancel_of_an_orphaned_exit_is_not_written_a_second_time`). The order of
/// the event itself is never cancelled: its terminal event ends its row.
#[test]
fn when_one_exit_fires_the_other_is_cancelled_and_the_one_that_fired_is_not() {
    let (positions, orders) = bracket_on_position_7();
    let mut fill = wire_order("sl", 501, ProtoOaOrderType::Stop, ProtoOaTradeSide::Sell, Some(7));
    fill.order_status = ProtoOaOrderStatus::OrderStatusFilled as i32;
    apply(
        &exec_ev(ProtoOaExecutionType::OrderFilled, Some(fill), Some(wire_gone(7))),
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
        let gone = exec_ev(ProtoOaExecutionType::OrderFilled, Some(close), Some(wire_gone(7)));
        apply(&gone, &positions, &orders);
        let first = cancels(orphan_cancel_commands(&positions, &orders));
        assert_eq!(first.len(), 2, "{how}: both exits are owed a cancel");
        assert!(orphan_cancel_commands(&positions, &orders).is_empty(), "{how}: written once");

        if how == "the write failed" {
            // What the actor does when `write_command` errs: the failure event the command built.
            let failed =
                Some(Event::OrderCancelRejected(vike_model::events::OrderCancelRejected {
                    client_order_id: "tp".into(),
                    reason: "cancel write failed; connection reset".into(),
                    ts: 0,
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
        &exec_ev(
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
    let tp = book.bound_exit("tp").expect("the take-profit is still bound");
    assert!(!tp.orphaned && !tp.cancelling);
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

/// ⚠ **An absence in a book that is not evidence is not a reason to cancel.** After an event carried a
/// position ref this build cannot classify (the entry is dropped and the book stops being evidence), the
/// next event about a LIVE bound exit must not orphan it for "its position is not in the book". A book the
/// venue answered for still does (`an_exit_accepted_after_its_position_vanished_...`).
#[test]
fn the_next_event_about_a_live_exit_does_not_cancel_it_after_an_unclassifiable_position_ref() {
    let (positions, orders) = bracket_on_position_7();
    let mut broken = wire_open(7, 100_000);
    broken.position_status = ProtoOaPositionStatus::PositionStatusError as i32;
    apply(&exec_ev(ProtoOaExecutionType::OrderReplaced, None, Some(broken)), &positions, &orders);
    assert!(!positions.lock().expect("lock").is_fetched(), "the claim to be evidence is withdrawn");
    assert!(positions.lock().expect("lock").position(7).is_none(), "and the entry is dropped");

    let live = wire_order("sl", 501, ProtoOaOrderType::Stop, ProtoOaTradeSide::Sell, Some(7));
    apply(&exec_ev(ProtoOaExecutionType::OrderReplaced, Some(live), None), &positions, &orders);
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
        &exec_ev(ProtoOaExecutionType::OrderFilled, Some(close), Some(wire_gone(7))),
        &positions,
        &orders,
    );
    assert_eq!(cancels(orphan_cancel_commands(&positions, &orders)).len(), 2);

    let refused = wire_order("tp", 502, ProtoOaOrderType::Limit, ProtoOaTradeSide::Sell, Some(7));
    apply(
        &exec_ev(ProtoOaExecutionType::OrderCancelRejected, Some(refused), None),
        &positions,
        &orders,
    );

    let book = positions.lock().expect("lock");
    assert!(book.position(7).is_none(), "the position is gone");
    let tp = book.bound_exit("tp").expect("still a row");
    assert!(!tp.orphaned && !tp.cancel_sent, "the refused cancel is given up: {tp:?}");
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

use super::stream::{EofStream, Stream};
use crate::framing::{self, FrameReader};

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
    let (eof, _flag) = EofStream::new(Stream::Plain(client));
    (FrameReader::new(eof), peer, addr)
}

/// Run the real actor loop over a reader whose writes fail, with `commands` queued and a `Shutdown` behind
/// them (the reconnect that the first failed write starts then ends the loop at once).
fn run_actor_over_a_dead_writer(
    commands: Vec<Command>,
    positions: &PositionMap,
    orders: &OrderIdMap,
) {
    let (reader, _peer, addr) = reader_whose_writes_fail();
    let (tx, rx) = mpsc::channel();
    for cmd in commands {
        tx.send(cmd).expect("queue");
    }
    tx.send(Command::Shutdown).expect("queue shutdown");
    let (events, _ingest) = vike_exec::event_channel(64);
    let closes: CloseState = Arc::new(Mutex::new(CloseTracker::default()));
    super::actor::actor_loop(
        reader,
        rx,
        ConnConfig::for_test(addr, "cid", "secret", "token"),
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
    let positions: PositionMap = Arc::new(Mutex::new(PositionBook::default()));
    let orders: OrderIdMap = Arc::new(Mutex::new(HashMap::new()));
    {
        let mut book = positions.lock().expect("lock");
        book.replace_all(
            [(7, TrackedPosition { symbol_id: 1, side: 1, volume: 100_000, open_ts: 1 })],
            1,
        );
        let free = book.for_symbol(1);
        book.bind_exit_among("sl-new", -1, ExitKind::Stop, 100_000, &free).expect("binds");
    }
    let order = crate::proto::ProtoOaNewOrderReq {
        client_order_id: Some("sl-new".into()),
        ..Default::default()
    };
    run_actor_over_a_dead_writer(vec![Command::NewOrder(order)], &positions, &orders);
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
        &exec_ev(ProtoOaExecutionType::OrderFilled, Some(close), Some(wire_gone(7))),
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
        &exec_ev(ProtoOaExecutionType::OrderFilled, Some(close), Some(wire_gone(7))),
        &positions,
        &orders,
    );
    let mut queue: VecDeque<Command> = orphan_cancel_commands(&positions, &orders).into();
    assert_eq!(queue.len(), 2, "both cancels queued, both marked written");
    assert!(positions.lock().expect("lock").exits_to_cancel().is_empty());
    assert!(positions.lock().expect("lock").is_fetched(), "evidence before the socket dies");

    super::actor::forget_the_dead_socket(&positions, &mut queue);

    assert!(queue.is_empty(), "a queue that predates the reconnect must not be written after it");
    let book = positions.lock().expect("lock");
    assert_eq!(book.exits_to_cancel().len(), 2, "both written cancels are owed again");
    assert!(!book.is_fetched(), "a dead socket's book is not evidence");
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
        &exec_ev(ProtoOaExecutionType::OrderFilled, Some(close), Some(wire_gone(7))),
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
        &HashSet::new(),
        &HashSet::new(),
        vec![Command::CancelOrder { order_id: 502, client_order_id: "tp".into() }],
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

/// After a reconnect the REAL reconcile entry point drops the rows of written orders that are not pending
/// any more (including one whose accept the order map never saw) and keeps the rest. Goes red if
/// `forget_vanished_exits` is deleted from `reconcile_after_reconnect`.
#[test]
fn the_reconnect_reconcile_drops_the_rows_of_exits_that_are_not_pending_any_more() {
    let positions: PositionMap = Arc::new(Mutex::new(PositionBook::default()));
    let orders: OrderIdMap = Arc::new(Mutex::new(HashMap::from([
        ("gone-known".to_string(), 1),
        ("still-pending".to_string(), 2),
    ])));
    {
        let mut book = positions.lock().expect("lock");
        for (coid, sent) in [
            ("gone-known", true),
            ("never-known", true),
            ("still-pending", true),
            ("queued", false),
        ] {
            book.bind_exit(coid, 7, ExitKind::Stop, 100_000);
            if sent {
                book.mark_sent(coid);
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
    let (eof, _flag) = EofStream::new(Stream::Plain(client));
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
        &exec_ev(ProtoOaExecutionType::OrderCancelRejected, Some(refused), None),
        &positions,
        &orders,
    );
    let mut book = positions.lock().expect("lock");
    assert!(!book.bound_exit("sl").expect("still bound").cancelling);
    let free = book.for_symbol(1);
    assert_eq!(
        book.bind_exit_among("sl2", -1, ExitKind::Stop, 100_000, &free),
        Err(crate::positions::ExitRefusal::Covered)
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
    apply(&exec_ev(ProtoOaExecutionType::OrderReplaced, Some(replaced), None), &positions, &orders);
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
    let positions: PositionMap = Arc::new(Mutex::new(PositionBook::default()));
    let orders: OrderIdMap = Arc::new(Mutex::new(HashMap::new()));
    {
        let mut book = positions.lock().expect("lock");
        // A book the venue has answered for (evidence), holding the long.
        book.replace_all(
            [(7, TrackedPosition { symbol_id: 1, side: 1, volume: 100_000, open_ts: 1 })],
            1,
        );
        let free = book.for_symbol(1);
        book.bind_exit_among("late", -1, ExitKind::Stop, 100_000, &free).expect("binds");
        book.remove(7); // the position closed before the venue answered
    }
    assert!(orphan_cancel_commands(&positions, &orders).is_empty(), "nothing orphaned yet");
    let order = wire_order("late", 777, ProtoOaOrderType::Stop, ProtoOaTradeSide::Sell, Some(7));
    apply(&exec_ev(ProtoOaExecutionType::OrderAccepted, Some(order), None), &positions, &orders);
    assert_eq!(
        cancels(orphan_cancel_commands(&positions, &orders)),
        vec![(777, "late".to_string())]
    );
}

/// ⚠ **An event belongs to a market close only if it carries NO client order id or the close's own.** A
/// resting exit bound to the same position references the same `positionId`, and its cancel (or fill)
/// during a `market-exit` must reach the engine under ITS coid, not terminalize the close's. Delete the
/// client-order-id test in `exec_event_to_canonical` and the first assert goes red.
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
    let cancelled = exec_ev(
        ProtoOaExecutionType::OrderCancelled,
        Some(wire_order("sl", 501, ProtoOaOrderType::Stop, ProtoOaTradeSide::Sell, Some(7))),
        None,
    );
    let out = exec_event_to_canonical(&cancelled, &symbols, 2, &closes);
    assert!(
        matches!(&out[..], [Event::OrderCanceled(c)] if c.client_order_id == "sl"),
        "the exit's cancel is the EXIT's: {out:?}"
    );
    assert!(closes.lock().expect("lock").is_tracked("mx"), "and the close is still in flight");

    // The close's own events carry its coid (the envelope id) or none: both fold, as before.
    for client_order_id in [Some("mx"), None] {
        let mut order =
            wire_order("ignored", 900, ProtoOaOrderType::Market, ProtoOaTradeSide::Sell, Some(7));
        order.client_order_id = client_order_id.map(str::to_string);
        let accepted = exec_ev(ProtoOaExecutionType::OrderAccepted, Some(order), None);
        let out = exec_event_to_canonical(&accepted, &symbols, 2, &in_flight());
        assert!(
            matches!(&out[..], [Event::OrderAccepted(a)] if a.client_order_id == "mx"),
            "{client_order_id:?}: the close's own accept is the close's: {out:?}"
        );
    }
}

/// The reconcile ADOPTS a resting stop or limit that sits on an OPEN position and faces it (a bound exit
/// of an earlier run, or placed by hand), so the bridge can cancel it when the position later closes; a
/// plain pending order (its position is a new, empty one), a protection order and a same-side order are
/// not adopted, and a NETTED account adopts nothing. An answer read in full that no longer lists the
/// position then orphans what was adopted.
#[test]
fn the_reconcile_adopts_exits_on_open_positions_and_a_flat_answer_orphans_them() {
    // `closing_order` is set on the stop and left unset on the limit: the venue's flag marks an order that
    // closes a position, which a bound exit is, so adoption must not depend on it either way.
    let mut adopt_sl =
        wire_order("adopt-sl", 601, ProtoOaOrderType::Stop, ProtoOaTradeSide::Sell, Some(7));
    adopt_sl.closing_order = Some(true);
    let adopt_tp =
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
        order: vec![adopt_sl, adopt_tp, plain, protection, same_side],
        ..Default::default()
    };

    let netted = SymbolMap::from_lists(&SymbolLists {
        account_type: Some(crate::proto::ProtoOaAccountType::Netted as i32),
        ..Default::default()
    });
    let netted_book: PositionMap = Arc::new(Mutex::new(PositionBook::default()));
    rebuild_position_book(&res, 99, &netted_book);
    adopt_bound_exits(&res, 99, &netted, &netted_book);
    assert_eq!(netted_book.lock().expect("lock").bound_exit("adopt-sl"), None, "netted: untouched");

    let positions: PositionMap = Arc::new(Mutex::new(PositionBook::default()));
    rebuild_position_book(&res, 99, &positions);
    adopt_bound_exits(&res, 99, &SymbolMap::default(), &positions);
    {
        let book = positions.lock().expect("lock");
        let sl = book.bound_exit("adopt-sl").expect("the stop is adopted");
        assert_eq!((sl.position_id, sl.kind, sl.volume), (7, ExitKind::Stop, 100_000));
        assert_eq!(book.bound_exit("adopt-tp").map(|b| b.kind), Some(ExitKind::Limit));
        for not_adopted in ["plain", "prot", "adds"] {
            assert_eq!(book.bound_exit(not_adopted), None, "{not_adopted}");
        }
    }

    // The position then closes while the bridge is away: the next answer, read in full, lists none.
    let orders: OrderIdMap = Arc::new(Mutex::new(HashMap::from([
        ("adopt-sl".to_string(), 601),
        ("adopt-tp".to_string(), 602),
    ])));
    let flat = ProtoOaReconcileRes { ctid_trader_account_id: 99, ..Default::default() };
    rebuild_position_book(&flat, 99, &positions);
    assert_eq!(
        cancels(orphan_cancel_commands(&positions, &orders)),
        vec![(601, "adopt-sl".to_string()), (602, "adopt-tp".to_string())]
    );
}

#[test]
fn conn_config_debug_redacts_secrets() {
    let cfg = ConnConfig::new(
        "demo.ctraderapi.com",
        5035,
        "my-client-id",
        "super-secret-client-secret",
        "super-secret-access-token",
    );
    let debug = format!("{cfg:?}");
    assert!(!debug.contains("super-secret-client-secret"), "leaked client_secret: {debug}");
    assert!(!debug.contains("super-secret-access-token"), "leaked access_token: {debug}");
    assert!(debug.contains("my-client-id"), "client_id should stay visible: {debug}");
    assert!(debug.contains("<redacted>"));
    assert!(debug.contains("refresh_token: \"None\""), "unset refresh_token stays None: {debug}");
}

#[test]
fn conn_config_debug_redacts_refresh_token_when_set() {
    let mut cfg = ConnConfig::new(
        "demo.ctraderapi.com",
        5035,
        "my-client-id",
        "super-secret-client-secret",
        "super-secret-access-token",
    );
    cfg.refresh_token = Some("super-secret-refresh-token".to_string());
    let debug = format!("{cfg:?}");
    assert!(!debug.contains("super-secret-refresh-token"), "leaked refresh_token: {debug}");
}

/// Build an `ERROR_RES`-typed `ProtoMessage` carrying `error_code`, for [`is_auth_error`]
/// unit tests below. The full network round-trip (`oauth::refresh` against a real cTrader
/// OAuth endpoint) is NOT exercised here — it can't run offline — and stays covered only by
/// the live smoke test (`tests/ctrader_demo_smoke.rs`); this covers just the pure classifier.
fn error_res_msg(error_code: &str) -> ProtoMessage {
    let err = ProtoOaErrorRes { error_code: error_code.to_string(), ..Default::default() };
    ProtoMessage {
        payload_type: pt::ERROR_RES,
        payload: Some(err.encode_to_vec()),
        client_msg_id: None,
    }
}

#[test]
fn is_auth_error_true_for_auth_and_token_error_codes() {
    assert!(is_auth_error(&error_res_msg("OA_AUTH_TOKEN_EXPIRED")));
    assert!(is_auth_error(&error_res_msg("ACCESS_TOKEN_INVALID")));
    // Case-insensitive: the classifier upper-cases before matching.
    assert!(is_auth_error(&error_res_msg("access_token_invalid")));
}

#[test]
fn is_auth_error_false_for_unrelated_error_code() {
    assert!(!is_auth_error(&error_res_msg("SYMBOL_NOT_FOUND")));
    assert!(!is_auth_error(&error_res_msg("ORDER_REJECTED")));
}

#[test]
fn is_auth_error_false_for_non_error_res_payload_type() {
    // Only `ERROR_RES` frames are ever classified — any other payload type is never an auth
    // error regardless of what bytes happen to be in its payload.
    let mut msg = error_res_msg("OA_AUTH_TOKEN_EXPIRED");
    msg.payload_type = pt::HEARTBEAT_EVENT;
    assert!(!is_auth_error(&msg));
}

#[test]
fn is_transient_retries_network_faults() {
    // A failed connect / reset / EOF-mid-handshake and a no-response timeout are the blips the
    // bounded connect-retry rides out.
    assert!(ConnError::Io(io::Error::new(io::ErrorKind::ConnectionReset, "reset")).is_transient());
    assert!(
        ConnError::Io(io::Error::new(io::ErrorKind::UnexpectedEof, "closed mid-handshake"))
            .is_transient()
    );
    assert!(ConnError::Timeout(pt::APPLICATION_AUTH_RES).is_transient());
}

#[test]
fn is_transient_fails_fast_on_permanent_faults() {
    // Auth rejection is the crux permanent case — a bad credential never becomes good by
    // waiting, so the retry must NOT burn attempts on it.
    assert!(
        !ConnError::Venue {
            error_code: "CH_CLIENT_AUTH_FAILURE".into(),
            description: "invalid client credentials".into(),
        }
        .is_transient()
    );
    assert!(!ConnError::NoAccounts.is_transient());
    assert!(!ConnError::Decode("schema mismatch".into()).is_transient());
    assert!(!ConnError::Tls("rustls config".into()).is_transient());
}

#[test]
fn connect_retry_default_is_bounded_and_short() {
    // The default policy must stay bounded (so a dead venue yields to paper) and cheap (a few
    // seconds total), or the "eventually fall back to paper" contract regresses.
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
    assert_eq!(RECONNECT_WAIT.initial, Duration::from_millis(200), "first reconnect wait");
    assert_eq!(RECONNECT_WAIT.max, Duration::from_secs(5), "reconnect ceiling");
}
