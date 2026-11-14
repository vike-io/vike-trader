//! `run_loop`'s command→event wiring over a mock `VenueRest` (NO network, NO real orders):
//! Submit→`submit_order` (forwarding its events), Cancel→`cancel_order` (failure → non-terminal
//! `OrderCancelRejected`), Modify→`modify_order` (forwarding a native amend's events; the
//! `VenueRest` default no-op forwards nothing), the gap-sentinel resync after an idle submit, and
//! the restart law's history floor. Venue-specific behaviour is tested in each venue crate.
use super::*;
use crate::execution_drive::drain_ready;
use crate::transport::VenueApiError;
use std::assert_matches;
use std::sync::Mutex;
use vike_execution::Ingest;
use vike_execution::event_channel;
use vike_model::events::{OrderAccepted, OrderModified, OrderSubmitted};

#[derive(Default)]
struct MockRest {
    submitted: Mutex<Vec<String>>,
    canceled: Mutex<Vec<String>>,
    modified: Mutex<Vec<(String, Option<f64>)>>,
    cancel_fails: bool,
    /// When true, `modify_order` returns a native `OrderModified` (the native-amend capability);
    /// when false it is the `VenueRest` default no-op (`[]`).
    native_modify: bool,
}

impl VenueRest for MockRest {
    fn submit_order(&self, request: &OrderRequest) -> Vec<Event> {
        self.submitted.lock().unwrap().push(request.order_client_id.clone());
        vec![
            Event::OrderSubmitted(OrderSubmitted {
                order_client_id: request.order_client_id.clone(),
                timestamp: 0,
            }),
            Event::OrderAccepted(OrderAccepted {
                order_client_id: request.order_client_id.clone(),
                venue_order_id: Some("v-1".to_string().into()),
                timestamp: 0,
            }),
        ]
    }
    fn cancel_order(&self, order_client_id: &str) -> Result<OrderCancelAck, VenueApiError> {
        self.canceled.lock().unwrap().push(order_client_id.to_string());
        if self.cancel_fails {
            Err(VenueApiError { code: 1, msg: "boom".to_string() })
        } else if order_client_id.starts_with("gone") {
            Ok(OrderCancelAck::AlreadyGone)
        } else {
            Ok(OrderCancelAck::Canceled)
        }
    }
    fn modify_order(
        &self,
        order: &OrderRequest,
        new_qty: Option<f64>,
        new_price: Option<f64>,
    ) -> Vec<Event> {
        self.modified.lock().unwrap().push((order.order_client_id.clone(), new_price));
        if self.native_modify {
            vec![Event::OrderModified(OrderModified {
                order_client_id: order.order_client_id.clone(),
                venue_order_id: Some("v-1".to_string().into()),
                new_qty,
                new_price,
                new_trigger_price: None,
                timestamp: 0,
            })]
        } else {
            Vec::new()
        }
    }
}

fn req(order_client_id: &str) -> OrderRequest {
    crate::execution_drive::limit("test", order_client_id, "BTCUSDT", 0.01, 50000.0)
}

/// Poll the ingest lane every 2 ms, collecting events, until `done(&seen)` holds or 5 s run out,
/// and return everything seen. It replaces a fixed sleep that had to outlast another thread's
/// work: the caller's assertions run on the returned events unchanged, so a missing effect still
/// fails them (with their own message) instead of passing on a short wait.
fn drain_until(
    rx: &mut tokio::sync::mpsc::Receiver<Ingest>,
    done: impl Fn(&[Event]) -> bool,
) -> Vec<Event> {
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    let mut seen = Vec::new();
    loop {
        seen.extend(drain_ready(|| rx.try_recv().ok()));
        if done(&seen) || std::time::Instant::now() >= deadline {
            return seen;
        }
        std::thread::sleep(Duration::from_millis(2));
    }
}

/// **An order that waited past its time on the queue is refused by name and never reaches the venue**,
/// a younger one is sent, and a cancel just as old still goes out: a late cancel is safe and a dropped
/// one is not. The old loop sent whatever it dequeued.
#[test]
fn an_order_past_its_wait_is_refused_and_a_cancel_of_the_same_age_is_not() {
    let (sender, mut ingest) = event_channel(64);
    let rest = MockRest::default();
    let (tx, command_rx) = channel();
    let old = Instant::now() - MAX_SUBMIT_WAIT - Duration::from_secs(1);
    let young = Instant::now() - (MAX_SUBMIT_WAIT - Duration::from_secs(1));
    tx.send(ExecutionClientActorCommand::Submit(Box::new(req("old")), old)).unwrap();
    tx.send(ExecutionClientActorCommand::Submit(Box::new(req("young")), young)).unwrap();
    tx.send(ExecutionClientActorCommand::Cancel {
        order_client_id: "resting".to_string(),
        intent: OrderCancelIntent::Unspecified,
    })
    .unwrap();
    tx.send(ExecutionClientActorCommand::Shutdown).unwrap();
    run_loop(&rest, &sender, command_rx, Vec::<Event>::new, Duration::from_secs(3600));

    assert_eq!(*rest.submitted.lock().unwrap(), ["young"], "the old order reached no venue");
    assert_eq!(*rest.canceled.lock().unwrap(), ["resting"], "a cancel has no age");
    let events = drain_ready(|| ingest.try_recv().ok());
    match &events[..2] {
        [Event::OrderSubmitted(s), Event::OrderRejected(r)] => {
            assert_eq!((s.order_client_id.as_str(), r.order_client_id.as_str()), ("old", "old"));
            assert!(r.reason.contains("NOT sent") && r.reason.contains("20 s"), "{}", r.reason);
        }
        other => panic!("expected [Submitted, Rejected] for the old order, got {other:?}"),
    }
}

/// **A close past its wait still goes**: a `reduce_only` MARKET is what a flatten leg, a margin call and a fired stop
/// are on every venue, and refused here it leaves the position open with nothing asking again. The age still binds
/// everything that can rest or open: a reduce-only limit, a reduce-only market with a trigger, a plain market.
#[test]
fn a_close_past_its_wait_is_sent_and_nothing_else_is() {
    use vike_model::OrderType::{Limit, Market};
    let (sender, mut ingest) = event_channel(64);
    let rest = MockRest::default();
    let (tx, command_rx) = channel();
    let old = Instant::now() - MAX_SUBMIT_WAIT - Duration::from_secs(1);
    let shaped = |order_client_id: &str, kind, reduce_only, trigger_price| {
        Box::new(OrderRequest {
            order_type: kind,
            reduce_only,
            trigger_price,
            price: None,
            ..req(order_client_id)
        })
    };
    tx.send(ExecutionClientActorCommand::Submit(shaped("leg", Market, true, None), old)).unwrap();
    tx.send(ExecutionClientActorCommand::Submit(shaped("resting", Limit, true, None), old))
        .unwrap();
    tx.send(ExecutionClientActorCommand::Submit(shaped("triggered", Market, true, Some(1.0)), old))
        .unwrap();
    tx.send(ExecutionClientActorCommand::Submit(shaped("opening", Market, false, None), old))
        .unwrap();
    tx.send(ExecutionClientActorCommand::Shutdown).unwrap();
    run_loop(&rest, &sender, command_rx, Vec::<Event>::new, Duration::from_secs(3600));

    assert_eq!(*rest.submitted.lock().unwrap(), ["leg"]);
    let refused: Vec<String> = drain_ready(|| ingest.try_recv().ok())
        .iter()
        .filter_map(|e| match e {
            Event::OrderRejected(r) => Some(r.order_client_id.clone()),
            _ => None,
        })
        .collect();
    assert_eq!(refused, ["resting", "triggered", "opening"]);
}

/// A late close on a venue that ignores the flag: one that read the position goes at what still faces it, nothing
/// facing it is refused in words, and a failed read is no evidence, so it goes. The closes behind it ask too, and
/// everything else asks nothing.
#[test]
fn a_late_close_is_cut_to_the_position_it_reads_and_the_closes_behind_it_ask_too() {
    let sell = OrderRequest {
        side: -1,
        qty: 10.0,
        order_type: vike_model::OrderType::Market,
        reduce_only: true,
        price: None,
        ..req("c")
    };
    assert_eq!(late_close_size(&sell, Ok(10.0)), Ok(None), "covered: as it is");
    assert_eq!(late_close_size(&sell, Ok(25.0)), Ok(None));
    assert_eq!(late_close_size(&sell, Ok(4.0)), Ok(Some(4.0)), "cut to what faces it");
    let why = late_close_size(&sell, Ok(0.0)).expect_err("flat");
    assert!(why.contains("NOT sent") && why.contains("flat"), "{why}");
    assert!(late_close_size(&sell, Ok(-3.0)).is_err(), "a short does not face a sell");
    assert_eq!(late_close_size(&sell, Err("down".into())), Ok(None), "no evidence, it goes");
    let buy = OrderRequest { side: 1, ..sell.clone() };
    assert_eq!(late_close_size(&buy, Ok(-4.0)), Ok(Some(4.0)));
    assert!(late_close_size(&buy, Ok(4.0)).is_err());

    let mut late = ExecutionClientActorLateCloses::default();
    let (young, old) = (Instant::now(), Instant::now() - MAX_SUBMIT_WAIT - Duration::from_secs(1));
    assert!(!late.must_read(&sell, young), "an ordinary close reads nothing");
    let plain = OrderRequest { reduce_only: false, ..sell.clone() };
    assert!(!late.must_read(&plain, old), "not a close: its age is too_old_to_send's");
    assert!(late.must_read(&sell, old));
    assert!(late.must_read(&sell, young), "the close behind it, young, asks too");
    assert!(!late.must_read(&plain, young));
}

fn cancel(order_client_id: &str) -> ExecutionClientActorCommand {
    ExecutionClientActorCommand::Cancel {
        order_client_id: order_client_id.to_string(),
        intent: OrderCancelIntent::Unspecified,
    }
}

/// Stamped ahead of the loop's spawn, so the restart law's floor lets it through.
fn filled(order_client_id: &str) -> Event {
    let now = vike_model::now_ms() + 60_000;
    Event::OrderFilled(vike_model::events::OrderFilled {
        order_client_id: order_client_id.to_string(),
        fill: vike_model::events::FillEvent {
            trade_id: vike_model::events::TradeId::new("t-1").expect("not empty"),
            order_client_id: order_client_id.to_string(),
            venue: "test".into(),
            symbol: "BTCUSDT".into(),
            side: 1,
            last_qty: 0.01,
            last_price: 50000.0,
            commission: 0.0,
            commission_asset: "USDT".into(),
            liquidity_side: "taker".into(),
            timestamp: now,
            mark_price: None,
            position_side: vike_model::events::PositionSide::Both,
        },
        timestamp: now,
    })
}

/// **A cancel that loses to a fill ends FILLED.** "Already gone" (`-2011`, `110001`, `51402`) beat the
/// stream's fill and used to be a terminal `OrderCanceled` at once; the order table has no
/// `Canceled -> Filled` edge, so a filled order read CANCELED for good with the position right. Now it is
/// a non-terminal answer, and the gap-sentinel's replay says how the order ended.
#[test]
fn already_gone_is_not_terminal_and_the_replay_ends_a_filled_order() {
    let (sender, mut ingest) = event_channel(64);
    let rest = MockRest::default();
    let (tx, command_rx) = channel();
    tx.send(cancel("gone-1")).unwrap();
    let handle = std::thread::spawn(move || {
        run_loop(&rest, &sender, command_rx, || vec![filled("gone-1")], Duration::from_millis(30));
    });
    let seen = drain_until(&mut ingest, |s| s.iter().any(|e| matches!(e, Event::OrderFilled(_))));
    tx.send(ExecutionClientActorCommand::Shutdown).unwrap();
    handle.join().unwrap();
    let seen: Vec<&Event> = seen.iter().collect();
    match seen.as_slice() {
        [Event::OrderCancelRejected(r), Event::OrderFilled(f), ..] => {
            assert_eq!(r.reason.as_str(), ALREADY_GONE_REASON);
            assert_eq!(f.order_client_id, "gone-1");
        }
        other => panic!("expected the advisory, then the replay's own terminal, got {other:?}"),
    }
    assert!(
        !seen.iter().any(|e| matches!(e, Event::OrderCanceled(_))),
        "the replay settled it, so no cancel is made up: {seen:?}"
    );
}

/// **With no word from the replay (a row past its window, a failed read) the cancel ends as it always
/// did, and no order waits for ever**, once, after its time: never a stream of them, and a busy thread
/// does not push it back.
#[test]
fn already_gone_the_replay_does_not_explain_ends_canceled_after_its_time() {
    let (sender, mut ingest) = event_channel(64);
    let rest = MockRest::default();
    let (tx, command_rx) = channel();
    let sentinel = Duration::from_millis(60);
    tx.send(cancel("gone-1")).unwrap();
    let handle = std::thread::spawn(move || {
        run_loop(&rest, &sender, command_rx, Vec::<Event>::new, sentinel);
    });
    // Commands more often than the sentinel: they used to push it back for as long as they came.
    let started = std::time::Instant::now();
    let mut seen = Vec::new();
    while !seen
        .iter()
        .any(|e| matches!(e, Event::OrderCanceled(c) if c.order_client_id == "gone-1"))
        && started.elapsed() < Duration::from_secs(5)
    {
        tx.send(cancel("resting")).unwrap();
        std::thread::sleep(Duration::from_millis(10));
        seen.extend(drain_ready(|| ingest.try_recv().ok()));
    }
    tx.send(ExecutionClientActorCommand::Shutdown).unwrap();
    handle.join().unwrap();
    seen.extend(drain_ready(|| ingest.try_recv().ok()));
    assert!(started.elapsed() >= sentinel, "it had its dwell");
    let of_it: Vec<&Event> = seen
        .iter()
        .filter(|e| match e {
            Event::OrderCancelRejected(r) => r.order_client_id == "gone-1",
            Event::OrderCanceled(c) => c.order_client_id == "gone-1",
            _ => false,
        })
        .collect();
    assert_matches!(of_it.as_slice(), [Event::OrderCancelRejected(_), Event::OrderCanceled(_)]);
}

/// A second "already gone" for one order waits as one, and the loop's end settles what is left, so a
/// stop leaves no order waiting (and the order-client-id-map reproductions still read the old terminal at the end).
#[test]
fn the_loops_end_settles_a_cancel_still_waiting() {
    let (sender, mut ingest) = event_channel(64);
    let rest = MockRest::default();
    let (tx, command_rx) = channel();
    tx.send(cancel("gone-1")).unwrap();
    tx.send(cancel("gone-1")).unwrap();
    tx.send(ExecutionClientActorCommand::Shutdown).unwrap();
    run_loop(&rest, &sender, command_rx, Vec::<Event>::new, Duration::from_secs(3600));
    let seen = drain_ready(|| ingest.try_recv().ok());
    assert_matches!(
        seen.as_slice(),
        [Event::OrderCancelRejected(_), Event::OrderCancelRejected(_), Event::OrderCanceled(_)]
    );
}

/// `ExecutionClientActor::submit` stamps the order when it joins the queue: an order behind a stalled thread comes
/// out carrying the whole wait, whatever the thread was doing meanwhile.
#[test]
fn the_wait_is_counted_from_the_submit_not_from_the_dequeue() {
    let (sender, _ingest) = event_channel(8);
    let (release, stalled) = channel::<()>();
    let (report, waited) = channel();
    let mut actor = ExecutionClientActor::spawn("stall-test", sender, move |rx| {
        stalled.recv().unwrap(); // the blocking call the order queues behind
        if let Ok(ExecutionClientActorCommand::Submit(_, queued_at)) = rx.recv() {
            report.send(queued_at.elapsed()).unwrap();
        }
    });
    actor.submit(&req("c1"));
    std::thread::sleep(Duration::from_millis(50));
    release.send(()).unwrap();
    assert!(waited.recv().unwrap() >= Duration::from_millis(50));
    actor.detach();
}

#[test]
fn submit_forwards_events_and_cancel_confirms() {
    let (sender, mut ingest) = event_channel(64);
    let rest = MockRest::default();
    let (tx, command_rx) = channel();
    tx.send(ExecutionClientActorCommand::Submit(Box::new(req("c1")), Instant::now())).unwrap();
    tx.send(ExecutionClientActorCommand::Cancel {
        order_client_id: "c1".to_string(),
        intent: OrderCancelIntent::Unspecified,
    })
    .unwrap();
    tx.send(ExecutionClientActorCommand::Shutdown).unwrap();
    // long sentinel + Shutdown already queued → the gap-sentinel never fires here
    run_loop(&rest, &sender, command_rx, Vec::<Event>::new, Duration::from_secs(3600));

    assert_eq!(*rest.submitted.lock().unwrap(), vec!["c1".to_string()]);
    assert_eq!(*rest.canceled.lock().unwrap(), vec!["c1".to_string()]);
    let events = drain_ready(|| ingest.try_recv().ok());
    let kinds: Vec<&str> = events
        .iter()
        .map(|e| match e {
            Event::OrderSubmitted(_) => "Submitted",
            Event::OrderAccepted(_) => "Accepted",
            Event::OrderCanceled(_) => "Canceled",
            Event::OrderCancelRejected(_) => "CancelRejected",
            _ => "other",
        })
        .collect();
    assert_eq!(kinds, vec!["Submitted", "Accepted", "Canceled"]);
}

#[test]
fn failed_cancel_surfaces_as_cancel_rejected() {
    let (sender, mut ingest) = event_channel(64);
    let rest = MockRest { cancel_fails: true, ..Default::default() };
    let (tx, command_rx) = channel();
    tx.send(ExecutionClientActorCommand::Cancel {
        order_client_id: "c9".to_string(),
        intent: OrderCancelIntent::Unspecified,
    })
    .unwrap();
    tx.send(ExecutionClientActorCommand::Shutdown).unwrap();
    run_loop(&rest, &sender, command_rx, Vec::<Event>::new, Duration::from_secs(3600));

    let events = drain_ready(|| ingest.try_recv().ok());
    match events.as_slice() {
        [Event::OrderCancelRejected(e)] => {
            assert_eq!(e.order_client_id, "c9");
            assert_eq!(e.reason, "boom");
        }
        other => panic!("expected one OrderCancelRejected, got {other:?}"),
    }
}

/// Modify routing, BOTH arms: a native amend's `[OrderModified]` is forwarded to the ingest;
/// the `VenueRest` default no-op forwards nothing (the resting order keeps its terms). The
/// venue-reality ties (`capabilities_for(venue).supports_modify`) stay in each venue's own tests.
#[test]
fn modify_routes_to_modify_order_and_forwards_events() {
    // native amend: `[OrderModified]` reaches the ingest
    let (sender, mut ingest) = event_channel(64);
    let rest = MockRest { native_modify: true, ..Default::default() };
    let (tx, command_rx) = channel();
    tx.send(ExecutionClientActorCommand::Modify {
        order: Box::new(req("c1")),
        new_qty: None,
        new_price: Some(51_000.0),
        new_trigger_price: None,
    })
    .unwrap();
    tx.send(ExecutionClientActorCommand::Shutdown).unwrap();
    run_loop(&rest, &sender, command_rx, Vec::<Event>::new, Duration::from_secs(3600));

    assert_eq!(*rest.modified.lock().unwrap(), vec![("c1".to_string(), Some(51_000.0))]);
    let events = drain_ready(|| ingest.try_recv().ok());
    match events.as_slice() {
        [Event::OrderModified(e)] => {
            assert_eq!(e.order_client_id, "c1");
            assert_eq!(e.new_price, Some(51_000.0));
        }
        other => panic!("expected one OrderModified, got {other:?}"),
    }

    // default no-op: `modify_order` is still invoked, but nothing reaches the ingest
    let (sender, mut ingest) = event_channel(64);
    let rest = MockRest::default();
    let (tx, command_rx) = channel();
    tx.send(ExecutionClientActorCommand::Modify {
        order: Box::new(req("c2")),
        new_qty: None,
        new_price: Some(52_000.0),
        new_trigger_price: None,
    })
    .unwrap();
    tx.send(ExecutionClientActorCommand::Shutdown).unwrap();
    run_loop(&rest, &sender, command_rx, Vec::<Event>::new, Duration::from_secs(3600));

    assert_eq!(*rest.modified.lock().unwrap(), vec![("c2".to_string(), Some(52_000.0))]);
    assert!(
        drain_ready(|| ingest.try_recv().ok()).is_empty(),
        "the default no-op must forward no events"
    );
}

/// A TRIGGER amend: an actor that never declared it refuses in the trait's words before the channel; a
/// declared one queues it with the trigger; and `run_loop` hands it to `VenueRest::modify_trigger`, whose
/// default refuses it in words, never `modify_order` with the limit alone.
#[test]
fn a_trigger_amend_is_queued_only_when_declared_and_refused_by_the_rest_default() {
    use vike_execution::execution_engine::TRIGGER_AMEND_UNSUPPORTED_REASON;
    let (sender, _ingest) = event_channel(8);
    let mut undeclared =
        ExecutionClientActor::spawn("trigger-undeclared", sender.clone(), |_rx| {});
    assert_eq!(
        undeclared.modify_trigger(&req("c1"), None, None, 91.0),
        Err(TRIGGER_AMEND_UNSUPPORTED_REASON)
    );
    undeclared.detach();
    let (report, got) = channel();
    let mut declared = ExecutionClientActor::spawn("trigger-declared", sender, move |rx| {
        if let Ok(ExecutionClientActorCommand::Modify { new_price, new_trigger_price, .. }) =
            rx.recv()
        {
            report.send((new_price, new_trigger_price)).unwrap();
        }
    })
    .with_trigger_amend();
    assert_eq!(declared.modify_trigger(&req("c1"), None, Some(89.0), 91.0), Ok(()));
    assert_eq!(got.recv().unwrap(), (Some(89.0), Some(91.0)));
    declared.detach();

    let (sender, mut ingest) = event_channel(64);
    let rest = MockRest { native_modify: true, ..Default::default() };
    let (tx, command_rx) = channel();
    tx.send(ExecutionClientActorCommand::Modify {
        order: Box::new(req("c3")),
        new_qty: None,
        new_price: Some(51_000.0),
        new_trigger_price: Some(50_500.0),
    })
    .unwrap();
    tx.send(ExecutionClientActorCommand::Shutdown).unwrap();
    run_loop(&rest, &sender, command_rx, Vec::<Event>::new, Duration::from_secs(3600));
    assert!(rest.modified.lock().unwrap().is_empty(), "the limit is not moved alone");
    match drain_ready(|| ingest.try_recv().ok()).as_slice() {
        [Event::OrderModifyRejected(r)] => {
            assert_eq!(r.order_client_id, "c3");
            assert!(r.reason.contains(TRIGGER_AMEND_UNSUPPORTED_REASON), "{}", r.reason);
        }
        other => panic!("expected one OrderModifyRejected, got {other:?}"),
    }
}

/// The gap-sentinel: after a Submit with NO follow-up command, `run_loop` fires the `resync`
/// closure `fill_sentinel` later and forwards its events — the recovery path for a fill the WS
/// lost. A normal WS delivery would just make this dedup'd downstream.
#[test]
fn sentinel_resyncs_after_idle_submit() {
    use std::sync::atomic::AtomicUsize;
    use vike_model::events::OrderCanceled;

    let (sender, mut ingest) = event_channel(64);
    let rest = MockRest::default();
    let (tx, command_rx) = channel();
    let calls = Arc::new(AtomicUsize::new(0));
    let resync = {
        let calls = calls.clone();
        move || {
            calls.fetch_add(1, Ordering::Relaxed);
            // a marker event proves the resync's output is forwarded to the ingest
            vec![Event::OrderCanceled(OrderCanceled {
                order_client_id: "resync-marker".to_string(),
                reason: String::new().into(),
                timestamp: 0,
            })]
        }
    };

    tx.send(ExecutionClientActorCommand::Submit(Box::new(req("c1")), Instant::now())).unwrap();
    // NB: no follow-up command — the sentinel must fire on its own after `fill_sentinel`.
    let h = std::thread::spawn(move || {
        run_loop(&rest, &sender, command_rx, resync, Duration::from_millis(60))
    });
    // Wait for the one-shot resync's marker to reach the ingest (it fires `fill_sentinel` after the
    // submit), not for a duration that has to outlast the actor thread's start on a loaded box.
    let events = drain_until(&mut ingest, |seen| {
        seen.iter()
            .any(|e| matches!(e, Event::OrderCanceled(c) if c.order_client_id == "resync-marker"))
    });
    tx.send(ExecutionClientActorCommand::Shutdown).unwrap();
    h.join().unwrap();

    assert!(calls.load(Ordering::Relaxed) >= 1, "sentinel must resync after an idle submit");
    assert!(
        events
            .iter()
            .any(|e| matches!(e, Event::OrderCanceled(c) if c.order_client_id == "resync-marker")),
        "resync events must reach the ingest: {events:?}"
    );
}

/// A client whose POST got no answer and whose re-query failed: `OrderSubmitted` alone, for every order.
struct NoWord;

impl VenueRest for NoWord {
    fn submit_order(&self, request: &OrderRequest) -> Vec<Event> {
        vec![Event::OrderSubmitted(OrderSubmitted {
            order_client_id: request.order_client_id.clone(),
            timestamp: 0,
        })]
    }
    fn cancel_order(&self, _: &str) -> Result<OrderCancelAck, VenueApiError> {
        Ok(OrderCancelAck::Canceled)
    }
}

/// Whatever the sentinel's replay said, in order.
fn replayed_after_a_submit<R: VenueRest + Sync>(rest: &R, order_client_id: &str) -> Vec<Event> {
    let (sender, mut ingest) = event_channel(64);
    let (tx, command_rx) = channel();
    tx.send(ExecutionClientActorCommand::Submit(Box::new(req(order_client_id)), Instant::now()))
        .unwrap();
    std::thread::scope(|scope| {
        let handle = scope.spawn(|| {
            let replay = || vec![filled("c1")];
            run_loop(rest, &sender, command_rx, replay, Duration::from_millis(20));
        });
        let seen =
            drain_until(&mut ingest, |s| s.iter().any(|e| matches!(e, Event::OrderFilled(_))));
        tx.send(ExecutionClientActorCommand::Shutdown).unwrap();
        handle.join().unwrap();
        seen
    })
}

/// **The sentinel's replay of a fill on an order with no answer brings its accept** (no venue's history mints one, and
/// the engine refuses a wrap on a `Submitted` order): behind a client that returns its answer, and behind a fence that
/// sent it itself. A client that answered gets none, and a second pass of the same fill brings none.
#[test]
fn the_sentinels_replay_of_a_silent_orders_fill_brings_the_accept_first() {
    use super::submit_fence::{SubmitFence, VenueRestOrderSubmitFence};
    let kinds = |seen: &[Event]| {
        seen.iter()
            .map(|e| match e {
                Event::OrderSubmitted(_) => "Submitted",
                Event::OrderAccepted(_) => "Accepted",
                Event::OrderFilled(_) => "OrderFilled",
                _ => "other",
            })
            .collect::<Vec<_>>()
    };

    let seen = replayed_after_a_submit(&NoWord, "c1");
    assert_eq!(kinds(&seen), ["Submitted", "Accepted", "OrderFilled"], "{seen:?}");

    let (fenced_lane, mut fenced_ingest) = event_channel(64);
    let fence = SubmitFence::new(fenced_lane);
    let inner = NoWord;
    let seen = replayed_after_a_submit(&VenueRestOrderSubmitFence::new(&inner, &fence), "c1");
    assert_eq!(kinds(&seen), ["Accepted", "OrderFilled"], "the sentinel's lane, behind the fence");
    let sent = drain_ready(|| fenced_ingest.try_recv().ok());
    assert_eq!(kinds(&sent), ["Submitted"], "the fence sent the order's own");

    let seen = replayed_after_a_submit(&MockRest::default(), "c1");
    assert_eq!(kinds(&seen), ["Submitted", "Accepted", "OrderFilled"], "its own answer, no other");
}

/// One replayed fill, stamped.
fn fill_at(order_client_id: &str, trade_id: &'static str, ts: i64) -> Event {
    let fill: vike_model::events::FillEvent = serde_json::from_value(serde_json::json!({
        "trade_id": trade_id, "order_client_id": order_client_id, "venue": "test", "symbol": "BTCUSDT",
        "side": 1, "last_qty": 1.0, "last_price": 50000.0, "commission": 0.1,
        "commission_asset": "USDT", "timestamp": ts
    }))
    .unwrap();
    Event::Fill(fill)
}

/// **The restart law** (`exec_actor`'s module doc): without a floor, the first gap-sentinel firing
/// after a restart folds a PREVIOUS session's fills through the non-idempotent
/// `Account::apply_fill`.
///
/// A scripted history closure returns one fill stamped an hour BEFORE this `run_loop`'s spawn
/// and one stamped an hour after. Only the second may reach the ingest — and the second is the
/// legitimate case this must not break (a mid-process WS reconnect's gap fills).
#[test]
fn history_replay_drops_fills_older_than_the_actor_spawn() {
    let (sender, mut ingest) = event_channel(64);
    let rest = MockRest::default();
    let (tx, command_rx) = channel();
    let now = vike_model::now_ms();
    let resync = move || {
        vec![
            // a PREVIOUS session's fill — retained by the venue, never placed by this process
            fill_at("c_prev_session", "e_old", now - 3_600_000),
            // ...and one from THIS process's life (the WS-gap fill the replay exists for)
            fill_at("c_this_session", "e_new", now + 3_600_000),
            // unstamped: never fabricated into a verdict, so it rides through
            Event::OrderCanceled(vike_model::events::OrderCanceled {
                order_client_id: "c_unstamped".to_string(),
                reason: String::new().into(),
                timestamp: 0,
            }),
        ]
    };

    tx.send(ExecutionClientActorCommand::Submit(Box::new(req("c1")), Instant::now())).unwrap();
    // no follow-up command — the one-shot sentinel fires on its own
    let h = std::thread::spawn(move || {
        run_loop(&rest, &sender, command_rx, resync, Duration::from_millis(60))
    });
    // The batch's LAST event is the unstamped marker: once it lands, every fill the floor lets
    // through has landed before it, so the assertions below see the whole filtered batch.
    let events = drain_until(&mut ingest, |seen| {
        seen.iter()
            .any(|e| matches!(e, Event::OrderCanceled(c) if c.order_client_id == "c_unstamped"))
    });
    tx.send(ExecutionClientActorCommand::Shutdown).unwrap();
    h.join().unwrap();

    let trade_ids: Vec<String> = events
        .iter()
        .filter_map(|e| match e {
            Event::Fill(f) => Some(f.trade_id.to_string()),
            _ => None,
        })
        .collect();
    assert!(
        !trade_ids.iter().any(|t| t == "e_old"),
        "a fill stamped BEFORE the actor spawned is a previous session's — it must never \
             reach the ingest, where apply_fill folds its fee and PnL again: {events:?}"
    );
    assert!(
        trade_ids.iter().any(|t| t == "e_new"),
        "a fill stamped after the spawn is the WS-gap recovery this lane exists for and must \
             still be delivered: {events:?}"
    );
    assert!(
        events
            .iter()
            .any(|e| matches!(e, Event::OrderCanceled(c) if c.order_client_id == "c_unstamped")),
        "an UNSTAMPED event carries no evidence of being historical and must ride through \
             (no timestamp is ever fabricated here): {events:?}"
    );
}

/// The floor's predicate, directly: only a POSITIVE stamp strictly below the floor is dropped.
/// The two boundaries are the ones a mutation would flip — `ts == 0` (unstamped) and
/// `ts == spawn_ms` (the instant itself) both survive.
#[test]
fn only_a_positive_stamp_strictly_below_the_floor_is_pre_spawn() {
    let floor = 1_000_000i64;
    assert!(is_pre_spawn(&fill_at("c", "e", floor - 1), floor));
    assert!(!is_pre_spawn(&fill_at("c", "e", floor), floor));
    assert!(!is_pre_spawn(&fill_at("c", "e", floor + 1), floor));
    assert!(!is_pre_spawn(&fill_at("c", "e", 0), floor), "unstamped is not historical");
    // and a floor of 0 (a clock that could not be read) can never drop anything
    assert!(!is_pre_spawn(&fill_at("c", "e", 1), 0));
}

/// ⚠ The core is gone (the ingest receiver is dropped) while the loop still has work: every event
/// it forwards is dropped. The first drop raises ONE `warn!`; the other five (three submits, two
/// events each) stay silent -- a line per message on the exec thread would be a flood.
#[test]
fn run_loop_warns_once_when_the_core_is_gone_not_per_dropped_event() {
    let (sender, ingest) = event_channel(64);
    drop(ingest);
    let rest = MockRest::default();
    let (tx, command_rx) = channel();
    for order_client_id in ["c1", "c2", "c3"] {
        tx.send(ExecutionClientActorCommand::Submit(
            Box::new(req(order_client_id)),
            Instant::now(),
        ))
        .unwrap();
    }
    tx.send(ExecutionClientActorCommand::Shutdown).unwrap();

    let ((), captured) = vike_log::capture::captured(|| {
        run_loop(&rest, &sender, command_rx, Vec::<Event>::new, Duration::from_secs(3600));
    });

    assert_eq!(rest.submitted.lock().unwrap().len(), 3, "the loop kept serving commands");
    let warnings: Vec<_> = captured
        .iter()
        .filter(|e| e.level == tracing::Level::WARN && e.message.contains("the core has exited"))
        .collect();
    assert_eq!(warnings.len(), 1, "6 dropped events must log ONE warning, got: {captured:?}");
}

/// The negative: with the core alive the loop logs no core-gone warning.
#[test]
fn run_loop_logs_no_core_gone_warning_while_the_core_is_alive() {
    let (sender, mut ingest) = event_channel(64);
    let rest = MockRest::default();
    let (tx, command_rx) = channel();
    tx.send(ExecutionClientActorCommand::Submit(Box::new(req("c1")), Instant::now())).unwrap();
    tx.send(ExecutionClientActorCommand::Shutdown).unwrap();

    let ((), captured) = vike_log::capture::captured(|| {
        run_loop(&rest, &sender, command_rx, Vec::<Event>::new, Duration::from_secs(3600));
    });

    assert!(
        !captured.iter().any(|e| e.message.contains("the core has exited")),
        "got: {captured:?}"
    );
    assert_eq!(drain_ready(|| ingest.try_recv().ok()).len(), 2);
}

/// `forward_poll`, as the bybit and okx funding polls ran it: events go out in order and end a streak;
/// a failure is handed back for the warn only on the FIRST of a streak; a closed lane is `Err`.
/// Kill proof: hand back every failure (`then_some` dropped), never reset the streak, or keep going on
/// a closed lane, and this goes red.
#[test]
fn forward_poll_forwards_in_order_and_hands_back_only_a_streaks_first_failure() {
    let (sender, mut ingest) = event_channel(64);
    let accepted = |order_client_id: &str| {
        Event::OrderAccepted(OrderAccepted {
            order_client_id: order_client_id.to_string(),
            venue_order_id: None,
            timestamp: 0,
        })
    };
    let mut streak = 0u32;
    let mut poll = |polled: Result<Vec<Event>, &'static str>| {
        forward_poll(polled, &sender, &mut streak).map_err(|_| "core gone")
    };

    assert_eq!(poll(Err("down")), Ok(Some("down")), "the first failure of a streak is said");
    assert_eq!(poll(Err("still down")), Ok(None), "the rest of the streak is quiet");
    assert_eq!(poll(Ok(vec![accepted("a"), accepted("b")])), Ok(None));
    assert_eq!(poll(Err("down again")), Ok(Some("down again")), "a success ended the streak");
    let order_client_ids: Vec<String> = drain_ready(|| ingest.try_recv().ok())
        .iter()
        .map(|e| match e {
            Event::OrderAccepted(a) => a.order_client_id.to_string(),
            other => panic!("only the polled events reach the lane: {other:?}"),
        })
        .collect();
    assert_eq!(order_client_ids, ["a", "b"]);

    drop(ingest);
    assert_eq!(poll(Ok(vec![accepted("c")])), Err("core gone"), "a closed lane ends the thread");
}
