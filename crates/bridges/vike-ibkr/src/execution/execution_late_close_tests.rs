//! **A close that waited reads the position first** ([`size_late_close`]): IB's order has no reduce-only word, so a flagged
//! market past the age limit on a position that has gone would go out plain and open the other way, and so would a second
//! close behind one the core rejected at 60-75 s. One that cannot be sized yet waits with the loop still running
//! ([`ParkedCloses`]). The real loop over the fake transport.

use std::sync::mpsc::{Sender, channel};
use std::time::Instant;

use vike_execution::{Ingest, event_channel};

use super::*;
use crate::transport::{IbkrTransportFake, ScriptedInbound};

/// A user name of `crate::testing::operator_rows` (decision 0290), held by every test that runs the loop.
const SYMBOL: &str = "AAPL";

/// Asked again at once and given up soon, so a test that waits for the give-up takes milliseconds.
const TIMES: LoopTimes = LoopTimes {
    learn_wait: Duration::from_millis(100),
    close_retry: Duration::from_millis(2),
    close_retry_for: Duration::from_millis(60),
    row_settle: Duration::from_millis(10),
    ..LoopTimes::PRODUCTION
};

/// [`TIMES`] with no give-up in sight: what the test does ends the wait, not the clock.
const NO_GIVING_UP: LoopTimes = LoopTimes { close_retry_for: Duration::from_secs(60), ..TIMES };

/// A flatten leg, a margin call's close or a fired stop, as the core marks them.
fn close(order_client_id: &str, side: i32, qty: f64) -> OrderRequest {
    OrderRequest {
        order_client_id: order_client_id.into(),
        venue: "ibkr".into(),
        symbol: SYMBOL.into(),
        side,
        qty,
        order_type: OrderType::Market,
        reduce_only: true,
        ..Default::default()
    }
}

/// When an order joined the queue so that it is past the age limit now.
fn waited() -> Instant {
    Instant::now().checked_sub(Duration::from_secs(25)).expect("a box up for 25 s")
}

fn ready_venue() -> IbkrTransportFake {
    let mut fake = IbkrTransportFake::new();
    fake.script(ScriptedInbound::NextValidId(101));
    fake.script(ScriptedInbound::AccountsReady);
    fake
}

/// A venue where the first order written executes `shares` at once. With `commission` its fill joins and the order ends
/// if that was all of it; without, the fill waits for a commission that does not come.
fn first_order_executes(shares: f64, commission: bool) -> IbkrTransportFake {
    let mut fake = ready_venue();
    let mut placed = 0;
    fake.on_place_order(move |order_id| {
        placed += 1;
        if placed > 1 {
            return Vec::new();
        }
        let exec_id = "e1".to_string();
        let mut frames = vec![ScriptedInbound::ExecutionDetails {
            order_id,
            exec_id: exec_id.clone(),
            shares,
            price: 190.0,
        }];
        if commission {
            frames.push(ScriptedInbound::Commission { exec_id, commission: 1.0 });
        }
        frames
    });
    fake
}

/// Run the commands through the real loop until each is placed or refused, and hand back every event it sent.
fn drive(fake: &IbkrTransportFake, orders: Vec<(OrderRequest, Instant)>) -> Vec<Event> {
    drive_with(fake, orders, TIMES, |_| ())
}

/// [`drive`] with its own times, and `during` run once the loop has its orders. A shutdown comes last.
fn drive_with(
    fake: &IbkrTransportFake,
    orders: Vec<(OrderRequest, Instant)>,
    times: LoopTimes,
    during: impl FnOnce(&Sender<ExecutionClientActorCommand>),
) -> Vec<Event> {
    crate::testing::hold_operator_rows();
    let order_client_ids: Vec<String> =
        orders.iter().map(|(o, _)| o.order_client_id.clone()).collect();
    let (events, mut rx) = event_channel(64);
    let (command_tx, command_rx) = channel();
    for (order, joined) in orders {
        command_tx.send(ExecutionClientActorCommand::Submit(Box::new(order), joined)).unwrap();
    }
    let transport = Box::new(fake.clone());
    let worker = std::thread::spawn(move || {
        run_loop_with(&command_rx, events, transport, Taught::default(), times);
    });
    during(&command_tx);
    let mut out = Vec::new();
    let mut collect = |out: &mut Vec<Event>| {
        while let Ok(ingest) = rx.try_recv() {
            if let Ingest::Event(event) = ingest {
                out.push(event);
            }
        }
    };
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        collect(&mut out);
        let specs = fake.place_specs();
        let placed = specs.lock().unwrap();
        let decided = |order_client_id: &String| {
            refusal_of(&out, order_client_id).is_some()
                || placed.iter().any(|s| s.order_ref == *order_client_id)
        };
        if order_client_ids.iter().all(decided) {
            break;
        }
        drop(placed);
        assert!(Instant::now() < deadline, "nothing decided {order_client_ids:?}: {out:?}");
        std::thread::sleep(Duration::from_millis(1));
    }
    // Refused when `during` already shut the loop down.
    let _ = command_tx.send(ExecutionClientActorCommand::Shutdown);
    worker.join().unwrap();
    collect(&mut out);
    out
}

/// Wait for `what`, which the running loop makes true.
fn until(what: impl Fn() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(20);
    while !what() {
        assert!(Instant::now() < deadline, "it never happened");
        std::thread::sleep(Duration::from_millis(1));
    }
}

fn refusal_of<'a>(events: &'a [Event], order_client_id: &str) -> Option<&'a str> {
    events.iter().find_map(|event| match event {
        Event::OrderRejected(r) if r.order_client_id == order_client_id => Some(r.reason.as_ref()),
        _ => None,
    })
}

#[test]
fn a_late_close_on_a_position_that_has_gone_is_refused_and_nothing_is_placed() {
    for held in [0.0, -5.0] {
        let fake = ready_venue();
        fake.set_held_position(Ok(held));
        let events = drive(&fake, vec![(close("late", -1, 100.0), waited())]);
        assert!(fake.place_calls().lock().unwrap().is_empty(), "held {held}: it would open");
        let why = refusal_of(&events, "late").expect("refused, in words");
        assert!(why.contains("NOT sent"), "{why}");
        let refusal = events.iter().find_map(|event| match event {
            Event::OrderRejected(r) if r.order_client_id == "late" => r.refusal_id.as_deref(),
            _ => None,
        });
        assert_eq!(refusal, Some(vike_model::OrderRefusalId::NotSent.word()));
    }
}

/// Cut to what still faces it, and the cut size is what completes it: its last fill is final, so the order ends FILLED.
#[test]
fn a_late_close_larger_than_what_is_left_is_cut_and_completes_at_the_cut_size() {
    crate::testing::hold_operator_rows();
    let mut fake = ready_venue();
    fake.set_held_position(Ok(40.0));
    fake.on_place_order(|order_id| {
        vec![
            ScriptedInbound::OrderStatus {
                order_id,
                order_ref: "late".into(),
                status: "Submitted".into(),
            },
            ScriptedInbound::ExecutionDetails {
                order_id,
                exec_id: "e1".into(),
                shares: 40.0,
                price: 190.0,
            },
            ScriptedInbound::Commission { exec_id: "e1".into(), commission: 1.0 },
        ]
    });
    // The loop reads one inbound per pass and stops at Shutdown, so the fill needs passes behind the close.
    let (events, mut rx) = event_channel(64);
    let (command_tx, command_rx) = channel();
    command_tx
        .send(ExecutionClientActorCommand::Submit(Box::new(close("late", -1, 100.0)), waited()))
        .unwrap();
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
    let mut filled = false;
    let deadline = Instant::now() + Duration::from_secs(5);
    while !filled && Instant::now() < deadline {
        match rx.try_recv() {
            Ok(Ingest::Event(Event::OrderFilled(f))) => filled = f.order_client_id == "late",
            Ok(Ingest::Event(Event::OrderPartiallyFilled(f))) => {
                panic!("the cut size completes it: {f:?}")
            }
            Ok(_) => {}
            Err(_) => std::thread::sleep(Duration::from_millis(5)),
        }
    }
    command_tx.send(ExecutionClientActorCommand::Shutdown).unwrap();
    worker.join().unwrap();
    assert!(filled, "the fill of the cut size ends the order");
    let specs = fake.place_specs();
    let specs = specs.lock().unwrap();
    assert_eq!(specs.len(), 1);
    assert_eq!(specs[0].total_qty, 40.0, "cut to what still faces it");
    assert_eq!(specs[0].action, "SELL");
}

/// **The second close, behind the first**: the core rejected the first at its backstop and no longer counts it as cover, so
/// another comes, young. A write to IB returns before the venue has seen the order, so the position still reads whole, and
/// sending it would flip the position: what is on its way is taken off. It waits to see how the first ends, and is refused
/// at the give-up. Either side.
#[test]
fn a_close_behind_one_still_on_its_way_is_refused() {
    for (side, held) in [(-1, 100.0), (1, -100.0)] {
        let fake = ready_venue();
        fake.set_held_position(Ok(held));
        let events = drive(
            &fake,
            vec![
                (close("first", side, 100.0), waited()),
                (close("second", side, 100.0), Instant::now()),
            ],
        );
        assert_eq!(fake.place_calls().lock().unwrap().len(), 1, "{side}: only the first");
        assert!(*fake.held_reads().lock().unwrap() >= 2, "the young one reads too");
        let why = refusal_of(&events, "second").expect("refused");
        assert!(why.contains("already on its way"), "{why}");
        assert!(refusal_of(&events, "first").is_none());
    }
}

/// A close the venue refused is no longer on its way, so the one behind it goes: the position is still open.
#[test]
fn a_close_behind_a_refused_one_goes() {
    let mut fake = ready_venue();
    fake.set_held_position(Ok(100.0));
    let mut placed = 0;
    fake.on_place_order(move |order_id| {
        placed += 1;
        if placed > 1 {
            return Vec::new();
        }
        vec![ScriptedInbound::Error { code: 201, order_id, msg: "refused".into() }]
    });
    let events = drive(
        &fake,
        vec![(close("first", -1, 100.0), waited()), (close("second", -1, 100.0), Instant::now())],
    );
    assert_eq!(fake.place_calls().lock().unwrap().len(), 2, "{events:?}");
    assert!(refusal_of(&events, "second").is_none());
    assert_eq!(fake.place_specs().lock().unwrap()[1].total_qty, 100.0);
}

/// A close behind one the venue has not answered waits to see how that ends: refused by the venue, the one behind goes. It
/// used to be refused at once, and the position stayed open with no close working.
#[test]
fn a_close_waiting_on_the_one_ahead_goes_when_the_venue_refuses_that_one() {
    let fake = ready_venue();
    fake.set_held_position(Ok(100.0));
    let (reads, venue, placed) = (fake.held_reads(), fake.clone(), fake.place_calls());
    let events = drive_with(
        &fake,
        vec![(close("a", -1, 100.0), waited()), (close("b", -1, 100.0), Instant::now())],
        NO_GIVING_UP,
        |_| {
            until(|| *reads.lock().unwrap() >= 2);
            let order_id = placed.lock().unwrap()[0];
            venue.inject(ScriptedInbound::Error { code: 201, order_id, msg: "refused".into() });
        },
    );
    assert!(refusal_of(&events, "b").is_none(), "{events:?}");
    assert_eq!(fake.place_specs().lock().unwrap()[1].total_qty, 100.0);
}

/// **A close that cannot be sized is refused only after the retries** (the doc of [`size_late_close`] says why it differs from
/// the other venues), and the words say the position is still open and what to do. The memory of a late close keeps the
/// ones behind it reading, so they refuse too.
#[test]
fn a_read_that_keeps_failing_refuses_the_close_after_the_retries_in_words() {
    let fake = ready_venue();
    fake.set_held_position(Err("no answer within 5 s".to_string()));
    let events = drive(
        &fake,
        vec![(close("late", -1, 100.0), waited()), (close("behind", -1, 100.0), Instant::now())],
    );
    assert!(fake.place_calls().lock().unwrap().is_empty());
    assert!(*fake.held_reads().lock().unwrap() > 1, "it was asked again");
    for order_client_id in ["late", "behind"] {
        let why = refusal_of(&events, order_client_id).expect("refused");
        for needle in ["NOT sent", "STILL OPEN", "TWS", SYMBOL] {
            assert!(why.contains(needle), "`{needle}` is missing from: {why}");
        }
        let plain = why.split_whitespace().collect::<Vec<_>>().join(" ");
        assert_eq!(plain, why, "one plain line, single-spaced");
    }
    let why = refusal_of(&events, "late").expect("refused");
    assert!(why.contains("no answer within 5 s"), "the read's own words: {why}");
}

/// **The gateway answers the second time, and the close goes**, sized by that answer. It used to be refused at the first
/// failed read with no further attempt: a plain stop is spent by then and `close-all` gets no second try.
#[test]
fn a_read_that_fails_and_then_answers_lets_the_close_go() {
    let fake = ready_venue();
    fake.script_held_reads(&[Err("no answer within 5 s".to_string())]);
    fake.set_held_position(Ok(40.0));
    let events =
        drive_with(&fake, vec![(close("late", -1, 100.0), waited())], NO_GIVING_UP, |_| ());
    assert!(refusal_of(&events, "late").is_none(), "{events:?}");
    assert_eq!(fake.place_specs().lock().unwrap()[0].total_qty, 40.0, "cut by the answer");
    assert_eq!(*fake.held_reads().lock().unwrap(), 2);
}

/// **The wait does not hold the loop**: a cancel behind a waiting close goes out while it waits. At the shutdown the close
/// is refused in words, not lost.
#[test]
fn a_close_waiting_for_a_read_does_not_hold_up_a_cancel_and_a_shutdown_refuses_it() {
    let fake = ready_venue();
    fake.set_held_position(Err("no answer within 5 s".to_string()));
    let (reads, cancels) = (fake.held_reads(), fake.cancel_calls());
    let events = drive_with(
        &fake,
        vec![(close("young", -1, 1.0), Instant::now()), (close("late", -1, 100.0), waited())],
        NO_GIVING_UP,
        |commands| {
            until(|| *reads.lock().unwrap() > 0);
            commands
                .send(ExecutionClientActorCommand::Cancel {
                    order_client_id: "young".into(),
                    intent: Default::default(),
                })
                .unwrap();
            until(|| !cancels.lock().unwrap().is_empty());
            commands.send(ExecutionClientActorCommand::Shutdown).unwrap();
        },
    );
    assert_eq!(fake.place_specs().lock().unwrap().len(), 1, "only the young one: {events:?}");
    let why = refusal_of(&events, "late").expect("refused at the shutdown, not lost");
    for needle in ["NOT sent", "STILL OPEN", "shutting down", SYMBOL] {
        assert!(why.contains(needle), "`{needle}` is missing from: {why}");
    }
}

/// A stream that dies ends the wait at once: nothing can answer any more, and the words name the death.
#[test]
fn a_stream_that_dies_refuses_the_waiting_close_at_once() {
    let fake = ready_venue();
    fake.set_held_position(Err("no answer within 5 s".to_string()));
    let (reads, venue) = (fake.held_reads(), fake.clone());
    let events =
        drive_with(&fake, vec![(close("late", -1, 100.0), waited())], NO_GIVING_UP, |_| {
            until(|| *reads.lock().unwrap() > 0);
            venue.inject(ScriptedInbound::StreamDead { reason: "the socket died".into() });
        });
    assert!(fake.place_calls().lock().unwrap().is_empty());
    let why = refusal_of(&events, "late").expect("refused");
    assert!(why.contains("the socket died") && why.contains("NOT sent"), "{why}");
}

/// A cancel of a close that has not gone ends it there: unsent, in words, and it cannot go out behind the caller's back.
#[test]
fn a_cancel_of_a_waiting_close_ends_it_unsent() {
    let fake = ready_venue();
    fake.set_held_position(Err("no answer within 5 s".to_string()));
    let reads = fake.held_reads();
    let events =
        drive_with(&fake, vec![(close("late", -1, 100.0), waited())], NO_GIVING_UP, |commands| {
            until(|| *reads.lock().unwrap() > 0);
            commands
                .send(ExecutionClientActorCommand::Cancel {
                    order_client_id: "late".into(),
                    intent: Default::default(),
                })
                .unwrap();
        });
    assert!(fake.place_calls().lock().unwrap().is_empty());
    assert!(refusal_of(&events, "late").expect("refused").contains("NOT sent"));
    let advisory = events.iter().any(|e| matches!(e, Event::OrderCancelRejected(_)));
    assert!(!advisory, "the cancel did what it asked: {events:?}");
}

/// A gateway that says it has no link, and a connection learning the venue again, could not take the order a read would
/// size (a fresh submit is refused for both): no read, no order, and the refusal at the end says what it waited for.
#[test]
fn a_waiting_close_is_neither_read_nor_sent_while_the_connection_could_not_take_it() {
    crate::testing::hold_operator_rows();
    for (no_link, words) in [(true, "no link"), (false, "learning")] {
        let mut fake = ready_venue();
        fake.set_held_position(Ok(100.0));
        let mut mapper = EventMapper::new(OrderVenueIdAllocator::default());
        if no_link {
            fake.set_server_link_broken(true);
        } else {
            mapper.begin_learning();
        }
        let times =
            LoopTimes { close_retry: Duration::ZERO, close_retry_for: Duration::ZERO, ..TIMES };
        let (events, mut rx) = event_channel(8);
        let mut parked = ParkedCloses::default();
        let contract = contract_of_user_name(SYMBOL).expect("one contract");
        parked.park(Box::new(close("late", -1, 100.0)), contract, "no answer".into(), &times);
        let mut closes_out = ClosesOut::default();
        retry_parked(&mut parked, &mut mapper, &mut fake, &events, &mut closes_out, &times);
        assert_eq!(*fake.held_reads().lock().unwrap(), 0, "{words}: nothing read");
        assert!(fake.place_calls().lock().unwrap().is_empty(), "{words}: nothing sent");
        assert!(parked.list.is_empty(), "{words}: it ended");
        let Ok(Ingest::Event(Event::OrderRejected(refused))) = rx.try_recv() else {
            panic!("{words}: refused");
        };
        assert!(refused.reason.contains(words) && refused.reason.contains("STILL OPEN"));
    }
}

/// **A pending commission does not refuse a real close**: long 20, close A for 10 executed and the row already shows 10, but
/// A's commission has not come. B's 10 are real, and it used to be refused with "holds nothing", leaving 10 open.
#[test]
fn a_pending_commission_does_not_refuse_the_close_behind_it() {
    let fake = first_order_executes(10.0, false);
    fake.set_held_position(Ok(10.0));
    let events = drive(
        &fake,
        vec![(close("a", -1, 10.0), Instant::now()), (close("b", -1, 10.0), waited())],
    );
    assert!(refusal_of(&events, "b").is_none(), "{events:?}");
    let specs = fake.place_specs();
    let specs = specs.lock().unwrap();
    assert_eq!(specs.len(), 2);
    assert_eq!(specs[1].total_qty, 10.0);
}

/// The partial version: long 25, A for 10 executed, the row shows 15. B for 15 used to be cut to 5, leaving 10 open.
#[test]
fn a_pending_commission_does_not_cut_the_close_behind_it() {
    let fake = first_order_executes(10.0, false);
    fake.set_held_position(Ok(15.0));
    let events = drive(
        &fake,
        vec![(close("a", -1, 10.0), Instant::now()), (close("b", -1, 15.0), waited())],
    );
    assert!(refusal_of(&events, "b").is_none(), "{events:?}");
    assert_eq!(fake.place_specs().lock().unwrap()[1].total_qty, 15.0);
}

/// **A row that lags never flips**, whether the commission joined and the order ended or not: long 20, A for all of it
/// executed, the row still says 20. Taken at its word B would sell 20 more and open short 20 (with the order ended it did:
/// a fill that joined used to count as in the row). B waits, and is refused when the row shows the position gone.
#[test]
fn a_row_that_lags_an_execution_never_flips_the_close_behind_it() {
    for commission in [true, false] {
        let fake = first_order_executes(20.0, commission);
        fake.script_held_reads(&[Ok(20.0)]);
        fake.set_held_position(Ok(0.0));
        let (venue, reads, placed) = (fake.clone(), fake.held_reads(), fake.place_calls());
        let written = || placed.lock().unwrap().len();
        let events = drive_with(
            &fake,
            vec![(close("a", -1, 20.0), Instant::now())],
            LoopTimes { row_settle: Duration::from_secs(60), ..NO_GIVING_UP },
            |commands| {
                // Sent once A's frames have folded: the loop reads one inbound a pass.
                until(|| written() == 1 && venue.undelivered() == 0);
                let b = Box::new(close("b", -1, 20.0));
                commands.send(ExecutionClientActorCommand::Submit(b, waited())).unwrap();
                until(|| written() == 2 || *reads.lock().unwrap() >= 2);
            },
        );
        assert_eq!(fake.place_calls().lock().unwrap().len(), 1, "{commission}: {events:?}");
        let why = refusal_of(&events, "b").expect("refused");
        assert!(why.contains("NOT sent"), "{why}");
    }
}

/// While the row may not show A, what fits under either reading goes at once: long 20, A for 10 executed, row still 20. B
/// for 10 faces 10 whichever is true.
#[test]
fn a_close_that_fits_either_way_does_not_wait_for_the_row() {
    let fake = first_order_executes(10.0, false);
    fake.set_held_position(Ok(20.0));
    let events = drive_with(
        &fake,
        vec![(close("a", -1, 10.0), Instant::now()), (close("b", -1, 10.0), waited())],
        LoopTimes { row_settle: Duration::from_secs(60), ..NO_GIVING_UP },
        |_| (),
    );
    assert!(refusal_of(&events, "b").is_none(), "{events:?}");
    assert_eq!(fake.place_specs().lock().unwrap()[1].total_qty, 10.0);
}

/// Everything else asks nothing: a young close reads no position, and an order that is no close keeps its age check.
#[test]
fn a_young_close_reads_nothing_and_an_old_opening_order_is_still_refused_for_its_age() {
    let fake = ready_venue();
    fake.set_held_position(Ok(0.0));
    let opening = OrderRequest { reduce_only: false, ..close("old-open", 1, 1.0) };
    let events =
        drive(&fake, vec![(close("young", -1, 100.0), Instant::now()), (opening, waited())]);
    assert_eq!(*fake.held_reads().lock().unwrap(), 0);
    assert_eq!(fake.place_calls().lock().unwrap().len(), 1, "the young close goes");
    assert!(refusal_of(&events, "old-open").expect("refused").contains("NOT sent"));
}
