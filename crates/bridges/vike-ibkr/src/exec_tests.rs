use super::*;
use crate::exec::amend::PendingAmend;
use crate::exec::event_mapper::{IbCommissionReport, IbExecDetails, IbOrderStatus};
use crate::transport::{AmendOutcome, FakeTransport};
use vike_model::events::Event;

/// The grace window these tests age against — a literal, not the production constant, because
/// what is under test is the WIRING (does a stale entry reach `request_executions`), not the
/// value of the window. `now_ms` is opaque and monotonic, so any scale works.
const GRACE: i64 = 1_000;

/// A mapper holding ONE execution buffered awaiting its commission — the stranded shape.
fn mapper_with_buffered_exec() -> EventMapper {
    let mut ids = IdRegistry::default();
    ids.on_next_valid_id(101);
    let order_id = ids.next_order_id().unwrap();
    ids.bind(order_id, "coid-A");
    let mut m = EventMapper::new(ids);
    m.on_submit(order_id, 10.0);
    let _ = m.on_order_status(IbOrderStatus {
        order_id,
        order_ref: "coid-A".into(),
        status: "Submitted".into(),
        filled: 0.0,
        avg_fill_price: 0.0,
        ..Default::default()
    });
    let evs = m.on_exec_details(IbExecDetails {
        order_id,
        order_ref: "coid-A".into(),
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
    let mut m = mapper_with_buffered_exec();
    let mut t = FakeTransport::new();
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
    let mut ids = IdRegistry::default();
    ids.on_next_valid_id(101);
    let mut m = EventMapper::new(ids);
    let mut t = FakeTransport::new();
    let calls = t.execution_requests();
    for tick in 0..10 {
        sweep_stranded_fills(&mut m, &mut t, tick * GRACE * 10, GRACE);
    }
    assert_eq!(*calls.lock().unwrap(), 0);
}

#[test]
fn sweep_stops_re_requesting_once_the_retry_budget_is_spent() {
    let mut m = mapper_with_buffered_exec();
    let mut t = FakeTransport::new();
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
    let mut m = mapper_with_buffered_exec();
    let mut t = FakeTransport::new();
    let calls = t.execution_requests();
    let evs = fold_inbound(&mut m, &mut t, IbInbound::StreamResync);
    assert!(evs.is_empty(), "the resync itself emits nothing");
    assert_eq!(*calls.lock().unwrap(), 1);
}

#[test]
fn the_replay_a_reconnect_triggers_emits_the_stranded_fill_with_the_real_commission() {
    // End to end through `fold_inbound`: the commission was lost, the socket blipped, the
    // resync replays BOTH halves, and the ordinary join emits the fill — carrying IBKR's own
    // commission figure, never a fabricated 0.0.
    let mut m = mapper_with_buffered_exec();
    let mut t = FakeTransport::new();
    assert!(fold_inbound(&mut m, &mut t, IbInbound::StreamResync).is_empty());

    // What `reqExecutions` delivers: the execution, then its commission report.
    let replayed_exec = IbInbound::ExecDetails(IbExecDetails {
        order_id: 101,
        order_ref: "coid-A".into(),
        exec_id: "e1".into(),
        symbol: "AAPL.SMART.USD".into(),
        side_buy: true,
        shares: 10.0,
        price: 190.0,
        ts: 7,
        ..Default::default()
    });
    assert!(fold_inbound(&mut m, &mut t, replayed_exec).is_empty());
    let replayed_commission = IbInbound::Commission(IbCommissionReport {
        exec_id: "e1".into(),
        commission: 1.25,
        currency: "USD".into(),
    });
    match fold_inbound(&mut m, &mut t, replayed_commission).as_slice() {
        [Event::Fill(bare), Event::OrderFilled(f)] => {
            assert_eq!(bare.commission, f.fill.commission);
            assert_eq!(f.client_order_id, "coid-A");
            assert_eq!(f.fill.trade_id, "e1");
            assert_eq!(f.fill.last_qty, 10.0);
            assert_eq!(f.fill.commission, 1.25);
        }
        other => panic!("expected exactly one OrderFilled, got {other:?}"),
    }

    // A SECOND reconnect replays the same pair again — now inert, so the recovery cannot
    // become a double-fill generator.
    assert!(fold_inbound(&mut m, &mut t, IbInbound::StreamResync).is_empty());
    let again = IbInbound::ExecDetails(IbExecDetails {
        order_id: 101,
        order_ref: "coid-A".into(),
        exec_id: "e1".into(),
        symbol: "AAPL.SMART.USD".into(),
        side_buy: true,
        shares: 10.0,
        price: 190.0,
        ts: 7,
        ..Default::default()
    });
    assert!(fold_inbound(&mut m, &mut t, again).is_empty());
    let again_c = IbInbound::Commission(IbCommissionReport {
        exec_id: "e1".into(),
        commission: 1.25,
        currency: "USD".into(),
    });
    assert!(fold_inbound(&mut m, &mut t, again_c).is_empty());
}

/// End to end through `fold_inbound` (decision 0238): the fill joins its commission, IB then REVISES the figure (and
/// re-sends it), and the revision leaves as a `FeeAdjustment` carrying the total over the fill's fee, once. The
/// engine's own ledger books it by the difference (`vike-exec`'s `fee_adjustment_fold` suite).
#[test]
fn a_revised_commission_leaves_as_a_fee_adjustment_once() {
    let mut m = mapper_with_buffered_exec();
    let mut t = FakeTransport::new();
    let report = |commission: f64| {
        IbInbound::Commission(IbCommissionReport {
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
            assert_eq!((a.trade_id.as_str(), a.client_order_id.as_str()), ("e1", "coid-A"));
            assert_eq!((a.symbol.as_str(), a.last_px, a.ts), ("AAPL.SMART.USD", 190.0, 7));
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
    let mut m = mapper_with_buffered_exec();
    let mut t = FakeTransport::new();
    let calls = t.execution_requests();

    let evs = fold_inbound(
        &mut m,
        &mut t,
        IbInbound::StreamDead { reason: "connection reset by peer".into() },
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
    let mut m = mapper_with_buffered_exec();
    let mut t = FakeTransport::new();
    let calls = t.execution_requests();

    // Alive: the ladder runs (this is `sweep_asks_the_venue_…`'s established behaviour).
    sweep_stranded_fills(&mut m, &mut t, 0, GRACE);
    sweep_stranded_fills(&mut m, &mut t, GRACE, GRACE);
    assert_eq!(*calls.lock().unwrap(), 1, "control: the ladder runs on a live stream");

    // Dead: every further pass must be a no-op, however far the clock is advanced and however
    // much retry budget is left. Without the gate this loop spends the rest of the budget.
    let _ = fold_inbound(&mut m, &mut t, IbInbound::StreamDead { reason: "reset".into() });
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
    let mut resync_m = mapper_with_buffered_exec();
    let mut resync_t = FakeTransport::new();
    let resync_calls = resync_t.execution_requests();
    let _ = fold_inbound(&mut resync_m, &mut resync_t, IbInbound::StreamResync);

    let mut dead_m = mapper_with_buffered_exec();
    let mut dead_t = FakeTransport::new();
    let dead_calls = dead_t.execution_requests();
    let _ = fold_inbound(&mut dead_m, &mut dead_t, IbInbound::StreamDead { reason: "x".into() });

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
    let m = mapper_with_buffered_exec();
    assert_eq!(m.live_client_order_ids(), vec!["coid-A"]);
}

/// The learning backstop: a snapshot request nothing ever ends is FAILED at the deadline, not pending
/// for ever. Observable through its effect: an end marker that turns up AFTER the deadline moves nothing
/// (no executions replay is asked for, the link is not announced up).
#[test]
fn a_snapshot_nothing_ends_is_abandoned_at_the_learning_deadline() {
    use crate::transport::{ScriptedInbound, Snapshot, SnapshotMode};
    use std::sync::mpsc::channel;
    use vike_exec::{Ingest, event_channel};

    let (events, mut rx) = event_channel(64);
    let mut fake = FakeTransport::new().with_socket_identity();
    fake.script_socket_connect(101);
    fake.open_orders_end(SnapshotMode::Silent);
    let exec_requests = fake.execution_requests();
    let (cmd_tx, cmd_rx) = channel();
    let transport = Box::new(fake.clone());
    let worker = std::thread::spawn(move || {
        run_loop(
            cmd_rx,
            events,
            transport,
            Taught::default(),
            Duration::from_millis(200),
            Duration::from_millis(100),
        );
    });

    std::thread::sleep(Duration::from_millis(600)); // well past the deadline
    fake.inject(ScriptedInbound::SnapshotEnd { snapshot: Snapshot::OpenOrders, complete: true });
    std::thread::sleep(Duration::from_millis(300));
    cmd_tx.send(ExecCommand::Shutdown).unwrap();
    worker.join().unwrap();

    assert_eq!(*exec_requests.lock().unwrap(), 0, "a late end moves nothing once abandoned");
    while let Ok(i) = rx.try_recv() {
        assert!(!matches!(i, Ingest::ExecLink(_)), "no link transition: {i:?}");
    }
}

/// **R5, the bound.** The wait for the learning is bounded: a transport that never ends it does not hold a
/// submit for ever; past the bound the order is REFUSED terminally (F6: the connection does not know which
/// orders of this client rest, so an id handed out now could be a resting order's), and a cancel of an unknown
/// coid is refused loudly.
#[test]
fn the_wait_for_the_learning_is_bounded() {
    use crate::transport::SnapshotMode;
    use std::sync::mpsc::channel;
    use vike_exec::{Ingest, event_channel};
    use vike_model::OrderRequest;

    let (events, mut rx) = event_channel(64);
    let mut fake = FakeTransport::new().with_socket_identity();
    fake.script_socket_connect(101);
    fake.open_orders_end(SnapshotMode::Silent);
    let places = fake.place_calls();
    let (cmd_tx, cmd_rx) = channel();
    let transport = Box::new(fake.clone());
    let worker = std::thread::spawn(move || {
        run_loop(
            cmd_rx,
            events,
            transport,
            Taught::default(),
            Duration::from_secs(30),
            Duration::from_millis(250),
        );
    });
    std::thread::sleep(Duration::from_millis(100));

    let order = OrderRequest {
        client_order_id: "coid-late".into(),
        venue: "ibkr".into(),
        symbol: "AAPL.SMART.USD".into(),
        side: 1,
        qty: 1.0,
        order_type: vike_model::OrderType::Market,
        ..Default::default()
    };
    cmd_tx.send(ExecCommand::Submit(Box::new(order))).unwrap();
    cmd_tx
        .send(ExecCommand::Cancel {
            client_order_id: "coid-unknown".into(),
            intent: Default::default(),
        })
        .unwrap();
    std::thread::sleep(Duration::from_millis(1_200));
    cmd_tx.send(ExecCommand::Shutdown).unwrap();
    worker.join().unwrap();

    assert!(places.lock().unwrap().is_empty(), "nothing was placed once the bound passed");
    let (mut refused, mut rejected) = (false, false);
    while let Ok(i) = rx.try_recv() {
        match i {
            Ingest::Event(Event::OrderCancelRejected(r)) => {
                refused |= r.client_order_id == "coid-unknown";
            }
            Ingest::Event(Event::OrderRejected(r)) => rejected |= r.client_order_id == "coid-late",
            _ => {}
        }
    }
    assert!(rejected, "the submit is rejected terminally, with words");
    assert!(refused, "and the cancel of an unknown coid is refused loudly after its own bound");
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
    use crate::transport::{Snapshot, SnapshotMode};
    let mut m = mapper_with_buffered_exec();
    let mut t = FakeTransport::new().with_socket_identity();
    let calls = t.execution_requests();
    t.open_orders_end(SnapshotMode::Silent);

    m.begin_learning();
    sweep_stranded_fills(&mut m, &mut t, 0, GRACE);
    sweep_stranded_fills(&mut m, &mut t, GRACE, GRACE);
    assert_eq!(*calls.lock().unwrap(), 0, "learning the open orders");
    let _ = m.snapshot_ended(Snapshot::OpenOrders, true);
    sweep_stranded_fills(&mut m, &mut t, 2 * GRACE, GRACE);
    assert_eq!(*calls.lock().unwrap(), 0, "learning the executions");
    assert_eq!(
        m.learning(),
        Learning::Executions,
        "and the learning is still waiting for ITS replay"
    );

    // Once the learning is over the buffered execution is swept as ever.
    let _ = m.snapshot_ended(Snapshot::Executions, true);
    sweep_stranded_fills(&mut m, &mut t, 3 * GRACE, GRACE); // stamps the entry
    sweep_stranded_fills(&mut m, &mut t, 4 * GRACE, GRACE);
    assert_eq!(*calls.lock().unwrap(), 1);
}

/// **F7, the loop's upkeep.** `sweep_tick` asks the venue for the commission a cancel is held for once the bound runs out,
/// releases the cancel one reply window later if nothing came, and sends what comes out on the lane, with the stream
/// dead or not.
#[test]
fn the_upkeep_pass_releases_a_cancel_held_for_a_commission_that_did_not_come() {
    use crate::exec::event_mapper::COMMISSION_REREQUEST_WAIT_MS;
    use vike_exec::{Ingest, event_channel};

    let (events, mut rx) = event_channel(16);
    let mut m = mapper_with_buffered_exec();
    let mut t = FakeTransport::new();
    let requests = t.execution_requests();
    let held = m.on_order_status(IbOrderStatus {
        order_id: 101,
        order_ref: "coid-A".into(),
        status: "Cancelled".into(),
        filled: 10.0,
        avg_fill_price: 190.0,
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
    while let Ok(Ingest::Event(ev)) = rx.try_recv() {
        got.push(ev);
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

/// **F6.** A cancel of a coid the connection does not know YET (the learning is still running when the wait
/// runs out) is answered with the advisory and RETRIED once, when the learning ends and has bound the order.
#[test]
fn a_cancel_of_an_unknown_coid_during_the_learning_is_retried_when_the_learning_ends() {
    use crate::transport::{ScriptedInbound, Snapshot, SnapshotMode};
    use std::sync::mpsc::channel;
    use vike_exec::{Ingest, event_channel};

    let (events, mut rx) = event_channel(64);
    let mut fake = FakeTransport::new().with_socket_identity();
    fake.script_socket_connect(101);
    fake.open_orders_end(SnapshotMode::Silent);
    let cancels = fake.cancel_calls();
    let (cmd_tx, cmd_rx) = channel();
    let transport = Box::new(fake.clone());
    let worker = std::thread::spawn(move || {
        run_loop(
            cmd_rx,
            events,
            transport,
            Taught::default(),
            Duration::from_secs(30),
            Duration::from_millis(100),
        );
    });
    std::thread::sleep(Duration::from_millis(100));
    cmd_tx
        .send(ExecCommand::Cancel {
            client_order_id: "coid-rested".into(),
            intent: Default::default(),
        })
        .unwrap();
    std::thread::sleep(Duration::from_millis(400)); // past the wait: the advisory went out
    assert!(cancels.lock().unwrap().is_empty(), "nothing is sent for an order nobody knows");

    // The learning ends and has bound the order: the deferred cancel goes out, once.
    fake.inject(ScriptedInbound::venue_order(105, "coid-rested", 10.0));
    fake.inject(ScriptedInbound::SnapshotEnd { snapshot: Snapshot::OpenOrders, complete: true });
    fake.inject(ScriptedInbound::SnapshotEnd { snapshot: Snapshot::Executions, complete: true });
    std::thread::sleep(Duration::from_millis(500));
    cmd_tx.send(ExecCommand::Shutdown).unwrap();
    worker.join().unwrap();

    assert_eq!(
        *cancels.lock().unwrap(),
        [105],
        "the retry reaches the venue under IB's own id, once"
    );
    let mut advisories = 0;
    while let Ok(i) = rx.try_recv() {
        if let Ingest::Event(Event::OrderCancelRejected(r)) = i {
            advisories += usize::from(r.client_order_id == "coid-rested");
        }
    }
    assert_eq!(advisories, 1, "the caller was told once that the request was not sent at first");
}

/// A deferred cancel is NOT retried when the learning failed: the connection still does not know the order.
#[test]
fn a_deferred_cancel_is_not_retried_when_the_learning_fails() {
    use crate::transport::{ScriptedInbound, Snapshot, SnapshotMode};
    use std::sync::mpsc::channel;
    use vike_exec::event_channel;

    let (events, _rx) = event_channel(64);
    let mut fake = FakeTransport::new().with_socket_identity();
    fake.script_socket_connect(101);
    fake.open_orders_end(SnapshotMode::Silent);
    let cancels = fake.cancel_calls();
    let (cmd_tx, cmd_rx) = channel();
    let transport = Box::new(fake.clone());
    let worker = std::thread::spawn(move || {
        run_loop(
            cmd_rx,
            events,
            transport,
            Taught::default(),
            Duration::from_secs(30),
            Duration::from_millis(100),
        );
    });
    std::thread::sleep(Duration::from_millis(100));
    cmd_tx
        .send(ExecCommand::Cancel { client_order_id: "coid-x".into(), intent: Default::default() })
        .unwrap();
    std::thread::sleep(Duration::from_millis(300));
    fake.inject(ScriptedInbound::venue_order(105, "coid-x", 10.0));
    fake.inject(ScriptedInbound::SnapshotEnd { snapshot: Snapshot::OpenOrders, complete: false });
    std::thread::sleep(Duration::from_millis(400));
    cmd_tx.send(ExecCommand::Shutdown).unwrap();
    worker.join().unwrap();
    assert!(cancels.lock().unwrap().is_empty(), "a failed learning bound nothing and sent nothing");
}

/// The host's teaching (`adopt_restored`) reaches the exec loop's mapper: the fill of a rebound order carries the
/// symbol the engine registered it under, not the instrument IB echoed.
#[test]
fn what_the_host_taught_about_a_restored_order_stamps_its_fills() {
    use crate::transport::ScriptedInbound;
    use std::sync::mpsc::channel;
    use vike_exec::{Ingest, event_channel};

    let (events, mut rx) = event_channel(64);
    let mut fake = FakeTransport::new().with_socket_identity();
    fake.script_socket_connect(101);
    fake.on_request_open_orders(|| vec![ScriptedInbound::venue_order(105, "coid-rested", 10.0)]);
    fake.on_request_executions(|| {
        vec![
            ScriptedInbound::ExecutionOf {
                order_id: 105,
                order_ref: "coid-rested".into(),
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
    taught.teach(&[("coid-rested".to_string(), "EUR.USD.IDEALPRO".to_string())]);
    let (cmd_tx, cmd_rx) = channel();
    let transport = Box::new(fake.clone());
    let run_taught = taught.clone();
    let worker = std::thread::spawn(move || {
        run_loop(
            cmd_rx,
            events,
            transport,
            run_taught,
            Duration::from_secs(30),
            Duration::from_millis(100),
        );
    });
    std::thread::sleep(Duration::from_millis(500));
    cmd_tx.send(ExecCommand::Shutdown).unwrap();
    worker.join().unwrap();

    let mut symbols = Vec::new();
    while let Ok(i) = rx.try_recv() {
        if let Ingest::Event(Event::Fill(f)) = i {
            symbols.push(f.symbol.to_string());
        }
    }
    assert_eq!(symbols, ["EUR.USD.IDEALPRO"]);
}

/// The amend of a coid not known yet during the learning is deferred like the cancel: the advisory now, ONE retry
/// when the learning has bound the order.
#[test]
fn an_amend_of_an_unknown_coid_during_the_learning_is_retried_when_the_learning_ends() {
    use crate::transport::{ScriptedInbound, Snapshot, SnapshotMode};
    use std::sync::mpsc::channel;
    use vike_exec::{Ingest, event_channel};

    let (events, mut rx) = event_channel(64);
    let mut fake = FakeTransport::new().with_socket_identity();
    fake.script_socket_connect(101);
    fake.open_orders_end(SnapshotMode::Silent);
    let modifies = fake.modify_calls();
    let (cmd_tx, cmd_rx) = channel();
    let transport = Box::new(fake.clone());
    let worker = std::thread::spawn(move || {
        run_loop(
            cmd_rx,
            events,
            transport,
            Taught::default(),
            Duration::from_secs(30),
            Duration::from_millis(100),
        );
    });
    std::thread::sleep(Duration::from_millis(100));
    let resting = vike_model::OrderRequest {
        client_order_id: "coid-rested".into(),
        venue: "ibkr".into(),
        symbol: "AAPL.SMART.USD".into(),
        side: 1,
        qty: 10.0,
        price: Some(100.0),
        order_type: vike_model::OrderType::Limit,
        ..Default::default()
    };
    cmd_tx
        .send(ExecCommand::Modify {
            order: Box::new(resting),
            new_qty: None,
            new_price: Some(101.0),
        })
        .unwrap();
    std::thread::sleep(Duration::from_millis(400));
    assert!(modifies.lock().unwrap().is_empty(), "nothing is sent for an order nobody knows yet");

    fake.inject(ScriptedInbound::venue_order(105, "coid-rested", 10.0));
    fake.inject(ScriptedInbound::SnapshotEnd { snapshot: Snapshot::OpenOrders, complete: true });
    fake.inject(ScriptedInbound::SnapshotEnd { snapshot: Snapshot::Executions, complete: true });
    std::thread::sleep(Duration::from_millis(500));
    cmd_tx.send(ExecCommand::Shutdown).unwrap();
    worker.join().unwrap();

    assert_eq!(
        *modifies.lock().unwrap(),
        [105],
        "the retry reaches the transport under IB's own id"
    );
    let mut advisories = 0;
    while let Ok(i) = rx.try_recv() {
        if let Ingest::Event(Event::OrderModifyRejected(r)) = i {
            advisories += usize::from(r.client_order_id == "coid-rested");
        }
    }
    assert_eq!(advisories, 1, "the caller was told once");
}

// ---- W14f ----------------------------------------------------------------------------------------------

/// The waits before the retries of a failed learning, shortened.
const RETRY_50: &[Duration] = &[Duration::from_millis(50), Duration::from_millis(50)];
const RETRY_30: &[Duration] = &[Duration::from_millis(30), Duration::from_millis(30)];

/// Poll `cond` for up to five seconds.
fn wait_for(cond: impl Fn() -> bool) -> bool {
    let deadline = Instant::now() + Duration::from_secs(5);
    while Instant::now() < deadline {
        if cond() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    cond()
}

/// The link transitions (`true` = up) and the symbols of the fills in what `rx` holds now.
fn link_and_fills(
    rx: &mut tokio::sync::mpsc::Receiver<vike_exec::Ingest>,
) -> (Vec<bool>, Vec<String>) {
    use vike_exec::Ingest;
    let (mut links, mut symbols) = (Vec::new(), Vec::new());
    while let Ok(i) = rx.try_recv() {
        match i {
            Ingest::ExecLink(u) => links.push(u.up),
            Ingest::Event(Event::Fill(f)) => symbols.push(f.symbol.to_string()),
            _ => {}
        }
    }
    (links, symbols)
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
    use vike_exec::event_channel;

    let (events, mut rx) = event_channel(64);
    let mut fake = FakeTransport::new().with_socket_identity();
    fake.script_socket_connect(101);
    fake.on_request_open_orders(|| vec![ScriptedInbound::venue_order(105, "coid-rested", 10.0)]);
    fake.on_request_executions(|| {
        vec![
            ScriptedInbound::ExecutionOf {
                order_id: 105,
                order_ref: "coid-rested".into(),
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
    let (cmd_tx, cmd_rx) = channel();
    let transport = Box::new(fake.clone());
    let run_taught = taught.clone();
    let worker = std::thread::spawn(move || {
        run_loop(
            cmd_rx,
            events,
            transport,
            run_taught,
            Duration::from_secs(30),
            Duration::from_millis(100),
        );
    });
    // The open orders are in and the loop is parked on the replay: it has said so, and asked for nothing.
    assert!(
        wait_for(|| taught.is_awaiting() && fake.undelivered() == 0),
        "the loop waits for the host"
    );
    assert_eq!(*exec_requests.lock().unwrap(), 0, "the replay is held back until the handover");
    assert!(link_and_fills(&mut rx).0.is_empty(), "and the link is not announced up");

    // The handover: the replay runs, and its fill carries the symbol the host taught.
    taught.teach(&[("coid-rested".to_string(), "EUR.USD.IDEALPRO".to_string())]);
    assert!(wait_for(|| *exec_requests.lock().unwrap() == 1));
    std::thread::sleep(Duration::from_millis(300));
    cmd_tx.send(ExecCommand::Shutdown).unwrap();
    worker.join().unwrap();
    let (links, symbols) = link_and_fills(&mut rx);
    assert_eq!(symbols, ["EUR.USD.IDEALPRO"]);
    assert_eq!(links, [true], "learned, announced once, after the replay");
}

/// A handover with nothing to teach (an empty `adopt_restored`, a first start) lets the replay through at once.
#[test]
fn an_empty_handover_lets_the_replay_through() {
    use std::sync::mpsc::channel;
    use vike_exec::event_channel;

    let (events, mut rx) = event_channel(64);
    let mut fake = FakeTransport::new().with_socket_identity();
    fake.script_socket_connect(101);
    let taught = Taught::gated();
    let (cmd_tx, cmd_rx) = channel();
    let transport = Box::new(fake.clone());
    let run_taught = taught.clone();
    let worker = std::thread::spawn(move || {
        run_loop(
            cmd_rx,
            events,
            transport,
            run_taught,
            Duration::from_secs(30),
            Duration::from_millis(100),
        );
    });
    assert!(wait_for(|| taught.is_awaiting()));
    taught.teach(&[]);
    assert!(wait_for(|| !taught.is_awaiting()));
    std::thread::sleep(Duration::from_millis(200));
    cmd_tx.send(ExecCommand::Shutdown).unwrap();
    worker.join().unwrap();
    assert_eq!(link_and_fills(&mut rx).0, [true]);
}

/// The fail-safe: a host that never hands the client over does not hold the learning for ever. The replay goes
/// ahead after the (shortened) bound, untaught.
#[test]
fn a_host_that_never_comes_does_not_hold_the_replay_for_ever() {
    use std::sync::mpsc::channel;
    use vike_exec::event_channel;

    let (events, mut rx) = event_channel(64);
    let mut fake = FakeTransport::new().with_socket_identity();
    fake.script_socket_connect(101);
    let exec_requests = fake.execution_requests();
    let (cmd_tx, cmd_rx) = channel();
    let transport = Box::new(fake.clone());
    let times = LoopTimes {
        learn_deadline: Duration::from_secs(30),
        learn_wait: Duration::from_millis(100),
        teach_wait: Duration::from_millis(300),
        retry: &[],
    };
    let worker = std::thread::spawn(move || {
        run_loop_with(cmd_rx, events, transport, Taught::gated(), times);
    });
    assert!(wait_for(|| *exec_requests.lock().unwrap() == 1), "the bound let the replay through");
    std::thread::sleep(Duration::from_millis(200));
    cmd_tx.send(ExecCommand::Shutdown).unwrap();
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
    use vike_exec::event_channel;

    let (events, mut rx) = event_channel(64);
    let mut fake = FakeTransport::new().with_socket_identity();
    fake.script_socket_connect(101);
    fake.on_request_executions(|| {
        vec![
            ScriptedInbound::ExecutionOf {
                order_id: 900,
                order_ref: "coid-old".into(),
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
    let (cmd_tx, cmd_rx) = channel();
    let transport = Box::new(fake.clone());
    let times = LoopTimes {
        learn_deadline: Duration::from_secs(30),
        learn_wait: Duration::from_millis(100),
        teach_wait: Duration::from_millis(300),
        retry: &[],
    };
    let worker = std::thread::spawn(move || {
        run_loop_with(cmd_rx, events, transport, Taught::gated(), times);
    });
    assert!(wait_for(|| *exec_requests.lock().unwrap() == 1), "the bound let the replay through");
    std::thread::sleep(Duration::from_millis(300));
    cmd_tx.send(ExecCommand::Shutdown).unwrap();
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
    use vike_exec::event_channel;

    let (events, mut rx) = event_channel(64);
    let mut fake = FakeTransport::new().with_socket_identity();
    fake.script_socket_connect(101);
    fake.open_orders_ends(&[SnapshotMode::Failed, SnapshotMode::Complete]);
    let open_requests = fake.open_order_requests();
    let (cmd_tx, cmd_rx) = channel();
    let transport = Box::new(fake.clone());
    let times = LoopTimes {
        learn_deadline: Duration::from_secs(30),
        learn_wait: Duration::from_millis(100),
        teach_wait: TEACH_WAIT,
        retry: RETRY_50,
    };
    let worker = std::thread::spawn(move || {
        run_loop_with(cmd_rx, events, transport, Taught::default(), times);
    });
    assert!(wait_for(|| *open_requests.lock().unwrap() == 2), "asked again after the failure");
    std::thread::sleep(Duration::from_millis(300));
    cmd_tx.send(ExecCommand::Shutdown).unwrap();
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
    use vike_exec::event_channel;

    let (events, mut rx) = event_channel(64);
    let mut fake = FakeTransport::new().with_socket_identity();
    fake.script_socket_connect(101);
    fake.open_orders_end(SnapshotMode::Failed);
    let open_requests = fake.open_order_requests();
    let (cmd_tx, cmd_rx) = channel();
    let transport = Box::new(fake.clone());
    let times = LoopTimes {
        learn_deadline: Duration::from_secs(30),
        learn_wait: Duration::from_millis(100),
        teach_wait: TEACH_WAIT,
        retry: RETRY_30,
    };
    let worker = std::thread::spawn(move || {
        run_loop_with(cmd_rx, events, transport, Taught::default(), times);
    });
    assert!(wait_for(|| *open_requests.lock().unwrap() == 3));
    std::thread::sleep(Duration::from_millis(400));
    cmd_tx.send(ExecCommand::Shutdown).unwrap();
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
        _: &crate::order::IbOrderSpec,
        _: &crate::contract::IbkrContract,
    ) {
    }
    fn cancel_order(&mut self, _: i32) {}
    fn request_open_orders(&mut self) {}
    fn next_recv(&mut self, _: Duration) -> Option<IbInbound> {
        None
    }
}

/// A backend that cannot amend REFUSES, in words. The trait default used to be a silent no-op, which is exactly
/// how a modify came to be accepted by the control door and then do nothing.
#[test]
fn a_transport_without_an_amend_refuses_in_words() {
    let spec = crate::order::IbOrderSpec {
        action: "BUY",
        total_qty: 1.0,
        order_type: "LMT",
        lmt_price: Some(1.0),
        aux_price: None,
        tif: "GTC",
        order_ref: "c".into(),
    };
    let contract = crate::contract::parse_simplified("AAPL.SMART.USD").unwrap();
    let out = NoAmend.modify_order(101, &spec, &contract);
    assert!(
        matches!(&out, AmendOutcome::Refused(why) if why.contains("no way to amend")),
        "got {out:?}"
    );
}

/// The loop's upkeep reports an amend TWS never echoed, once, after the bound; before it nothing goes out.
#[test]
fn an_amend_no_echo_answers_is_reported_unconfirmed_by_the_upkeep() {
    use vike_exec::{Ingest, event_channel};
    let (events, mut rx) = event_channel(16);
    let mut m = mapper_with_buffered_exec();
    let mut t = FakeTransport::new();
    assert!(
        m.note_amend("coid-A", PendingAmend::new(101, (10.0, Some(1.0)), (10.0, Some(2.0)), false))
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
            assert_eq!(r.client_order_id, "coid-A");
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
