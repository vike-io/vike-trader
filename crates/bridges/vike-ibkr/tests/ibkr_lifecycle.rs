//! Full submit→accept→fill lifecycle over the `FakeTransport` — no Gateway, no ibapi.
//!
//! Drives one market BUY through the SAME `run_exec` loop the socket backend will use (Task 9) and
//! asserts the emitted events arrive in order: OrderSubmitted → OrderAccepted → OrderFilled. The
//! fake scripts, in response to `place_order(order_id, ...)`, a Submitted status + an execDetails +
//! a commission report, which fold through the real `EventMapper` (Task 7) into those events.
//!
//! Feature-gated on `test-support`: the `testing` surface (`FakeTransport`/`run_exec_for_test`)
//! only exists behind that feature, so without it this integration test compiles to nothing (and
//! the plain `cargo test -p vike-ibkr` lane stays green).
#![cfg(feature = "test-support")]

use std::time::{Duration, Instant};

use vike_exec::lanes::{event_channel, Ingest};
use vike_exec::ExecutionClient;
use vike_ibkr::run_exec_for_test;
use vike_ibkr::testing::{FakeTransport, ScriptedInbound};
use vike_model::events::Event;
use vike_model::OrderRequest;

#[test]
fn submit_accept_fill_emits_ordered_events() {
    vike_log::test_init();
    let (events, mut rx_events) = event_channel(64);

    // Script: NextValidId(101), AccountsReady, then — in response to the submit's place_order —
    // Submitted status, execDetails, and the commission that joins the fill (by exec_id).
    let mut fake = FakeTransport::new();
    fake.script(ScriptedInbound::NextValidId(101));
    fake.script(ScriptedInbound::AccountsReady);
    fake.on_place_order(|order_id| {
        vec![
            ScriptedInbound::OrderStatus {
                order_id,
                order_ref: "coid-1".into(),
                status: "Submitted".into(),
            },
            ScriptedInbound::ExecDetails {
                order_id,
                exec_id: "e1".into(),
                shares: 10.0,
                price: 190.0,
            },
            ScriptedInbound::Commission { exec_id: "e1".into(), commission: 1.0 },
        ]
    });

    let mut handle = run_exec_for_test(events, Box::new(fake));
    let order = OrderRequest {
        client_order_id: "coid-1".into(),
        venue: "ibkr".into(),
        symbol: "AAPL.SMART.USD".into(),
        side: 1,
        qty: 10.0,
        order_type: "market".into(),
        ..Default::default()
    };
    handle.submit(&order);

    let got = drain(&mut rx_events, 3, Duration::from_secs(2));
    assert_eq!(got.len(), 3, "expected exactly 3 events in 2s, got {got:?}");
    assert!(matches!(got[0], Event::OrderSubmitted(_)), "got[0]={:?}", got[0]);
    assert!(matches!(got[1], Event::OrderAccepted(_)), "got[1]={:?}", got[1]);
    assert!(matches!(got[2], Event::OrderFilled(_)), "got[2]={:?}", got[2]);

    handle.detach();
}

/// IBKR does not replay open orders/executions on reconnect, so the bridge must re-request them
/// and rebuild its coid⇄orderId map from the snapshot. Drives: NextValidId/AccountsReady, then a
/// `StreamResync` inbound (injected asynchronously via a cloned `FakeTransport` handle, mimicking
/// the transport's own background pump signalling a dropped/re-established socket) triggers
/// `request_open_orders`, which the fake scripts to replay ONE open order (order_id 55, orderRef
/// "coid-old") that was never submitted through this process. A subsequent `Cancelled` status for
/// order 55 carrying an EMPTY orderRef must still resolve to "coid-old" — proving the map was
/// rebuilt from the `OpenOrder` snapshot rather than surviving in some other form.
#[test]
fn reconnect_rebuilds_id_map_from_open_orders() {
    vike_log::test_init();
    let (events, mut rx_events) = event_channel(64);

    let mut fake = FakeTransport::new();
    fake.script(ScriptedInbound::NextValidId(101));
    fake.script(ScriptedInbound::AccountsReady);
    // On reconnect the transport replays an open order (order_id 55, orderRef coid-old) via
    // request_open_orders → then a status update the mapper must be able to resolve.
    fake.on_request_open_orders(|| {
        vec![ScriptedInbound::OpenOrder { order_id: 55, order_ref: "coid-old".into() }]
    });

    // `fake.clone()` shares the SAME underlying queue with the handle moved onto the exec loop's
    // thread, so `fake.inject(..)` below still lands on the running transport.
    let mut handle = run_exec_for_test(events, Box::new(fake.clone()));
    fake.inject(ScriptedInbound::StreamResync);
    // After resync, a Cancelled status for order 55 with an EMPTY order_ref must resolve to
    // "coid-old" (proving the map was rebuilt) and emit OrderCanceled with that client_order_id.
    fake.inject(ScriptedInbound::OrderStatus {
        order_id: 55,
        order_ref: "".into(),
        status: "Cancelled".into(),
    });

    let got = drain(&mut rx_events, 1, Duration::from_secs(2));
    assert_eq!(got.len(), 1, "expected exactly 1 event in 2s, got {got:?}");
    assert!(
        matches!(got[0], Event::OrderCanceled(ref e) if e.client_order_id == "coid-old"),
        "got[0]={:?}",
        got[0]
    );

    handle.detach();
}

/// After a submit binds coid→order_id, an `ExecutionClient::modify` must flow through `run_exec` as
/// `ExecCommand::Modify`, resolve that numeric order_id, and call `transport.modify_order` with it
/// (the socket backend inherits the default no-op; here the FakeTransport records the call so we
/// prove the wiring). This is the seam the cpapi backend's native amend hangs off.
#[test]
fn modify_forwards_resolved_order_id_to_transport() {
    vike_log::test_init();
    let (events, _rx) = event_channel(64);

    let mut fake = FakeTransport::new();
    fake.script(ScriptedInbound::NextValidId(101));
    fake.script(ScriptedInbound::AccountsReady);
    let recorded = fake.modify_calls();

    let mut handle = run_exec_for_test(events, Box::new(fake.clone()));
    let order = OrderRequest {
        client_order_id: "coid-1".into(),
        venue: "ibkr".into(),
        symbol: "AAPL.SMART.USD".into(),
        side: 1,
        qty: 5.0,
        order_type: "limit".into(),
        price: Some(190.0),
        ..Default::default()
    };
    handle.submit(&order); // binds order_id 101 → "coid-1"
    handle.modify(&order, Some(7.0), Some(191.0));

    // Wait (bounded) for the exec loop to process both commands in FIFO order.
    let deadline = Instant::now() + Duration::from_secs(2);
    while recorded.lock().unwrap().is_empty() && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(5));
    }
    assert_eq!(
        recorded.lock().unwrap().as_slice(),
        &[101],
        "run_exec must resolve the coid to order_id 101 and call modify_order with it"
    );
    handle.detach();
}

/// Block-receive up to `n` events within `timeout` off the ingest lane. Runs on a plain test
/// thread (no tokio runtime), so `try_recv` + a short sleep is the poll primitive; non-`Event`
/// `Ingest` variants are ignored. `rx_events` is a `tokio::sync::mpsc::Receiver<Ingest>` — kept
/// un-named so the test needs no direct `tokio` dev-dep.
fn drain(rx: &mut tokio::sync::mpsc::Receiver<Ingest>, n: usize, timeout: Duration) -> Vec<Event> {
    let deadline = Instant::now() + timeout;
    let mut out = Vec::new();
    while out.len() < n && Instant::now() < deadline {
        match rx.try_recv() {
            Ok(Ingest::Event(ev)) => out.push(ev),
            Ok(_) => {}
            // Empty (nothing yet) or Disconnected (senders gone) — the deadline bounds the wait.
            Err(_) => std::thread::sleep(Duration::from_millis(5)),
        }
    }
    out
}

/// A fill whose `commissionReport` never arrives emits NOTHING — the join is the only emitter — so
/// on an order that is never cancelled it used to be held forever and the platform's position and
/// realized PnL silently ran short (`event_mapper`'s module doc trap 5). The cure is to RECOVER the
/// real commission from the venue, not to invent one: IBKR's `reqExecutions` re-delivers the day's
/// executions *and* their commission reports, and the exec loop now issues it on every reconnect —
/// the moment a socket blip is the most likely explanation for the missing half.
///
/// Drives the whole thing through the real `run_exec` loop: submit → Submitted → execDetails with
/// NO commission (the fill is provably NOT emitted), then a `StreamResync` triggers
/// `request_executions`, whose scripted replay carries both halves and completes the fill — with
/// IBKR's own commission figure on it.
#[test]
fn reconnect_replay_emits_a_fill_whose_commission_was_lost() {
    vike_log::test_init();
    let (events, mut rx_events) = event_channel(64);

    let mut fake = FakeTransport::new();
    fake.script(ScriptedInbound::NextValidId(101));
    fake.script(ScriptedInbound::AccountsReady);
    // The strand: an accepted order and an execution, but the commissionReport is lost on the wire.
    fake.on_place_order(|order_id| {
        vec![
            ScriptedInbound::OrderStatus {
                order_id,
                order_ref: "coid-1".into(),
                status: "Submitted".into(),
            },
            ScriptedInbound::ExecDetails {
                order_id,
                exec_id: "e1".into(),
                shares: 10.0,
                price: 190.0,
            },
        ]
    });
    // What `reqExecutions` answers with: BOTH halves of the day's executions.
    fake.on_request_executions(|| {
        vec![
            ScriptedInbound::ExecDetails {
                order_id: 101,
                exec_id: "e1".into(),
                shares: 10.0,
                price: 190.0,
            },
            ScriptedInbound::Commission { exec_id: "e1".into(), commission: 1.25 },
        ]
    });

    let mut handle = run_exec_for_test(events, Box::new(fake.clone()));
    let order = OrderRequest {
        client_order_id: "coid-1".into(),
        venue: "ibkr".into(),
        symbol: "AAPL.SMART.USD".into(),
        side: 1,
        qty: 10.0,
        order_type: "market".into(),
        ..Default::default()
    };
    handle.submit(&order);

    let got = drain(&mut rx_events, 2, Duration::from_secs(2));
    assert_eq!(got.len(), 2, "expected Submitted + Accepted in 2s, got {got:?}");
    assert!(matches!(got[0], Event::OrderSubmitted(_)), "got[0]={:?}", got[0]);
    assert!(matches!(got[1], Event::OrderAccepted(_)), "got[1]={:?}", got[1]);
    // THE BUG, asserted directly: with its commission lost, the fill does not emit at all.
    let stranded = drain(&mut rx_events, 1, Duration::from_millis(300));
    assert!(stranded.is_empty(), "the fill must still be unemitted, got {stranded:?}");

    // THE FIX: the reconnect resync asks IBKR to replay the day's executions.
    fake.inject(ScriptedInbound::StreamResync);
    let got = drain(&mut rx_events, 1, Duration::from_secs(2));
    assert_eq!(got.len(), 1, "expected the recovered fill in 2s, got {got:?}");
    match &got[0] {
        Event::OrderFilled(f) => {
            assert_eq!(f.client_order_id, "coid-1");
            assert_eq!(f.fill.last_qty, 10.0);
            // The REAL venue commission — never a fabricated 0.0, which would silently understate
            // cost basis on every fill this path recovers.
            assert_eq!(f.fill.commission, 1.25);
        }
        other => panic!("expected OrderFilled, got {other:?}"),
    }

    handle.detach();
}

// ---------------------------------------------------------------------------------------------
// TIER-1: the stream-death contract. `IbInbound::Reconnected` used to be the socket backend's
// notice that its order-update stream had TERMINATED — a name for a thing that never happened.
// Nothing reconnected, the exec loop stayed alive, and it kept accepting submits against a socket
// whose reader thread had exited. These drive the real `run_exec` loop over the SAME seam.
// ---------------------------------------------------------------------------------------------

/// **The bug, and the fix, in one test.** An order submitted after the stream dies must terminalize
/// HERE and must never reach the transport.
///
/// Before the fix this test's two assertions failed in the two ways that compound: the order WAS
/// placed (into a socket nobody reads — where `submit_order`'s write lands in the kernel buffer and
/// returns `Ok`, so not even the synchronous-failure reject fired), and NO terminal was ever
/// emitted, because every venue event arrives through the pump that had just exited. The order was
/// then unreachable in both directions — no accept, no fill, no terminal, and `cancel` could not
/// reach it either.
///
/// ⚠ The `place_calls` assertion is the load-bearing one. The event lane alone cannot distinguish a
/// refusal from a placement that is simply never answered: both show `OrderSubmitted` and then
/// silence. Only the transport recording proves the order did not go out.
#[test]
fn a_submit_after_the_stream_dies_is_rejected_and_never_reaches_the_transport() {
    vike_log::test_init();
    let (events, mut rx_events) = event_channel(64);

    let mut fake = FakeTransport::new();
    fake.script(ScriptedInbound::NextValidId(101));
    fake.script(ScriptedInbound::AccountsReady);
    // The death notice, delivered before any order — exactly what the socket pump sends when its
    // subscription returns a terminal `Err`.
    fake.script(ScriptedInbound::StreamDead { reason: "connection reset by peer".into() });
    let places = fake.place_calls();

    let mut handle = run_exec_for_test(events, Box::new(fake.clone()));
    // Let the loop fold the three scripted inbounds before the submit lands.
    std::thread::sleep(Duration::from_millis(150));

    let order = OrderRequest {
        client_order_id: "coid-after-death".into(),
        venue: "ibkr".into(),
        symbol: "AAPL.SMART.USD".into(),
        side: 1,
        qty: 10.0,
        order_type: "market".into(),
        ..Default::default()
    };
    handle.submit(&order);

    let got = drain(&mut rx_events, 2, Duration::from_secs(3));
    assert_eq!(got.len(), 2, "expected Submitted + a TERMINAL, got {got:?}");
    assert!(matches!(got[0], Event::OrderSubmitted(_)), "got[0]={:?}", got[0]);
    match &got[1] {
        Event::OrderRejected(r) => {
            assert_eq!(r.client_order_id, "coid-after-death");
            // The refusal must name the ORIGINAL fault, not a generic "not connected" — that is
            // what `IbInbound::StreamDead` carries a `reason` for.
            let reason = r.reason.to_string();
            assert!(reason.contains("DEAD"), "reason must say the stream is dead: {reason}");
            assert!(
                reason.contains("connection reset by peer"),
                "reason must carry the transport's own diagnostic: {reason}"
            );
        }
        other => panic!("expected OrderRejected after stream death, got {other:?}"),
    }
    assert!(
        places.lock().unwrap().is_empty(),
        "an order submitted after the stream died must NEVER reach the transport, but place_order \
         was called with {:?}",
        places.lock().unwrap()
    );

    handle.detach();
}

/// A cancel after the death must synthesize NOTHING. The order may still be resting at IB — the
/// stream dying tells us we cannot see it, not that it is gone — so a fabricated `OrderCanceled`
/// would tell the platform it is flat while the venue holds live exposure. That is the inversion of
/// the vanishing-order bug and the more dangerous direction of the two.
#[test]
fn a_cancel_after_the_stream_dies_synthesizes_no_terminal() {
    vike_log::test_init();
    let (events, mut rx_events) = event_channel(64);

    let mut fake = FakeTransport::new();
    fake.script(ScriptedInbound::NextValidId(101));
    fake.script(ScriptedInbound::AccountsReady);
    fake.on_place_order(|order_id| {
        vec![ScriptedInbound::OrderStatus {
            order_id,
            order_ref: "coid-resting".into(),
            status: "Submitted".into(),
        }]
    });

    let cancels = fake.cancel_calls();
    let mut handle = run_exec_for_test(events, Box::new(fake.clone()));
    let order = OrderRequest {
        client_order_id: "coid-resting".into(),
        venue: "ibkr".into(),
        symbol: "AAPL.SMART.USD".into(),
        side: 1,
        qty: 10.0,
        order_type: "limit".into(),
        price: Some(1.0),
        ..Default::default()
    };
    handle.submit(&order);
    let got = drain(&mut rx_events, 2, Duration::from_secs(2));
    assert_eq!(got.len(), 2, "expected Submitted + Accepted, got {got:?}");
    assert!(matches!(got[1], Event::OrderAccepted(_)), "got[1]={:?}", got[1]);

    // Now the stream dies under a RESTING order, and the operator tries to cancel it.
    fake.inject(ScriptedInbound::StreamDead { reason: "stream closed".into() });
    std::thread::sleep(Duration::from_millis(150));
    handle.cancel("coid-resting");

    // ⚠ THE assertion. The fake's `cancel_order` emits nothing either way, so checking only that no
    // event appeared would pass with the refusal deleted — the test has to key on whether the
    // cancel REACHED the transport.
    std::thread::sleep(Duration::from_millis(300));
    assert!(
        cancels.lock().unwrap().is_empty(),
        "a cancel issued after the stream died must not be handed to a dead transport, but \
         cancel_order was called with {:?}",
        cancels.lock().unwrap()
    );
    let after = drain(&mut rx_events, 1, Duration::from_millis(400));
    assert!(
        after.is_empty(),
        "...and it must synthesize NO terminal — the order may still be live at IB. Got {after:?}"
    );

    handle.detach();
}

/// The venue stays dead. A second submit, well after the notice, is refused on the same terms —
/// the latch is not a one-shot.
#[test]
fn the_stream_death_latch_refuses_every_later_submit() {
    vike_log::test_init();
    let (events, mut rx_events) = event_channel(64);

    let mut fake = FakeTransport::new();
    fake.script(ScriptedInbound::NextValidId(101));
    fake.script(ScriptedInbound::AccountsReady);
    fake.script(ScriptedInbound::StreamDead { reason: "reset".into() });
    let places = fake.place_calls();

    let mut handle = run_exec_for_test(events, Box::new(fake.clone()));
    std::thread::sleep(Duration::from_millis(150));

    for n in 0..3 {
        let order = OrderRequest {
            client_order_id: format!("coid-{n}"),
            venue: "ibkr".into(),
            symbol: "AAPL.SMART.USD".into(),
            side: 1,
            qty: 1.0,
            order_type: "market".into(),
            ..Default::default()
        };
        handle.submit(&order);
    }

    let got = drain(&mut rx_events, 6, Duration::from_secs(3));
    let rejects = got.iter().filter(|e| matches!(e, Event::OrderRejected(_))).count();
    assert_eq!(rejects, 3, "every submit after the death must terminalize, got {got:?}");
    assert!(places.lock().unwrap().is_empty(), "none of them may reach the transport");

    handle.detach();
}
