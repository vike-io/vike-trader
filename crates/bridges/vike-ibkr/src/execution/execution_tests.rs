use super::*;
use crate::execution::amend::PendingAmend;
use crate::execution::event_mapper::{IbkrCommissionReport, IbkrExecutionDetails, IbkrOrderStatus};
use crate::transport::{AmendOutcome, IbkrTransportFake};
use vike_log::poll::wait_until;
use vike_model::events::Event;

/// The grace window these tests age against — a literal, not the production constant, because
/// what is under test is the WIRING (does a stale entry reach `request_executions`), not the
/// value of the window. `now_ms` is opaque and monotonic, so any scale works.
const GRACE: i64 = 1_000;

/// A mapper holding ONE execution buffered awaiting its commission — the stranded shape.
fn mapper_with_buffered_execution() -> EventMapper {
    let mut ids = OrderVenueIdAllocator::default();
    ids.on_next_valid_id(101);
    let order_id = ids.next_order_id().unwrap();
    ids.bind(order_id, "order-client-id-A");
    let mut m = EventMapper::new(ids);
    m.on_submit(order_id, 10.0);
    let _ = m.on_order_status(IbkrOrderStatus {
        order_id,
        order_ref: "order-client-id-A".into(),
        status: "Submitted".into(),
        filled: 0.0,
        average_fill_price: 0.0,
        ..Default::default()
    });
    let evs = m.on_execution_details(IbkrExecutionDetails {
        order_id,
        order_ref: "order-client-id-A".into(),
        exec_id: "e1".into(),
        symbol: "AAPL.SMART.USD".into(),
        side_buy: true,
        shares: 10.0,
        price: 190.0,
        ts: 7,
        ..Default::default()
    });
    assert!(evs.is_empty(), "the fill must not emit before its commission joins");
    m
}

#[test]
fn sweep_asks_the_venue_to_replay_executions_only_once_the_grace_window_passes() {
    let mut m = mapper_with_buffered_execution();
    let mut t = IbkrTransportFake::new();
    let calls = t.execution_requests();

    // Inside the window: no request. (The very first sweep can only STAMP the entry — the
    // mapper is clockless, so nothing knows how old it is until a sweep observes it.)
    sweep_stranded_fills(&mut m, &mut t, 0, GRACE);
    sweep_stranded_fills(&mut m, &mut t, GRACE - 1, GRACE);
    assert_eq!(*calls.lock().unwrap(), 0, "a fresh buffered exec must not provoke a replay");

    // Past it: exactly one account-wide re-request.
    sweep_stranded_fills(&mut m, &mut t, GRACE, GRACE);
    assert_eq!(*calls.lock().unwrap(), 1);
}

#[test]
fn sweep_is_silent_and_free_when_nothing_is_buffered() {
    // The steady state — the overwhelming majority of passes. No `pending` entry means no
    // request at any age, so the loop's 1s cadence costs the venue nothing.
    let mut ids = OrderVenueIdAllocator::default();
    ids.on_next_valid_id(101);
    let mut m = EventMapper::new(ids);
    let mut t = IbkrTransportFake::new();
    let calls = t.execution_requests();
    for tick in 0..10 {
        sweep_stranded_fills(&mut m, &mut t, tick * GRACE * 10, GRACE);
    }
    assert_eq!(*calls.lock().unwrap(), 0);
}

#[test]
fn sweep_stops_re_requesting_once_the_retry_budget_is_spent() {
    let mut m = mapper_with_buffered_execution();
    let mut t = IbkrTransportFake::new();
    let calls = t.execution_requests();
    let mut now = 0;
    // Well past the budget: the ladder must plateau, never hammer the venue forever.
    for _ in 0..(MAX_PENDING_RECOVERY_ATTEMPTS + 5) {
        sweep_stranded_fills(&mut m, &mut t, now, GRACE);
        now += GRACE;
    }
    assert_eq!(*calls.lock().unwrap(), MAX_PENDING_RECOVERY_ATTEMPTS);
}

#[test]
fn a_resync_re_requests_open_orders_and_executions() {
    // A filled order is not an OPEN order, so the open-order snapshot structurally cannot
    // recover a fill lost across the blip — the executions replay is what does.
    let mut m = mapper_with_buffered_execution();
    let mut t = IbkrTransportFake::new();
    let calls = t.execution_requests();
    let evs = fold_inbound(&mut m, &mut t, IbkrInbound::StreamResync);
    assert!(evs.is_empty(), "the resync itself emits nothing");
    assert_eq!(*calls.lock().unwrap(), 1);
}

#[test]
fn the_replay_a_reconnect_triggers_emits_the_stranded_fill_with_the_real_commission() {
    // End to end through `fold_inbound`: the commission was lost, the socket blipped, the
    // resync replays BOTH halves, and the ordinary join emits the fill — carrying IBKR's own
    // commission figure, never a fabricated 0.0.
    let mut m = mapper_with_buffered_execution();
    let mut t = IbkrTransportFake::new();
    assert!(fold_inbound(&mut m, &mut t, IbkrInbound::StreamResync).is_empty());

    // What `reqExecutions` delivers: the execution, then its commission report.
    let replayed_execution = IbkrInbound::ExecutionDetails(IbkrExecutionDetails {
        order_id: 101,
        order_ref: "order-client-id-A".into(),
        exec_id: "e1".into(),
        symbol: "AAPL.SMART.USD".into(),
        side_buy: true,
        shares: 10.0,
        price: 190.0,
        ts: 7,
        ..Default::default()
    });
    assert!(fold_inbound(&mut m, &mut t, replayed_execution).is_empty());
    let replayed_commission = IbkrInbound::Commission(IbkrCommissionReport {
        exec_id: "e1".into(),
        commission: 1.25,
        currency: "USD".into(),
    });
    match fold_inbound(&mut m, &mut t, replayed_commission).as_slice() {
        [Event::Fill(bare), Event::OrderFilled(f)] => {
            assert_eq!(bare.commission, f.fill.commission);
            assert_eq!(f.order_client_id, "order-client-id-A");
            assert_eq!(f.fill.trade_id, "e1");
            assert_eq!(f.fill.last_qty, 10.0);
            assert_eq!(f.fill.commission, 1.25);
        }
        other => panic!("expected exactly one OrderFilled, got {other:?}"),
    }

    // A SECOND reconnect replays the same pair again — now inert, so the recovery cannot
    // become a double-fill generator.
    assert!(fold_inbound(&mut m, &mut t, IbkrInbound::StreamResync).is_empty());
    let again = IbkrInbound::ExecutionDetails(IbkrExecutionDetails {
        order_id: 101,
        order_ref: "order-client-id-A".into(),
        exec_id: "e1".into(),
        symbol: "AAPL.SMART.USD".into(),
        side_buy: true,
        shares: 10.0,
        price: 190.0,
        ts: 7,
        ..Default::default()
    });
    assert!(fold_inbound(&mut m, &mut t, again).is_empty());
    let again_c = IbkrInbound::Commission(IbkrCommissionReport {
        exec_id: "e1".into(),
        commission: 1.25,
        currency: "USD".into(),
    });
    assert!(fold_inbound(&mut m, &mut t, again_c).is_empty());
}

/// End to end through `fold_inbound` (decision 0238): the fill joins its commission, IB then REVISES the figure (and
/// re-sends it), and the revision leaves as a `FeeAdjustment` carrying the total over the fill's fee, once. The
/// engine's own ledger books it by the difference (`vike-execution`'s `fee_adjustment_fold` suite).
#[test]
fn a_revised_commission_leaves_as_a_fee_adjustment_once() {
    let mut m = mapper_with_buffered_execution();
    let mut t = IbkrTransportFake::new();
    let report = |commission: f64| {
        IbkrInbound::Commission(IbkrCommissionReport {
            exec_id: "e1".into(),
            commission,
            currency: "USD".into(),
        })
    };
    assert!(matches!(
        fold_inbound(&mut m, &mut t, report(1.25)).as_slice(),
        [Event::Fill(f), Event::OrderFilled(_)] if f.commission == 1.25
    ));
    assert!(fold_inbound(&mut m, &mut t, report(1.25)).is_empty(), "IB re-sending the figure");
    match fold_inbound(&mut m, &mut t, report(1.40)).as_slice() {
        [Event::FeeAdjustment(a)] => {
            assert!((a.extra_commission - 0.15).abs() < 1e-12, "{}", a.extra_commission);
            assert_eq!(
                (a.trade_id.as_str(), a.order_client_id.as_str()),
                ("e1", "order-client-id-A")
            );
            assert_eq!(
                (a.symbol.as_str(), a.last_price, a.timestamp),
                ("AAPL.SMART.USD", 190.0, 7)
            );
        }
        other => panic!("expected one FeeAdjustment, got {other:?}"),
    }
    assert!(fold_inbound(&mut m, &mut t, report(1.40)).is_empty(), "the revision again");
}

/// The death notice latches the registry closed and — unlike the resync it used to be spelled
/// as — issues NO venue requests. `request_open_orders`/`request_executions` are answered
/// THROUGH the pump that has just exited, so on a dead stream they are writes into a socket
/// nobody reads: at best a logged error, at worst a kernel-buffered `Ok` that looks like it
/// worked.
#[test]
fn a_stream_death_latches_the_venue_and_requests_nothing() {
    let mut m = mapper_with_buffered_execution();
    let mut t = IbkrTransportFake::new();
    let calls = t.execution_requests();

    let evs = fold_inbound(
        &mut m,
        &mut t,
        IbkrInbound::StreamDead { reason: "connection reset by peer".into() },
    );
    assert!(
        evs.is_empty(),
        "the death itself emits nothing — in-flight state is UNKNOWN, not terminal"
    );
    assert_eq!(
        *calls.lock().unwrap(),
        0,
        "a dead stream must provoke NO replay request — the resync arm is the other variant"
    );
    assert!(m.ids().stream_dead());
    assert_eq!(m.ids().stream_death_reason(), Some("connection reset by peer"));
    assert!(!m.ids().is_connected());
}

/// The stranded-fill sweep must stop once the stream is dead. Its whole mechanism is "ask IBKR
/// to re-deliver, and read the answer off the pump" — with the pump gone the answer can never
/// arrive, so an ungated sweep spends the entire finite retry budget on writes that cannot be
/// answered, and the ladder is exhausted for a rebuilt connection that could still have used it.
#[test]
fn the_stranded_fill_sweep_stops_once_the_stream_is_dead() {
    let mut m = mapper_with_buffered_execution();
    let mut t = IbkrTransportFake::new();
    let calls = t.execution_requests();

    // Alive: the ladder runs (this is `sweep_asks_the_venue_…`'s established behaviour).
    sweep_stranded_fills(&mut m, &mut t, 0, GRACE);
    sweep_stranded_fills(&mut m, &mut t, GRACE, GRACE);
    assert_eq!(*calls.lock().unwrap(), 1, "control: the ladder runs on a live stream");

    // Dead: every further pass must be a no-op, however far the clock is advanced and however
    // much retry budget is left. Without the gate this loop spends the rest of the budget.
    let _ = fold_inbound(&mut m, &mut t, IbkrInbound::StreamDead { reason: "reset".into() });
    let mut now = GRACE;
    for _ in 0..(MAX_PENDING_RECOVERY_ATTEMPTS + 5) {
        now += GRACE;
        sweep_stranded_fills(&mut m, &mut t, now, GRACE);
    }
    assert_eq!(
        *calls.lock().unwrap(),
        1,
        "a dead stream must end the ladder — no further replay requests may be issued"
    );
}

/// The whole point of splitting the variant: the two conditions must NOT fold the same way.
/// `StreamResync` re-requests state; `StreamDead` latches the venue closed and requests nothing.
#[test]
fn resync_and_death_fold_differently() {
    let mut resync_m = mapper_with_buffered_execution();
    let mut resync_t = IbkrTransportFake::new();
    let resync_calls = resync_t.execution_requests();
    let _ = fold_inbound(&mut resync_m, &mut resync_t, IbkrInbound::StreamResync);

    let mut dead_m = mapper_with_buffered_execution();
    let mut dead_t = IbkrTransportFake::new();
    let dead_calls = dead_t.execution_requests();
    let _ = fold_inbound(&mut dead_m, &mut dead_t, IbkrInbound::StreamDead { reason: "x".into() });

    assert_eq!(*resync_calls.lock().unwrap(), 1, "resync re-requests the day's executions");
    assert_eq!(*dead_calls.lock().unwrap(), 0, "death requests nothing");
    assert!(!resync_m.ids().stream_dead(), "a resync must NOT latch the venue closed");
    assert!(dead_m.ids().stream_dead());
}

/// The orders named in the death log are the ones whose venue state became unknowable — the
/// list an operator takes to TWS. Nothing is synthesized for them, so NAMING them is the only
/// thing the bridge can truthfully do.
#[test]
fn the_death_notice_can_name_the_orders_left_in_limbo() {
    let m = mapper_with_buffered_execution();
    assert_eq!(m.live_order_client_ids(), vec!["order-client-id-A"]);
}

/// The learning backstop: a snapshot request nothing ever ends is FAILED at the deadline, not pending
/// for ever. Observable through its effect: an end marker that turns up AFTER the deadline moves nothing
/// (no executions replay is asked for, the link is not announced up).
#[test]
fn a_snapshot_nothing_ends_is_abandoned_at_the_learning_deadline() {
    use crate::transport::{ScriptedInbound, SnapshotMode, SnapshotRequest};
    use std::sync::mpsc::channel;
    use vike_execution::{Ingest, event_channel};

    let (events, mut rx) = event_channel(64);
    let mut fake = IbkrTransportFake::new().with_socket_identity();
    fake.script_socket_connect(101);
    fake.open_orders_end(SnapshotMode::Silent);
    let exec_requests = fake.execution_requests();
    let (command_tx, command_rx) = channel();
    let transport = Box::new(fake.clone());
    let worker = std::thread::spawn(move || {
        run_loop(
            command_rx,
            events,
            transport,
            Taught::default(),
            Duration::from_millis(200),
            Duration::from_millis(100),
        );
    });

    std::thread::sleep(Duration::from_millis(600)); // well past the deadline
    fake.inject(ScriptedInbound::SnapshotEnd {
        snapshot: SnapshotRequest::OpenOrders,
        complete: true,
    });
    std::thread::sleep(Duration::from_millis(300));
    command_tx.send(ExecutionClientActorCommand::Shutdown).unwrap();
    worker.join().unwrap();

    assert_eq!(*exec_requests.lock().unwrap(), 0, "a late end moves nothing once abandoned");
    while let Ok(i) = rx.try_recv() {
        assert!(!matches!(i, Ingest::TradeLink(_)), "no link transition: {i:?}");
    }
}

/// **R5, the bound.** The wait for the learning is bounded: a transport that never ends it does not hold a
/// submit for ever; past the bound the order is REFUSED terminally (F6: the connection does not know which
/// orders of this client rest, so an id handed out now could be a resting order's), and a cancel of an unknown
/// order client id is refused loudly.
#[test]
fn the_wait_for_the_learning_is_bounded() {
    use crate::transport::SnapshotMode;
    use std::sync::mpsc::channel;
    use vike_execution::{Ingest, event_channel};
    use vike_model::OrderRequest;

    let (events, mut rx) = event_channel(64);
    let mut fake = IbkrTransportFake::new().with_socket_identity();
    fake.script_socket_connect(101);
    fake.open_orders_end(SnapshotMode::Silent);
    let places = fake.place_calls();
    let (command_tx, command_rx) = channel();
    let transport = Box::new(fake.clone());
    let worker = std::thread::spawn(move || {
        run_loop(
            command_rx,
            events,
            transport,
            Taught::default(),
            Duration::from_secs(30),
            Duration::from_millis(250),
        );
    });
    std::thread::sleep(Duration::from_millis(100));

    let order = OrderRequest {
        order_client_id: "order-client-id-late".into(),
        venue: "ibkr".into(),
        symbol: "AAPL.SMART.USD".into(),
        side: 1,
        qty: 1.0,
        order_type: vike_model::OrderType::Market,
        ..Default::default()
    };
    command_tx
        .send(ExecutionClientActorCommand::Submit(Box::new(order), std::time::Instant::now()))
        .unwrap();
    command_tx
        .send(ExecutionClientActorCommand::Cancel {
            order_client_id: "order-client-id-unknown".into(),
            intent: Default::default(),
        })
        .unwrap();
    std::thread::sleep(Duration::from_millis(1_200));
    command_tx.send(ExecutionClientActorCommand::Shutdown).unwrap();
    worker.join().unwrap();

    assert!(places.lock().unwrap().is_empty(), "nothing was placed once the bound passed");
    let (mut refused, mut rejected) = (false, false);
    while let Ok(i) = rx.try_recv() {
        match i {
            Ingest::Event(Event::OrderCancelRejected(r)) => {
                refused |= r.order_client_id == "order-client-id-unknown";
            }
            Ingest::Event(Event::OrderRejected(r)) => {
                rejected |= r.order_client_id == "order-client-id-late"
            }
            _ => {}
        }
    }
    assert!(rejected, "the submit is rejected terminally, with words");
    assert!(
        refused,
        "and the cancel of an unknown order client id is refused loudly after its own bound"
    );
}

// -----------------------------------------------------------------------------------------
// W14e (re-review of Part B): F6 (what a submit and a cancel need to know), F7 (the loop's upkeep releases a
// held cancel), F9 (the sweep does not close the learning), and the host's teaching reaching the mapper.
// -----------------------------------------------------------------------------------------

/// **F9.** The stranded-fill sweep must not ask for the day's executions while the connection is LEARNING: its
/// request would end into the learning's `SnapshotEnd` (the mapper cannot tell the two requests apart) and
/// close the learning before its own replay finished.
#[test]
fn the_sweep_asks_for_nothing_while_the_connection_learns_the_venue() {
    use crate::transport::{SnapshotMode, SnapshotRequest};
    let mut m = mapper_with_buffered_execution();
    let mut t = IbkrTransportFake::new().with_socket_identity();
    let calls = t.execution_requests();
    t.open_orders_end(SnapshotMode::Silent);

    m.begin_learning();
    sweep_stranded_fills(&mut m, &mut t, 0, GRACE);
    sweep_stranded_fills(&mut m, &mut t, GRACE, GRACE);
    assert_eq!(*calls.lock().unwrap(), 0, "learning the open orders");
    let _ = m.snapshot_ended(SnapshotRequest::OpenOrders, true);
    sweep_stranded_fills(&mut m, &mut t, 2 * GRACE, GRACE);
    assert_eq!(*calls.lock().unwrap(), 0, "learning the executions");
    assert_eq!(
        m.learning(),
        Learning::Executions,
        "and the learning is still waiting for ITS replay"
    );

    // Once the learning is over the buffered execution is swept as ever.
    let _ = m.snapshot_ended(SnapshotRequest::Executions, true);
    sweep_stranded_fills(&mut m, &mut t, 3 * GRACE, GRACE); // stamps the entry
    sweep_stranded_fills(&mut m, &mut t, 4 * GRACE, GRACE);
    assert_eq!(*calls.lock().unwrap(), 1);
}

/// **F7, the loop's upkeep.** `sweep_tick` asks the venue for the commission a cancel is held for once the bound runs out,
/// releases the cancel one reply window later if nothing came, and sends what comes out on the lane, with the stream
/// dead or not.
#[test]
fn the_upkeep_pass_releases_a_cancel_held_for_a_commission_that_did_not_come() {
    use crate::execution::event_mapper::COMMISSION_REREQUEST_WAIT_MS;
    use vike_execution::{Ingest, event_channel};

    let (events, mut rx) = event_channel(16);
    let mut m = mapper_with_buffered_execution();
    let mut t = IbkrTransportFake::new();
    let requests = t.execution_requests();
    let held = m.on_order_status(IbkrOrderStatus {
        order_id: 101,
        order_ref: "order-client-id-A".into(),
        status: "Cancelled".into(),
        filled: 10.0,
        average_fill_price: 190.0,
        ..Default::default()
    });
    assert!(held.is_empty(), "held for the commission of e1");

    sweep_tick(&mut m, &mut t, &events, 0, GRACE, 500);
    assert!(rx.try_recv().is_err(), "inside the bound nothing goes out");
    assert_eq!(*requests.lock().unwrap(), 0, "and nothing is asked");
    sweep_tick(&mut m, &mut t, &events, 500, GRACE, 500);
    assert!(
        rx.try_recv().is_err(),
        "the bound ran out: the venue is asked FIRST, the fee is not booked yet"
    );
    assert_eq!(
        *requests.lock().unwrap(),
        1,
        "the ask is `reqExecutions`, which re-delivers the commission reports"
    );
    sweep_tick(&mut m, &mut t, &events, 500 + COMMISSION_REREQUEST_WAIT_MS, GRACE, 500);
    let mut got = Vec::new();
    while let Ok(Ingest::Event(event)) = rx.try_recv() {
        got.push(event);
    }
    assert!(
        matches!(
            got.as_slice(),
            [
                Event::Fill(_),
                Event::OrderFilled(_) | Event::OrderPartiallyFilled(_),
                Event::OrderCanceled(_)
            ]
        ),
        "the fills (unknown fee) and then the cancel: {got:?}"
    );
}

/// **F6.** A cancel of an order client id the connection does not know YET (the learning is still running when the wait
/// runs out) is answered with the advisory and RETRIED once, when the learning ends and has bound the order.
#[test]
fn a_cancel_of_an_unknown_order_client_id_during_the_learning_is_retried_when_the_learning_ends() {
    use crate::transport::{ScriptedInbound, SnapshotMode, SnapshotRequest};
    use std::sync::mpsc::channel;
    use vike_execution::{Ingest, event_channel};

    let (events, mut rx) = event_channel(64);
    let mut fake = IbkrTransportFake::new().with_socket_identity();
    fake.script_socket_connect(101);
    fake.open_orders_end(SnapshotMode::Silent);
    let cancels = fake.cancel_calls();
    let (command_tx, command_rx) = channel();
    let transport = Box::new(fake.clone());
    let worker = std::thread::spawn(move || {
        run_loop(
            command_rx,
            events,
            transport,
            Taught::default(),
            Duration::from_secs(30),
            Duration::from_millis(100),
        );
    });
    std::thread::sleep(Duration::from_millis(100));
    command_tx
        .send(ExecutionClientActorCommand::Cancel {
            order_client_id: "order-client-id-rested".into(),
            intent: Default::default(),
        })
        .unwrap();
    std::thread::sleep(Duration::from_millis(400)); // past the wait: the advisory went out
    assert!(cancels.lock().unwrap().is_empty(), "nothing is sent for an order nobody knows");

    // The learning ends and has bound the order: the deferred cancel goes out, once.
    fake.inject(ScriptedInbound::venue_order(105, "order-client-id-rested", 10.0));
    fake.inject(ScriptedInbound::SnapshotEnd {
        snapshot: SnapshotRequest::OpenOrders,
        complete: true,
    });
    fake.inject(ScriptedInbound::SnapshotEnd {
        snapshot: SnapshotRequest::Executions,
        complete: true,
    });
    std::thread::sleep(Duration::from_millis(500));
    command_tx.send(ExecutionClientActorCommand::Shutdown).unwrap();
    worker.join().unwrap();

    assert_eq!(
        *cancels.lock().unwrap(),
        [105],
        "the retry reaches the venue under IB's own id, once"
    );
    let mut advisories = 0;
    while let Ok(i) = rx.try_recv() {
        if let Ingest::Event(Event::OrderCancelRejected(r)) = i {
            advisories += usize::from(r.order_client_id == "order-client-id-rested");
        }
    }
    assert_eq!(advisories, 1, "the caller was told once that the request was not sent at first");
}

/// A deferred cancel is NOT retried when the learning failed: the connection still does not know the order.
#[test]
fn a_deferred_cancel_is_not_retried_when_the_learning_fails() {
    use crate::transport::{ScriptedInbound, SnapshotMode, SnapshotRequest};
    use std::sync::mpsc::channel;
    use vike_execution::event_channel;

    let (events, _rx) = event_channel(64);
    let mut fake = IbkrTransportFake::new().with_socket_identity();
    fake.script_socket_connect(101);
    fake.open_orders_end(SnapshotMode::Silent);
    let cancels = fake.cancel_calls();
    let (command_tx, command_rx) = channel();
    let transport = Box::new(fake.clone());
    let worker = std::thread::spawn(move || {
        run_loop(
            command_rx,
            events,
            transport,
            Taught::default(),
            Duration::from_secs(30),
            Duration::from_millis(100),
        );
    });
    std::thread::sleep(Duration::from_millis(100));
    command_tx
        .send(ExecutionClientActorCommand::Cancel {
            order_client_id: "order-client-id-x".into(),
            intent: Default::default(),
        })
        .unwrap();
    std::thread::sleep(Duration::from_millis(300));
    fake.inject(ScriptedInbound::venue_order(105, "order-client-id-x", 10.0));
    fake.inject(ScriptedInbound::SnapshotEnd {
        snapshot: SnapshotRequest::OpenOrders,
        complete: false,
    });
    std::thread::sleep(Duration::from_millis(400));
    command_tx.send(ExecutionClientActorCommand::Shutdown).unwrap();
    worker.join().unwrap();
    assert!(cancels.lock().unwrap().is_empty(), "a failed learning bound nothing and sent nothing");
}

/// The host's teaching (`adopt_restored`) reaches the exec loop's mapper: the fill of a rebound order carries the
/// symbol the engine registered it under, not the instrument IB echoed.
#[test]
fn what_the_host_taught_about_a_restored_order_stamps_its_fills() {
    use crate::transport::ScriptedInbound;
    use std::sync::mpsc::channel;
    use vike_execution::{Ingest, event_channel};

    let (events, mut rx) = event_channel(64);
    let mut fake = IbkrTransportFake::new().with_socket_identity();
    fake.script_socket_connect(101);
    fake.on_request_open_orders(|| {
        vec![ScriptedInbound::venue_order(105, "order-client-id-rested", 10.0)]
    });
    fake.on_request_executions(|| {
        vec![
            ScriptedInbound::ExecutionOf {
                order_id: 105,
                order_ref: "order-client-id-rested".into(),
                exec_id: "e1".into(),
                symbol: "EUR.IDEALPRO.USD".into(),
                shares: 4.0,
                price: 1.1,
                client_id: crate::transport::FAKE_CLIENT_ID,
            },
            ScriptedInbound::Commission { exec_id: "e1".into(), commission: 1.0 },
        ]
    });
    let taught = Taught::default();
    taught.teach(&[("order-client-id-rested".to_string(), "EUR.USD.IDEALPRO".to_string())]);
    let (command_tx, command_rx) = channel();
    let transport = Box::new(fake.clone());
    let run_taught = taught.clone();
    let worker = std::thread::spawn(move || {
        run_loop(
            command_rx,
            events,
            transport,
            run_taught,
            Duration::from_secs(30),
            Duration::from_millis(100),
        );
    });
    std::thread::sleep(Duration::from_millis(500));
    command_tx.send(ExecutionClientActorCommand::Shutdown).unwrap();
    worker.join().unwrap();

    let mut symbols = Vec::new();
    while let Ok(i) = rx.try_recv() {
        if let Ingest::Event(Event::Fill(f)) = i {
            symbols.push(f.symbol.to_string());
        }
    }
    assert_eq!(symbols, ["EUR.USD.IDEALPRO"]);
}

/// The amend of an order client id not known yet during the learning is deferred like the cancel: the advisory now, ONE retry
/// when the learning has bound the order.
#[test]
fn an_amend_of_an_unknown_order_client_id_during_the_learning_is_retried_when_the_learning_ends() {
    use crate::transport::{ScriptedInbound, SnapshotMode, SnapshotRequest};
    use std::sync::mpsc::channel;
    use vike_execution::{Ingest, event_channel};

    let (events, mut rx) = event_channel(64);
    let mut fake = IbkrTransportFake::new().with_socket_identity();
    fake.script_socket_connect(101);
    fake.open_orders_end(SnapshotMode::Silent);
    let modifies = fake.modify_calls();
    let (command_tx, command_rx) = channel();
    let transport = Box::new(fake.clone());
    let worker = std::thread::spawn(move || {
        run_loop(
            command_rx,
            events,
            transport,
            Taught::default(),
            Duration::from_secs(30),
            Duration::from_millis(100),
        );
    });
    std::thread::sleep(Duration::from_millis(100));
    let resting = vike_model::OrderRequest {
        order_client_id: "order-client-id-rested".into(),
        venue: "ibkr".into(),
        symbol: "AAPL.SMART.USD".into(),
        side: 1,
        qty: 10.0,
        price: Some(100.0),
        order_type: vike_model::OrderType::Limit,
        ..Default::default()
    };
    command_tx
        .send(ExecutionClientActorCommand::Modify {
            order: Box::new(resting),
            new_qty: None,
            new_price: Some(101.0),
            new_trigger_price: None,
        })
        .unwrap();
    std::thread::sleep(Duration::from_millis(400));
    assert!(modifies.lock().unwrap().is_empty(), "nothing is sent for an order nobody knows yet");

    fake.inject(ScriptedInbound::venue_order(105, "order-client-id-rested", 10.0));
    fake.inject(ScriptedInbound::SnapshotEnd {
        snapshot: SnapshotRequest::OpenOrders,
        complete: true,
    });
    fake.inject(ScriptedInbound::SnapshotEnd {
        snapshot: SnapshotRequest::Executions,
        complete: true,
    });
    std::thread::sleep(Duration::from_millis(500));
    command_tx.send(ExecutionClientActorCommand::Shutdown).unwrap();
    worker.join().unwrap();

    assert_eq!(
        *modifies.lock().unwrap(),
        [105],
        "the retry reaches the transport under IB's own id"
    );
    let mut advisories = 0;
    while let Ok(i) = rx.try_recv() {
        if let Ingest::Event(Event::OrderModifyRejected(r)) = i {
            advisories += usize::from(r.order_client_id == "order-client-id-rested");
        }
    }
    assert_eq!(advisories, 1, "the caller was told once");
}

/// **Arrival order.** What was deferred during the learning goes out BEFORE a later command whose own wait for the
/// learning saw it end. Here an amend is deferred, then a cancel of the same order waits, and the learning ends inside
/// that wait: the cancel used to go out first and the deferred amend re-placed an order whose cancel was on its way.
#[test]
fn a_deferred_amend_goes_before_the_cancel_whose_wait_saw_the_learning_end() {
    use crate::transport::{ScriptedInbound, SnapshotMode, SnapshotRequest};
    use std::sync::mpsc::channel;
    use std::sync::{Arc, Mutex};
    use vike_execution::{Ingest, event_channel};

    let (events, mut rx) = event_channel(64);
    let mut fake = IbkrTransportFake::new().with_socket_identity();
    fake.script_socket_connect(101);
    fake.open_orders_end(SnapshotMode::Silent);
    let cancels = fake.cancel_calls();
    let learning_asked = fake.open_order_requests();
    // How many cancels had gone out when each amend did.
    let cancels_at_amend = Arc::new(Mutex::new(Vec::new()));
    let (seen_cancels, seen) = (Arc::clone(&cancels), Arc::clone(&cancels_at_amend));
    fake.on_modify_order(move |_| {
        seen.lock().unwrap().push(seen_cancels.lock().unwrap().len());
        Vec::new()
    });
    let (command_tx, command_rx) = channel();
    let transport = Box::new(fake.clone());
    let worker = std::thread::spawn(move || {
        run_loop(
            command_rx,
            events,
            transport,
            Taught::default(),
            Duration::from_secs(30),
            Duration::from_millis(1_000),
        );
    });
    assert!(
        wait_until(Duration::from_secs(5), || *learning_asked.lock().unwrap() == 1),
        "the learning runs"
    );
    let resting = vike_model::OrderRequest {
        order_client_id: "order-client-id-rested".into(),
        venue: "ibkr".into(),
        symbol: "AAPL.SMART.USD".into(),
        side: 1,
        qty: 10.0,
        price: Some(100.0),
        order_type: vike_model::OrderType::Limit,
        ..Default::default()
    };
    command_tx
        .send(ExecutionClientActorCommand::Modify {
            order: Box::new(resting),
            new_qty: None,
            new_price: Some(101.0),
            new_trigger_price: None,
        })
        .unwrap();
    command_tx
        .send(ExecutionClientActorCommand::Cancel {
            order_client_id: "order-client-id-rested".into(),
            intent: Default::default(),
        })
        .unwrap();
    // The amend's wait ran out (its advisory is out, it is deferred); the cancel is now waiting for the learning.
    assert!(wait_until(Duration::from_secs(5), || {
        matches!(rx.try_recv(), Ok(Ingest::Event(Event::OrderModifyRejected(_))))
    }));
    std::thread::sleep(Duration::from_millis(200));
    fake.inject(ScriptedInbound::venue_order(105, "order-client-id-rested", 10.0));
    fake.inject(ScriptedInbound::SnapshotEnd {
        snapshot: SnapshotRequest::OpenOrders,
        complete: true,
    });
    assert!(wait_until(Duration::from_secs(5), || !cancels.lock().unwrap().is_empty()));
    std::thread::sleep(Duration::from_millis(200));
    command_tx.send(ExecutionClientActorCommand::Shutdown).unwrap();
    worker.join().unwrap();

    assert_eq!(*cancels.lock().unwrap(), [105]);
    assert_eq!(*cancels_at_amend.lock().unwrap(), [0], "the deferred amend went out first, once");
}

/// **A submit is never held for a later command.** With the learning parked on the host's handover (no
/// `adopt_restored` yet: the open orders are known, the executions replay is held), a lone submit reaches the
/// transport after its bounded wait, with nothing sent behind it and the learning still running.
#[test]
fn a_submit_during_the_learning_reaches_the_transport_with_no_further_command() {
    use std::sync::mpsc::channel;
    use vike_execution::event_channel;

    let (events, _rx) = event_channel(64);
    let mut fake = IbkrTransportFake::new().with_socket_identity();
    fake.script_socket_connect(101);
    let places = fake.place_calls();
    let taught = Taught::gated();
    let (command_tx, command_rx) = channel();
    let transport = Box::new(fake.clone());
    let run_taught = taught.clone();
    let worker = std::thread::spawn(move || {
        run_loop(
            command_rx,
            events,
            transport,
            run_taught,
            Duration::from_secs(30),
            Duration::from_millis(100),
        );
    });
    assert!(
        wait_until(Duration::from_secs(5), || taught.is_awaiting()),
        "learning, held for the host"
    );
    let order = vike_model::OrderRequest {
        order_client_id: "order-client-id-new".into(),
        venue: "ibkr".into(),
        symbol: "AAPL.SMART.USD".into(),
        side: 1,
        qty: 1.0,
        price: Some(100.0),
        order_type: vike_model::OrderType::Limit,
        ..Default::default()
    };
    command_tx
        .send(ExecutionClientActorCommand::Submit(Box::new(order), std::time::Instant::now()))
        .unwrap();
    assert!(
        wait_until(Duration::from_secs(5), || places.lock().unwrap().len() == 1),
        "the submit reached the transport on its own"
    );
    assert!(taught.is_awaiting(), "while the learning still runs");
    command_tx.send(ExecutionClientActorCommand::Shutdown).unwrap();
    worker.join().unwrap();
}

// ---- W14f ----------------------------------------------------------------------------------------------

/// The waits before the retries of a failed learning, shortened.
const RETRY_50: &[Duration] = &[Duration::from_millis(50), Duration::from_millis(50)];
const RETRY_30: &[Duration] = &[Duration::from_millis(30), Duration::from_millis(30)];

/// The link transitions (`true` = up) and the symbols of the fills in what `rx` holds now.
fn link_and_fills(
    rx: &mut tokio::sync::mpsc::Receiver<vike_execution::Ingest>,
) -> (Vec<bool>, Vec<String>) {
    use vike_execution::Ingest;
    let (mut links, mut symbols) = (Vec::new(), Vec::new());
    while let Ok(i) = rx.try_recv() {
        match i {
            Ingest::TradeLink(u) => links.push(u.up),
            Ingest::Event(Event::Fill(f)) => symbols.push(f.symbol.to_string()),
            _ => {}
        }
    }
    (links, symbols)
}

/// **Inside the 1100 window an order went to a gateway with no link**: `submit_refusal` looked only at a dead stream, the
/// watchdog waits `LOST_AFTER`, and a gateway that sent the order at the 1102 left the venue holding what the ladder had
/// called dead. Refused while the gateway says so, and let through once it says otherwise.
#[test]
fn a_gateway_with_no_server_link_refuses_a_submit_until_the_link_is_back() {
    let (events, _lane) = vike_execution::event_channel(8);
    let mut ids = OrderVenueIdAllocator::default();
    ids.on_next_valid_id(101);
    ids.set_accounts_ready(true);
    let mut m = EventMapper::new(ids);
    let mut t = IbkrTransportFake::new();
    assert_eq!(submit_refusal(&mut m, &mut t, &events), None, "a healthy gateway takes it");

    t.set_server_link_broken(true);
    let refused = submit_refusal(&mut m, &mut t, &events).expect("refused");
    assert!(refused.contains("no link") && refused.contains("NOT sent"), "{refused}");
    assert!(!refused.contains('\n') && !refused.contains("  "), "one plain line: {refused:?}");

    t.set_server_link_broken(false);
    assert_eq!(submit_refusal(&mut m, &mut t, &events), None);
}

/// **Finding 4.** The refusal text of a submit placed while the venue's open orders are unknown is ONE plain line:
/// it once carried literal newlines and runs of spaces (a mangled line continuation) and told the user to retry
/// "once the connection reports itself up", which can never happen after a failed learning. It now says what
/// failed and the action, and these assertions are what keeps a continuation from sneaking back in.
#[test]
fn the_unlearned_refusal_is_one_plain_line() {
    assert!(!UNLEARNED_REASON.contains('\n'), "no newline: {UNLEARNED_REASON:?}");
    assert!(!UNLEARNED_REASON.contains('\r'));
    assert!(!UNLEARNED_REASON.contains('\\'), "no backslash at all: {UNLEARNED_REASON:?}");
    assert!(!UNLEARNED_REASON.contains("  "), "no run of spaces: {UNLEARNED_REASON:?}");
    assert!(UNLEARNED_REASON.trim() == UNLEARNED_REASON);
    assert!(UNLEARNED_REASON.contains("NOT sent"), "says nothing was sent");
    assert!(UNLEARNED_REASON.contains("FAILED"), "says what can have failed");
    assert!(
        UNLEARNED_REASON.contains("restart the gatewayhub"),
        "and the action that is left once it failed: {UNLEARNED_REASON:?}"
    );
}

/// **Finding 1, the bridge half.** The learning's executions replay is NOT asked for until the host has handed the
/// client over (its first `adopt_restored`, even an empty one): the fills the replay brings of orders the host
/// knew must carry the symbols it teaches, and a replay that ran ahead of them was dropped by the engine for good.
#[test]
fn the_executions_replay_waits_for_the_hosts_handover_and_then_carries_the_taught_symbol() {
    use crate::transport::{FAKE_CLIENT_ID, ScriptedInbound};
    use std::sync::mpsc::channel;
    use vike_execution::event_channel;

    let (events, mut rx) = event_channel(64);
    let mut fake = IbkrTransportFake::new().with_socket_identity();
    fake.script_socket_connect(101);
    fake.on_request_open_orders(|| {
        vec![ScriptedInbound::venue_order(105, "order-client-id-rested", 10.0)]
    });
    fake.on_request_executions(|| {
        vec![
            ScriptedInbound::ExecutionOf {
                order_id: 105,
                order_ref: "order-client-id-rested".into(),
                exec_id: "e1".into(),
                symbol: "EUR.IDEALPRO.USD".into(),
                shares: 4.0,
                price: 1.1,
                client_id: FAKE_CLIENT_ID,
            },
            ScriptedInbound::Commission { exec_id: "e1".into(), commission: 1.0 },
        ]
    });
    let exec_requests = fake.execution_requests();
    let taught = Taught::gated();
    let (command_tx, command_rx) = channel();
    let transport = Box::new(fake.clone());
    let run_taught = taught.clone();
    let worker = std::thread::spawn(move || {
        run_loop(
            command_rx,
            events,
            transport,
            run_taught,
            Duration::from_secs(30),
            Duration::from_millis(100),
        );
    });
    // The open orders are in and the loop is parked on the replay: it has said so, and asked for nothing.
    assert!(
        wait_until(Duration::from_secs(5), || taught.is_awaiting() && fake.undelivered() == 0),
        "the loop waits for the host"
    );
    assert_eq!(*exec_requests.lock().unwrap(), 0, "the replay is held back until the handover");
    assert!(link_and_fills(&mut rx).0.is_empty(), "and the link is not announced up");

    // The handover: the replay runs, and its fill carries the symbol the host taught.
    taught.teach(&[("order-client-id-rested".to_string(), "EUR.USD.IDEALPRO".to_string())]);
    assert!(wait_until(Duration::from_secs(5), || *exec_requests.lock().unwrap() == 1));
    std::thread::sleep(Duration::from_millis(300));
    command_tx.send(ExecutionClientActorCommand::Shutdown).unwrap();
    worker.join().unwrap();
    let (links, symbols) = link_and_fills(&mut rx);
    assert_eq!(symbols, ["EUR.USD.IDEALPRO"]);
    assert_eq!(links, [true], "learned, announced once, after the replay");
}

/// A handover with nothing to teach (an empty `adopt_restored`, a first start) lets the replay through at once.
#[test]
fn an_empty_handover_lets_the_replay_through() {
    use std::sync::mpsc::channel;
    use vike_execution::event_channel;

    let (events, mut rx) = event_channel(64);
    let mut fake = IbkrTransportFake::new().with_socket_identity();
    fake.script_socket_connect(101);
    let taught = Taught::gated();
    let (command_tx, command_rx) = channel();
    let transport = Box::new(fake.clone());
    let run_taught = taught.clone();
    let worker = std::thread::spawn(move || {
        run_loop(
            command_rx,
            events,
            transport,
            run_taught,
            Duration::from_secs(30),
            Duration::from_millis(100),
        );
    });
    assert!(wait_until(Duration::from_secs(5), || taught.is_awaiting()));
    taught.teach(&[]);
    assert!(wait_until(Duration::from_secs(5), || !taught.is_awaiting()));
    std::thread::sleep(Duration::from_millis(200));
    command_tx.send(ExecutionClientActorCommand::Shutdown).unwrap();
    worker.join().unwrap();
    assert_eq!(link_and_fills(&mut rx).0, [true]);
}

/// The fail-safe: a host that never hands the client over does not hold the learning for ever. The replay goes
/// ahead after the (shortened) bound, untaught.
#[test]
fn a_host_that_never_comes_does_not_hold_the_replay_for_ever() {
    use std::sync::mpsc::channel;
    use vike_execution::event_channel;

    let (events, mut rx) = event_channel(64);
    let mut fake = IbkrTransportFake::new().with_socket_identity();
    fake.script_socket_connect(101);
    let exec_requests = fake.execution_requests();
    let (command_tx, command_rx) = channel();
    let transport = Box::new(fake.clone());
    let times = LoopTimes {
        learn_deadline: Duration::from_secs(30),
        learn_wait: Duration::from_millis(100),
        teach_wait: Duration::from_millis(300),
        retry: &[],
        ..LoopTimes::PRODUCTION
    };
    let worker = std::thread::spawn(move || {
        run_loop_with(command_rx, events, transport, Taught::gated(), times);
    });
    assert!(
        wait_until(Duration::from_secs(5), || *exec_requests.lock().unwrap() == 1),
        "the bound let the replay through"
    );
    std::thread::sleep(Duration::from_millis(200));
    command_tx.send(ExecutionClientActorCommand::Shutdown).unwrap();
    worker.join().unwrap();
    assert_eq!(link_and_fills(&mut rx).0, [true]);
}

/// The fail-safe COSTS the fills of the orders the host would have taught: the replay that goes ahead after the
/// bound finds no symbol for them, and an order nobody taught is not recovered (it is dropped in the bridge, whatever
/// IB echoes), so a handover that comes too late loses the fill to the reconcile, not just its label.
#[test]
fn a_replay_that_goes_ahead_without_its_host_drops_the_executions_of_orders_nobody_taught() {
    use crate::transport::{FAKE_CLIENT_ID, ScriptedInbound};
    use std::sync::mpsc::channel;
    use vike_execution::event_channel;

    let (events, mut rx) = event_channel(64);
    let mut fake = IbkrTransportFake::new().with_socket_identity();
    fake.script_socket_connect(101);
    fake.on_request_executions(|| {
        vec![
            ScriptedInbound::ExecutionOf {
                order_id: 900,
                order_ref: "order-client-id-old".into(),
                exec_id: "e1".into(),
                symbol: "AAPL.SMART.USD".into(),
                shares: 4.0,
                price: 190.0,
                client_id: FAKE_CLIENT_ID,
            },
            ScriptedInbound::Commission { exec_id: "e1".into(), commission: 1.0 },
        ]
    });
    let exec_requests = fake.execution_requests();
    let (command_tx, command_rx) = channel();
    let transport = Box::new(fake.clone());
    let times = LoopTimes {
        learn_deadline: Duration::from_secs(30),
        learn_wait: Duration::from_millis(100),
        teach_wait: Duration::from_millis(300),
        retry: &[],
        ..LoopTimes::PRODUCTION
    };
    let worker = std::thread::spawn(move || {
        run_loop_with(command_rx, events, transport, Taught::gated(), times);
    });
    assert!(
        wait_until(Duration::from_secs(5), || *exec_requests.lock().unwrap() == 1),
        "the bound let the replay through"
    );
    std::thread::sleep(Duration::from_millis(300));
    command_tx.send(ExecutionClientActorCommand::Shutdown).unwrap();
    worker.join().unwrap();
    let (links, fills) = link_and_fills(&mut rx);
    assert_eq!(links, [true], "the learning still ends, the link is announced");
    assert!(fills.is_empty(), "and the untaught order's fill is not emitted: {fills:?}");
}

/// **Finding 2, recovery.** A learning that fails is tried again (the first request errors, the second completes):
/// the connection learns, announces the link up once, and asked for the open orders twice.
#[test]
fn a_failed_learning_is_tried_again_and_the_retry_can_succeed() {
    use crate::transport::SnapshotMode;
    use std::sync::mpsc::channel;
    use vike_execution::event_channel;

    let (events, mut rx) = event_channel(64);
    let mut fake = IbkrTransportFake::new().with_socket_identity();
    fake.script_socket_connect(101);
    fake.open_orders_ends(&[SnapshotMode::Failed, SnapshotMode::Complete]);
    let open_requests = fake.open_order_requests();
    let (command_tx, command_rx) = channel();
    let transport = Box::new(fake.clone());
    let times = LoopTimes {
        learn_deadline: Duration::from_secs(30),
        learn_wait: Duration::from_millis(100),
        teach_wait: TEACH_WAIT,
        retry: RETRY_50,
        ..LoopTimes::PRODUCTION
    };
    let worker = std::thread::spawn(move || {
        run_loop_with(command_rx, events, transport, Taught::default(), times);
    });
    assert!(
        wait_until(Duration::from_secs(5), || *open_requests.lock().unwrap() == 2),
        "asked again after the failure"
    );
    std::thread::sleep(Duration::from_millis(300));
    command_tx.send(ExecutionClientActorCommand::Shutdown).unwrap();
    worker.join().unwrap();
    assert_eq!(*open_requests.lock().unwrap(), 2, "and no third time: the retry learned");
    assert_eq!(link_and_fills(&mut rx).0, [true], "announced up once, by the retry");
}

/// **Finding 2, bounded.** A learning that keeps failing is tried three times in all and then given up: the link
/// is never announced up and nothing asks again.
#[test]
fn a_learning_that_keeps_failing_is_given_up_after_its_attempts() {
    use crate::transport::SnapshotMode;
    use std::sync::mpsc::channel;
    use vike_execution::event_channel;

    let (events, mut rx) = event_channel(64);
    let mut fake = IbkrTransportFake::new().with_socket_identity();
    fake.script_socket_connect(101);
    fake.open_orders_end(SnapshotMode::Failed);
    let open_requests = fake.open_order_requests();
    let (command_tx, command_rx) = channel();
    let transport = Box::new(fake.clone());
    let times = LoopTimes {
        learn_deadline: Duration::from_secs(30),
        learn_wait: Duration::from_millis(100),
        teach_wait: TEACH_WAIT,
        retry: RETRY_30,
        ..LoopTimes::PRODUCTION
    };
    let worker = std::thread::spawn(move || {
        run_loop_with(command_rx, events, transport, Taught::default(), times);
    });
    assert!(wait_until(Duration::from_secs(5), || *open_requests.lock().unwrap() == 3));
    std::thread::sleep(Duration::from_millis(400));
    command_tx.send(ExecutionClientActorCommand::Shutdown).unwrap();
    worker.join().unwrap();
    assert_eq!(*open_requests.lock().unwrap(), 3, "three attempts, then it stops asking");
    assert!(link_and_fills(&mut rx).0.is_empty(), "never announced up");
}

// ---- order modify ----------------------------------------------------------------------------------------------

/// A transport that implements only the REQUIRED methods: what a backend with no amend looks like.
struct NoAmend;

impl IbkrTransport for NoAmend {
    fn place_order(
        &mut self,
        _: i32,
        _: &crate::execution::order::IbkrOrderSpec,
        _: &crate::instrument::contract::IbkrContract,
    ) {
    }
    fn cancel_order(&mut self, _: i32) -> Result<(), String> {
        Ok(())
    }
    fn request_open_orders(&mut self) {}
    fn next_recv(&mut self, _: Duration) -> Option<IbkrInbound> {
        None
    }
}

/// A transport that locates every restored order it is asked about under `-1` (the cpapi backend's first synthetic id)
/// and cannot address it: its cancel and its amend are refused in its words, nothing sent.
struct LocatedWithoutAnAddress;

impl IbkrTransport for LocatedWithoutAnAddress {
    fn place_order(
        &mut self,
        _: i32,
        _: &crate::execution::order::IbkrOrderSpec,
        _: &crate::instrument::contract::IbkrContract,
    ) {
    }
    fn cancel_order(&mut self, order_id: i32) -> Result<(), String> {
        assert_eq!(order_id, -1, "the cancel goes to the order the teaching bound");
        Err("cpapi cancel NOT sent: no gateway id".to_string())
    }
    fn request_open_orders(&mut self) {}
    fn next_recv(&mut self, _: Duration) -> Option<IbkrInbound> {
        None
    }
    fn locate_restored(&mut self, order_client_ids: &[String]) -> Vec<(i32, String)> {
        order_client_ids.iter().map(|order_client_id| (-1, order_client_id.clone())).collect()
    }
}

/// **A restored order the host taught is bound by what the transport locates, and a cancel the transport refuses is
/// an `OrderCancelRejected` in its words** (non-terminal: the order may still rest), never a silent no-op and never an
/// `OrderCanceled`. Run synchronously: the loop ends at the `Shutdown` queued after the cancel.
#[test]
fn a_restored_order_is_bound_by_the_teaching_and_a_refused_cancel_says_why() {
    use std::sync::mpsc::channel;
    use vike_execution::{Ingest, event_channel};

    let (events, mut rx) = event_channel(16);
    let taught = Taught::default();
    taught.teach(&[("order-client-id-r".to_string(), "AAPL.SMART.USD".to_string())]);
    let (command_tx, command_rx) = channel();
    command_tx
        .send(ExecutionClientActorCommand::Cancel {
            order_client_id: "order-client-id-r".into(),
            intent: Default::default(),
        })
        .unwrap();
    command_tx.send(ExecutionClientActorCommand::Shutdown).unwrap();
    run_loop(
        command_rx,
        events,
        Box::new(LocatedWithoutAnAddress),
        taught,
        Duration::from_secs(30),
        Duration::from_millis(100),
    );
    let mut got = Vec::new();
    while let Ok(i) = rx.try_recv() {
        if let Ingest::Event(event) = i {
            got.push(event);
        }
    }
    assert!(
        matches!(got.as_slice(), [Event::OrderCancelRejected(r)]
            if r.order_client_id == "order-client-id-r" && r.reason.contains("NOT sent: no gateway id")),
        "{got:?}"
    );
}

/// A backend that cannot amend REFUSES, in words. The trait default used to be a silent no-op, which is exactly
/// how a modify came to be accepted by the control door and then do nothing.
#[test]
fn a_transport_without_an_amend_refuses_in_words() {
    let spec = crate::execution::order::IbkrOrderSpec {
        action: "BUY",
        total_qty: 1.0,
        order_type: "LMT",
        lmt_price: Some(1.0),
        aux_price: None,
        tif: "GTC",
        order_ref: "c".into(),
        display_size: None,
        lmt_price_offset: None,
        activation: None,
        ..Default::default()
    };
    let contract = crate::instrument::contract::parse_simplified("AAPL.SMART.USD").unwrap();
    let out = NoAmend.modify_order(101, &spec, &contract);
    assert!(
        matches!(&out, AmendOutcome::Refused(why) if why.contains("no way to amend")),
        "got {out:?}"
    );
}

/// The loop's upkeep reports an amend TWS never echoed, once, after the bound; before it nothing goes out.
#[test]
fn an_amend_no_echo_answers_is_reported_unconfirmed_by_the_upkeep() {
    use vike_execution::{Ingest, event_channel};
    let (events, mut rx) = event_channel(16);
    let mut m = mapper_with_buffered_execution();
    let mut t = IbkrTransportFake::new();
    assert!(
        m.note_amend(
            "order-client-id-A",
            PendingAmend::new(101, (10.0, Some(1.0)), (10.0, Some(2.0)), false)
        )
        .is_none()
    );
    sweep_tick(&mut m, &mut t, &events, 0, GRACE, 500);
    sweep_tick(&mut m, &mut t, &events, AMEND_CONFIRM_WAIT_MS - 1, GRACE, 500);
    assert!(
        !matches!(rx.try_recv(), Ok(Ingest::Event(Event::OrderModifyRejected(_)))),
        "inside the bound nothing is reported"
    );
    sweep_tick(&mut m, &mut t, &events, AMEND_CONFIRM_WAIT_MS, GRACE, 500);
    let mut rejected = 0;
    while let Ok(i) = rx.try_recv() {
        if let Ingest::Event(Event::OrderModifyRejected(r)) = i {
            assert_eq!(r.order_client_id, "order-client-id-A");
            rejected += 1;
        }
    }
    assert_eq!(rejected, 1);
    sweep_tick(&mut m, &mut t, &events, 3 * AMEND_CONFIRM_WAIT_MS, GRACE, 500);
    assert!(
        !matches!(rx.try_recv(), Ok(Ingest::Event(Event::OrderModifyRejected(_)))),
        "reported once"
    );
}

/// **A kind this bridge builds no IB order for is refused through the real loop, never sent as MARKET**: a word
/// outside the six (the core's preflight refuses it first; this is the net behind it) answers `OrderSubmitted` then a
/// terminal `OrderRejected` naming the kind, and nothing reaches the transport. A `take_profit`, refused the same way
/// until wave 2 of the new order kinds, is now placed: as IB's `LIT` (its limit `lmtPrice`, its trigger `auxPrice`).
#[test]
fn an_unknown_kind_is_refused_by_name_and_a_take_profit_is_placed_as_an_if_touched_order() {
    use vike_execution::{ExecutionClient, event_channel};
    vike_log::test_init();
    let (events, mut rx) = event_channel(16);
    let mut fake = IbkrTransportFake::new();
    fake.script(crate::testing::ScriptedInbound::NextValidId(101));
    fake.script(crate::testing::ScriptedInbound::AccountsReady);
    let specs = fake.place_specs();
    let mut handle = crate::run_execution_for_test(events, Box::new(fake));
    let lit = OrderRequest {
        order_type: OrderType::TakeProfit,
        trigger_price: Some(210.0),
        ..vike_bridge_core::execution_drive::limit("ibkr", "tp-1", "AAPL.SMART.USD", 1.0, 211.0)
    };
    handle.submit(&OrderRequest {
        order_client_id: "trail-1".into(),
        order_type: OrderType::Unrecognized("trail".into()),
        ..lit.clone()
    });
    let evs =
        vike_bridge_core::execution_drive::drain(|| rx.try_recv().ok(), 2, Duration::from_secs(2));
    assert!(matches!(evs.first(), Some(Event::OrderSubmitted(_))), "{evs:?}");
    match evs.get(1) {
        Some(Event::OrderRejected(r)) => {
            assert_eq!(r.order_client_id, "trail-1");
            assert!(r.reason.contains("`trail`"), "{}", r.reason);
        }
        other => panic!("expected the refusal, got {other:?} in {evs:?}"),
    }
    assert!(specs.lock().unwrap().is_empty(), "nothing was placed");

    handle.submit(&lit);
    assert!(
        wait_until(Duration::from_secs(2), || specs.lock().unwrap().len() == 1),
        "the take-profit is placed"
    );
    let spec = specs.lock().unwrap()[0].clone();
    assert_eq!(
        (spec.order_type, spec.lmt_price, spec.aux_price),
        ("LIT", Some(211.0), Some(210.0))
    );
    handle.detach();
}

/// **The doors' order-terms rule holds in the bridge too** (`vike_model::order_terms_refusal`): a stop-limit with no
/// trigger, sent by a caller with no door in front, is refused by name before anything is sent, where it would have gone
/// out as an `STP LMT` with no `auxPrice`. A stop-limit with both prices is placed.
#[test]
fn a_stop_limit_with_no_trigger_is_refused_by_name_and_reaches_no_transport() {
    use vike_execution::{ExecutionClient, event_channel};
    vike_log::test_init();
    let (events, mut rx) = event_channel(16);
    let mut fake = IbkrTransportFake::new();
    fake.script(crate::testing::ScriptedInbound::NextValidId(101));
    fake.script(crate::testing::ScriptedInbound::AccountsReady);
    let specs = fake.place_specs();
    let mut handle = crate::run_execution_for_test(events, Box::new(fake));
    let stop_limit = OrderRequest {
        order_type: OrderType::StopLimit,
        ..vike_bridge_core::execution_drive::limit("ibkr", "sl-bare", "AAPL.SMART.USD", 1.0, 211.0)
    };
    handle.submit(&stop_limit);
    let evs =
        vike_bridge_core::execution_drive::drain(|| rx.try_recv().ok(), 2, Duration::from_secs(2));
    match evs.get(1) {
        Some(Event::OrderRejected(r)) => {
            assert_eq!(r.order_client_id, "sl-bare");
            assert_eq!(
                r.reason.to_string(),
                "ibkr: a stop_limit order needs a trigger price above zero; nothing was sent"
            );
        }
        other => panic!("expected the refusal, got {other:?} in {evs:?}"),
    }
    assert!(specs.lock().unwrap().is_empty(), "nothing was placed");

    handle.submit(&OrderRequest {
        order_client_id: "sl-full".into(),
        trigger_price: Some(210.0),
        ..stop_limit
    });
    assert!(
        wait_until(Duration::from_secs(2), || specs.lock().unwrap().len() == 1),
        "a stop-limit with both prices is placed"
    );
    assert_eq!(specs.lock().unwrap()[0].order_type, "STP LMT");
    handle.detach();
}

/// **The doors' attribute and adjustment rules hold in the bridge too** (`vike_model::attributes_refusal`,
/// `vike_model::stop_adjust_refusal`): an all-or-none `ioc` limit and a sell stop whose adjustment level sits below its stop
/// are refused by name with nothing sent; an all-or-none GTC limit is placed with `allOrNone`.
#[test]
fn an_attribute_or_an_adjustment_its_order_cannot_carry_is_refused_and_reaches_no_transport() {
    use vike_execution::{ExecutionClient, event_channel};
    use vike_model::{OrderAttributes, OrderStopAdjust, OrderStopAdjustTo, TimeInForce};
    vike_log::test_init();
    let (events, mut rx) = event_channel(16);
    let mut fake = IbkrTransportFake::new();
    fake.script(crate::testing::ScriptedInbound::NextValidId(101));
    fake.script(crate::testing::ScriptedInbound::AccountsReady);
    let specs = fake.place_specs();
    let mut handle = crate::run_execution_for_test(events, Box::new(fake));
    let aon = OrderRequest {
        attributes: Some(OrderAttributes { all_or_none: true, ..Default::default() }),
        ..vike_bridge_core::execution_drive::limit("ibkr", "aon-ioc", "AAPL.SMART.USD", 1.0, 150.0)
    };
    let low_adjust = OrderRequest {
        order_client_id: "adj-low".into(),
        order_type: OrderType::Stop,
        side: -1,
        price: None,
        trigger_price: Some(180.0),
        attributes: None,
        adjust: Some(OrderStopAdjust { at: 170.0, to: OrderStopAdjustTo::Stop { stop: 165.0 } }),
        ..aon.clone()
    };
    handle.submit(&OrderRequest { time_in_force: TimeInForce::Ioc, ..aon.clone() });
    handle.submit(&low_adjust);
    let evs =
        vike_bridge_core::execution_drive::drain(|| rx.try_recv().ok(), 4, Duration::from_secs(2));
    let refused: Vec<String> = evs
        .iter()
        .filter_map(|e| match e {
            Event::OrderRejected(r) => Some(format!("{}: {}", r.order_client_id, r.reason)),
            _ => None,
        })
        .collect();
    assert_eq!(refused.len(), 2, "{evs:?}");
    assert!(
        refused[0].starts_with("aon-ioc: ibkr: an all-or-none")
            && refused[0].ends_with("nothing was sent")
    );
    assert!(refused[1].starts_with("adj-low: ibkr: the adjustment level 170"), "{}", refused[1]);
    assert!(specs.lock().unwrap().is_empty(), "nothing was placed");

    handle.submit(&OrderRequest { order_client_id: "aon-gtc".into(), ..aon });
    assert!(
        wait_until(Duration::from_secs(2), || specs.lock().unwrap().len() == 1),
        "an all-or-none GTC limit is placed"
    );
    let spec = specs.lock().unwrap()[0].clone();
    assert_eq!((spec.order_type, spec.all_or_none), ("LMT", true));
    handle.detach();
}

/// **The trailing kinds go through the real loop** (wave 3, as the paper gateway answered them on 2026-11-09): a trailing
/// order with no trail is refused by the doors' order-terms rule before anything is sent; a `trailing_limit` (IB wants its
/// starting stop) and a `trailing` with an activation (IB held its price condition unanswered) are refused by name, nothing
/// sent; a plain `trailing` is placed as IB's `TRAIL`, the trail as `auxPrice`, no `lmtPrice`, no activation.
#[test]
fn a_trailing_order_is_placed_as_trail_and_a_trailing_limit_or_an_activation_is_refused() {
    use vike_execution::{ExecutionClient, event_channel};
    vike_log::test_init();
    let (events, mut rx) = event_channel(16);
    let mut fake = IbkrTransportFake::new();
    fake.script(crate::testing::ScriptedInbound::NextValidId(101));
    fake.script(crate::testing::ScriptedInbound::AccountsReady);
    let specs = fake.place_specs();
    let mut handle = crate::run_execution_for_test(events, Box::new(fake));
    let trailing = OrderRequest {
        order_type: OrderType::TrailingLimit,
        price: None,
        trigger_price: Some(230.0),
        limit_offset: Some(0.5),
        side: -1,
        ..vike_bridge_core::execution_drive::limit("ibkr", "tl-bare", "AAPL.SMART.USD", 1.0, 211.0)
    };
    handle.submit(&trailing);
    let evs =
        vike_bridge_core::execution_drive::drain(|| rx.try_recv().ok(), 2, Duration::from_secs(2));
    match evs.get(1) {
        Some(Event::OrderRejected(r)) => {
            assert_eq!(r.order_client_id, "tl-bare");
            assert!(r.reason.contains("needs a trail distance above zero"), "{}", r.reason);
        }
        other => panic!("expected the refusal, got {other:?} in {evs:?}"),
    }
    assert!(specs.lock().unwrap().is_empty(), "nothing was placed");

    for (order_client_id, req, words) in [
        (
            "tl-full",
            OrderRequest {
                order_client_id: "tl-full".into(),
                trail: Some(2.0),
                trigger_price: None,
                ..trailing.clone()
            },
            "trailStopPrice",
        ),
        (
            "tr-act",
            OrderRequest {
                order_client_id: "tr-act".into(),
                order_type: OrderType::Trailing,
                trail: Some(2.0),
                limit_offset: None,
                ..trailing.clone()
            },
            "activation",
        ),
    ] {
        handle.submit(&req);
        let evs = vike_bridge_core::execution_drive::drain(
            || rx.try_recv().ok(),
            2,
            Duration::from_secs(2),
        );
        match evs.get(1) {
            Some(Event::OrderRejected(r)) if r.order_client_id == order_client_id => {
                assert!(
                    r.reason.contains(words) && r.reason.contains("nothing was sent"),
                    "{}",
                    r.reason
                );
            }
            other => panic!("{order_client_id}: expected the refusal, got {other:?} in {evs:?}"),
        }
    }
    assert!(specs.lock().unwrap().is_empty(), "nothing was placed");

    handle.submit(&OrderRequest {
        order_client_id: "tr-full".into(),
        order_type: OrderType::Trailing,
        trail: Some(2.0),
        trigger_price: None,
        limit_offset: None,
        ..trailing
    });
    assert!(
        wait_until(Duration::from_secs(2), || specs.lock().unwrap().len() == 1),
        "a trailing order with its trail is placed"
    );
    let spec = specs.lock().unwrap()[0].clone();
    assert_eq!(
        (spec.order_type, spec.lmt_price, spec.aux_price, spec.lmt_price_offset, spec.activation),
        ("TRAIL", None, Some(2.0), None, None)
    );
    handle.detach();
}

/// **A refusal IB sends right behind its own end of a FRESH order reaches the core in IB's words** (paper #12,
/// 2026-11-09: IB accepted an all-or-none limit, ended it 0.2 ms later and sent `[201] ...` 2 ms after that; the end went
/// out bare and the words were only logged). Through the real loop: the end is held, the refusal ends the order as an
/// `OrderCanceled` carrying IB's words (the engine's FSM takes no reject after `Accepted`), and an end with no refusal
/// behind it goes out bare once the hold ran out.
#[test]
fn a_refusal_right_behind_ibs_own_end_of_a_fresh_order_reaches_the_core_in_ibs_words() {
    use crate::execution::event_mapper::REFUSAL_HOLD_MS;
    use crate::testing::ScriptedInbound;
    use vike_execution::{ExecutionClient, event_channel};
    vike_log::test_init();
    const WORDS: &str = "[201] Order rejected - reason:All-or-none orders not supported for this \
                         combination of exchange and security type.";
    let (events, mut rx) = event_channel(32);
    let mut fake = IbkrTransportFake::new();
    fake.script(ScriptedInbound::NextValidId(101));
    fake.script(ScriptedInbound::AccountsReady);
    fake.on_place_order(|order_id| {
        let status = |word: &str| ScriptedInbound::OrderStatus {
            order_id,
            order_ref: String::new(),
            status: word.into(),
        };
        let mut ibkr = vec![status("PreSubmitted"), status("Cancelled")];
        if order_id == 101 {
            ibkr.push(ScriptedInbound::Error { code: 201, order_id, msg: WORDS.into() });
        }
        ibkr
    });
    let mut handle = crate::run_execution_for_test(events, Box::new(fake));
    let limit = |order_client_id: &str| {
        vike_bridge_core::execution_drive::limit(
            "ibkr",
            order_client_id,
            "AAPL.SMART.USD",
            1.0,
            150.0,
        )
    };

    handle.submit(&limit("aon"));
    let evs =
        vike_bridge_core::execution_drive::drain(|| rx.try_recv().ok(), 3, Duration::from_secs(3));
    assert!(
        matches!(
            evs.as_slice(),
            [Event::OrderSubmitted(_), Event::OrderAccepted(_), Event::OrderCanceled(c)]
                if c.order_client_id == "aon" && c.reason.as_str() == WORDS
        ),
        "{evs:?}"
    );

    let began = Instant::now();
    handle.submit(&limit("quiet"));
    let evs =
        vike_bridge_core::execution_drive::drain(|| rx.try_recv().ok(), 3, Duration::from_secs(3));
    assert!(
        matches!(
            evs.as_slice(),
            [Event::OrderSubmitted(_), Event::OrderAccepted(_), Event::OrderCanceled(c)]
                if c.order_client_id == "quiet" && c.reason.is_empty()
        ),
        "{evs:?}"
    );
    assert!(
        began.elapsed() >= Duration::from_millis(REFUSAL_HOLD_MS as u64),
        "the bare end waited for IB's words first: {:?}",
        began.elapsed()
    );
    handle.detach();
}

/// **An order IB does not answer at all is looked for in the venue's open orders, once** (`SILENT_SUBMIT_ASK`). On paper
/// 2026-11-10 a limit-on-close placed inside the regular session drew no frame for its order id while IB held it, and the
/// core's watchdog rejects such an order at about 60 s although IB executes it at the close. Listed working, it is accepted.
#[test]
fn an_order_ibkr_does_not_answer_is_accepted_once_the_open_orders_list_it() {
    use crate::transport::ScriptedInbound;
    use std::sync::mpsc::channel;
    use std::sync::{Arc, Mutex};
    use vike_execution::{Ingest, event_channel};

    let (events, mut rx) = event_channel(64);
    let mut fake = IbkrTransportFake::new().with_socket_identity();
    fake.script(ScriptedInbound::NextValidId(101));
    fake.script(ScriptedInbound::AccountsReady);
    // IB writes nothing for the order it was handed; its open orders list it, working.
    let placed: Arc<Mutex<Option<i32>>> = Arc::default();
    let seen = Arc::clone(&placed);
    fake.on_place_order(move |order_id| {
        *seen.lock().unwrap() = Some(order_id);
        Vec::new()
    });
    let listed = Arc::clone(&placed);
    fake.on_request_open_orders(move || {
        let id = *listed.lock().unwrap();
        id.map(|id| vec![ScriptedInbound::venue_order(id, "order-client-id-loc", 1.0)])
            .unwrap_or_default()
    });
    let asks = fake.open_order_requests();
    let (command_tx, command_rx) = channel();
    let transport = Box::new(fake.clone());
    let worker = std::thread::spawn(move || {
        run_loop_with(command_rx, events, transport, Taught::default(), LoopTimes::PRODUCTION);
    });
    let loc = OrderRequest {
        order_client_id: "order-client-id-loc".into(),
        venue: "ibkr".into(),
        symbol: "AAPL.SMART.USD".into(),
        side: 1,
        qty: 1.0,
        order_type: OrderType::Limit,
        price: Some(150.0),
        time_in_force: vike_model::TimeInForce::AtClose,
        ..Default::default()
    };
    command_tx.send(ExecutionClientActorCommand::Submit(Box::new(loc), Instant::now())).unwrap();

    let mut answers = Vec::new();
    let deadline = Instant::now() + Duration::from_secs(12);
    while Instant::now() < deadline
        && !answers.iter().any(|e| matches!(e, Event::OrderAccepted(_) | Event::OrderRejected(_)))
    {
        match rx.try_recv() {
            Ok(Ingest::Event(e)) => answers.push(e),
            Ok(_) => {}
            Err(_) => std::thread::sleep(Duration::from_millis(20)),
        }
    }
    std::thread::sleep(Duration::from_secs(1)); // an answered order is asked about no more
    command_tx.send(ExecutionClientActorCommand::Shutdown).unwrap();
    worker.join().unwrap();

    assert!(placed.lock().unwrap().is_some(), "the order was written: {answers:?}");
    assert!(
        answers.iter().any(
            |e| matches!(e, Event::OrderAccepted(a) if a.order_client_id == "order-client-id-loc")
        ),
        "the order IB lists is accepted: {answers:?}"
    );
    assert_eq!(*asks.lock().unwrap(), 1, "one read of the open orders for the silent order");
}
