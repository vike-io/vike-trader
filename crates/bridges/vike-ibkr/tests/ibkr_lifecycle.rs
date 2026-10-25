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

use std::assert_matches;
use std::time::{Duration, Instant};

use vike_exec::ExecutionClient;
use vike_exec::{Ingest, event_channel};
use vike_ibkr::run_exec_for_test;
use vike_ibkr::testing::{FakeTransport, ScriptedInbound};
use vike_model::OrderRequest;
use vike_model::events::Event;

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
        order_type: vike_model::OrderType::Market,
        ..Default::default()
    };
    handle.submit(&order);

    let got = drain(&mut rx_events, 4, Duration::from_secs(2));
    assert_eq!(got.len(), 4, "expected exactly 4 events in 2s, got {got:?}");
    assert_matches!(got[0], Event::OrderSubmitted(_), "got[0]={:?}", got[0]);
    assert_matches!(got[1], Event::OrderAccepted(_), "got[1]={:?}", got[1]);
    // The bare fill (what reaches the account) then its wrapper (what advances the order).
    assert_matches!(got[2], Event::Fill(_), "got[2]={:?}", got[2]);
    assert_matches!(got[3], Event::OrderFilled(_), "got[3]={:?}", got[3]);

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
    assert_matches!(
        got[0], Event::OrderCanceled(ref e) if e.client_order_id == "coid-old",
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
        order_type: vike_model::OrderType::Limit,
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
        order_type: vike_model::OrderType::Market,
        ..Default::default()
    };
    handle.submit(&order);

    let got = drain(&mut rx_events, 2, Duration::from_secs(2));
    assert_eq!(got.len(), 2, "expected Submitted + Accepted in 2s, got {got:?}");
    assert_matches!(got[0], Event::OrderSubmitted(_), "got[0]={:?}", got[0]);
    assert_matches!(got[1], Event::OrderAccepted(_), "got[1]={:?}", got[1]);
    // THE BUG, asserted directly: with its commission lost, the fill does not emit at all.
    let stranded = drain(&mut rx_events, 1, Duration::from_millis(300));
    assert!(stranded.is_empty(), "the fill must still be unemitted, got {stranded:?}");

    // THE FIX: the reconnect resync asks IBKR to replay the day's executions.
    fake.inject(ScriptedInbound::StreamResync);
    let got = drain(&mut rx_events, 2, Duration::from_secs(2));
    assert_eq!(got.len(), 2, "expected the recovered fill (Fill + wrapper) in 2s, got {got:?}");
    assert_matches!(&got[0], Event::Fill(b) if b.commission == 1.25);
    match &got[1] {
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
        order_type: vike_model::OrderType::Market,
        ..Default::default()
    };
    handle.submit(&order);

    let got = drain(&mut rx_events, 2, Duration::from_secs(3));
    assert_eq!(got.len(), 2, "expected Submitted + a TERMINAL, got {got:?}");
    assert_matches!(got[0], Event::OrderSubmitted(_), "got[0]={:?}", got[0]);
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
        order_type: vike_model::OrderType::Limit,
        price: Some(1.0),
        ..Default::default()
    };
    handle.submit(&order);
    let got = drain(&mut rx_events, 2, Duration::from_secs(2));
    assert_eq!(got.len(), 2, "expected Submitted + Accepted, got {got:?}");
    assert_matches!(got[1], Event::OrderAccepted(_), "got[1]={:?}", got[1]);

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
            order_type: vike_model::OrderType::Market,
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

// ---------------------------------------------------------------------------------------------
// LEARNING the venue on a fresh socket connection (the gatewayhub rebuilds the adapter after the
// gateway's daily logoff), and the loud edges of the order path. W14c deliverable B: the rework of the
// W14b bridge edit that review 12 held out of the release.
// ---------------------------------------------------------------------------------------------

use vike_ibkr::testing::{FAKE_ACCOUNT, Snapshot, SnapshotMode};

fn limit_order(coid: &str) -> OrderRequest {
    OrderRequest {
        client_order_id: coid.into(),
        venue: "ibkr".into(),
        symbol: "AAPL.SMART.USD".into(),
        side: 1,
        qty: 10.0,
        order_type: vike_model::OrderType::Limit,
        price: Some(1.0),
        ..Default::default()
    }
}

/// What the lane carried, in order, as short words: `Accepted:coid`, `PartiallyFilled:coid`, ... and
/// `link:up` / `link:down` for the `ExecLink` transitions. Reads until nothing arrived for `quiet`.
fn seen(rx: &mut tokio::sync::mpsc::Receiver<Ingest>, quiet: Duration) -> Vec<String> {
    let mut out = Vec::new();
    let mut last = Instant::now();
    while last.elapsed() < quiet {
        match rx.try_recv() {
            // The bare `Fill` (what reaches the account) is not what these lists are about: the lifecycle is.
            Ok(Ingest::Event(Event::Fill(_))) => last = Instant::now(),
            Ok(Ingest::Event(ev)) => {
                last = Instant::now();
                out.push(word(&ev));
            }
            Ok(Ingest::ExecLink(u)) => {
                last = Instant::now();
                out.push(format!("link:{}", if u.up { "up" } else { "down" }));
            }
            Ok(_) => {}
            Err(_) => std::thread::sleep(Duration::from_millis(5)),
        }
    }
    out
}

fn word(ev: &Event) -> String {
    match ev {
        Event::OrderSubmitted(e) => format!("Submitted:{}", e.client_order_id),
        Event::OrderAccepted(e) => format!("Accepted:{}", e.client_order_id),
        Event::OrderRejected(e) => format!("Rejected:{}", e.client_order_id),
        Event::OrderPartiallyFilled(e) => format!("PartiallyFilled:{}", e.client_order_id),
        Event::OrderFilled(e) => format!("Filled:{}", e.client_order_id),
        Event::OrderCanceled(e) => format!("Canceled:{}", e.client_order_id),
        Event::OrderCancelRejected(e) => format!("CancelRejected:{}", e.client_order_id),
        Event::OrderModifyRejected(e) => format!("ModifyRejected:{}", e.client_order_id),
        other => format!("{other:?}"),
    }
}

fn wait_for(mut cond: impl FnMut() -> bool) -> bool {
    let deadline = Instant::now() + Duration::from_secs(3);
    while Instant::now() < deadline {
        if cond() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    cond()
}

/// A venue row of ANOTHER owner: `client_id`, `order_ref` and `account` as given.
fn row_of(order_id: i32, order_ref: &str, client_id: i32, account: &str) -> ScriptedInbound {
    ScriptedInbound::VenueOrder {
        order_id,
        order_ref: order_ref.into(),
        client_id,
        account: account.into(),
        total_qty: 10.0,
        status: "Submitted".into(),
        filled_qty: 0.0,
    }
}

fn fill_of(order_id: i32, coid: &str, exec_id: &str, shares: f64) -> Vec<ScriptedInbound> {
    vec![
        ScriptedInbound::ExecutionOf {
            order_id,
            order_ref: coid.into(),
            exec_id: exec_id.into(),
            symbol: "AAPL.SMART.USD".into(),
            shares,
            price: 190.0,
            client_id: vike_ibkr::testing::FAKE_CLIENT_ID,
        },
        ScriptedInbound::Commission { exec_id: exec_id.into(), commission: 1.0 },
    ]
}

/// **The whole rebuilt-connection story through the real loop**, with the strangers the open-order
/// request also returns. The connection asks for the open orders on `Connected`, binds only the one
/// order that is OURS (a manual TWS order, another client's and another account's change nothing: not the
/// id map, not the id counter), announces it accepted, asks for the day's executions only once the open
/// orders ended, folds the replay (our order's partial as a PARTIAL, a finished order's execution as
/// nothing), and only then announces the link up. The order can then be cancelled under IB's own id,
/// and the fill that completes it is final.
#[test]
fn a_rebuilt_connection_learns_its_own_orders_and_nobody_elses_then_announces_the_link_up() {
    vike_log::test_init();
    let (events, mut rx) = event_channel(256);

    let mut fake = FakeTransport::new().with_socket_identity();
    fake.script_socket_connect(101);
    fake.on_request_open_orders(|| {
        vec![
            ScriptedInbound::venue_order(105, "coid-rested", 10.0),
            // A manual TWS order: id 0, no orderRef, client id 0.
            row_of(0, "", 0, FAKE_ACCOUNT),
            // Another API client's order, with a huge id (must not move the counter) and a ref.
            row_of(5000, "theirs", 3, FAKE_ACCOUNT),
            // Our client id, another account of the login.
            row_of(78, "coid-other-account", vike_ibkr::testing::FAKE_CLIENT_ID, "DU999"),
        ]
    });
    fake.on_request_executions(|| {
        let mut rows = fill_of(105, "coid-rested", "e1", 4.0);
        // The day's execution of an order that finished before this connection.
        rows.extend(fill_of(900, "coid-finished", "old1", 2.0));
        rows
    });
    let cancels = fake.cancel_calls();
    let places = fake.place_calls();
    let exec_requests = fake.execution_requests();
    let mut handle = run_exec_for_test(events, Box::new(fake.clone()));

    assert_eq!(
        seen(&mut rx, Duration::from_millis(400)),
        [
            "Accepted:coid-rested",
            "PartiallyFilled:coid-rested",
            "PartiallyFilled:coid-finished",
            "link:up"
        ],
        "the order is announced, its partial is a partial, the finished order's execution is RECOVERED \
         (a partial, by its orderRef: the engine's trade-id dedup drops it if it already folded it), \
         and the link is announced up LAST, once"
    );
    assert_eq!(*exec_requests.lock().unwrap(), 1, "the executions were asked for once");

    // The learned order is managed under IB's own id.
    handle.cancel("coid-rested");
    assert!(wait_for(|| !cancels.lock().unwrap().is_empty()));
    assert_eq!(cancels.lock().unwrap().as_slice(), &[105]);
    // The strangers are not: a cancel of one is refused loudly and nothing is sent.
    handle.cancel("theirs");
    handle.cancel("coid-other-account");
    assert_eq!(
        seen(&mut rx, Duration::from_millis(300)),
        ["CancelRejected:theirs", "CancelRejected:coid-other-account"]
    );
    assert_eq!(cancels.lock().unwrap().as_slice(), &[105]);

    // A new order takes the id above OUR resting order's (106), not above the stranger's (5001).
    handle.submit(&limit_order("coid-new"));
    assert!(wait_for(|| !places.lock().unwrap().is_empty()));
    assert_eq!(places.lock().unwrap().as_slice(), &[106]);

    // The rest of the rebound order fills: the fill that completes it is final.
    for s in fill_of(105, "coid-rested", "e2", 6.0) {
        fake.inject(s);
    }
    assert_eq!(
        seen(&mut rx, Duration::from_millis(300)),
        ["Submitted:coid-new", "Filled:coid-rested"],
        "the completing fill is final"
    );
    handle.detach();
}

/// **Finding 1 through the loop.** A manual TWS order row (id 0, empty ref) used to bind `""` under id 0
/// and swallow the next id-less async rejection, which the bridge delivers with order id 0.
#[test]
fn a_manual_order_row_does_not_swallow_the_next_idless_rejection() {
    vike_log::test_init();
    let (events, mut rx) = event_channel(64);
    let mut fake = FakeTransport::new().with_socket_identity();
    fake.script_socket_connect(101);
    fake.on_request_open_orders(|| vec![row_of(0, "", 0, FAKE_ACCOUNT)]);
    fake.on_place_order(|_| Vec::new()); // placed, never acknowledged
    let mut handle = run_exec_for_test(events, Box::new(fake.clone()));
    assert_eq!(seen(&mut rx, Duration::from_millis(300)), ["link:up"]);

    handle.submit(&limit_order("coid-real"));
    std::thread::sleep(Duration::from_millis(100));
    fake.inject(ScriptedInbound::Error { code: 201, order_id: 0, msg: "order rejected".into() });
    assert_eq!(
        seen(&mut rx, Duration::from_millis(300)),
        ["Submitted:coid-real", "Rejected:coid-real"],
        "the rejection is the real order's, not the coid \"\""
    );
    handle.detach();
}

/// **The 2026-10-25 defect through the loop.** IB refused a flagged GTC limit order with 10052 "Invalid time in
/// force:GTC" (id-less on the stream, no order status after it); the order used to stay SUBMITTED until the
/// core's watchdog, a minute later. Now the refusal ends it at once.
#[test]
fn invalid_time_in_force_ends_the_order_at_once() {
    vike_log::test_init();
    let (events, mut rx) = event_channel(64);
    let mut fake = FakeTransport::new().with_socket_identity();
    fake.script_socket_connect(101);
    fake.on_place_order(|_| Vec::new()); // placed, never acknowledged: IB sent only the error
    let mut handle = run_exec_for_test(events, Box::new(fake.clone()));
    assert_eq!(seen(&mut rx, Duration::from_millis(300)), ["link:up"]);

    handle.submit(&limit_order("coid-flagged"));
    std::thread::sleep(Duration::from_millis(100));
    fake.inject(ScriptedInbound::Error {
        code: 10052,
        order_id: 0,
        msg: "Invalid time in force:GTC".into(),
    });
    assert_eq!(
        seen(&mut rx, Duration::from_millis(300)),
        ["Submitted:coid-flagged", "Rejected:coid-flagged"]
    );
    handle.detach();
}

/// When the open-order request fails, the connection is NOT learned: no link-up is announced, the
/// order it could not see cannot be cancelled (said loudly), and new orders still work.
#[test]
fn a_failed_open_order_request_leaves_the_connection_unlearned_and_the_link_unannounced() {
    vike_log::test_init();
    let (events, mut rx) = event_channel(64);
    let mut fake = FakeTransport::new().with_socket_identity();
    fake.script_socket_connect(101);
    fake.open_orders_end(SnapshotMode::Failed);
    let exec_requests = fake.execution_requests();
    let places = fake.place_calls();
    let mut handle = run_exec_for_test(events, Box::new(fake.clone()));

    assert!(seen(&mut rx, Duration::from_millis(400)).is_empty(), "no link-up, no events");
    assert_eq!(*exec_requests.lock().unwrap(), 0, "no executions replay without the open orders");

    handle.cancel("coid-rested");
    assert_eq!(seen(&mut rx, Duration::from_millis(300)), ["CancelRejected:coid-rested"]);
    // F6: a connection that does not know which orders of this client rest at the venue places no order: an id
    // handed out now could be a resting order's, and in IB's model that is an AMEND of it.
    handle.submit(&limit_order("coid-new"));
    assert_eq!(
        seen(&mut rx, Duration::from_millis(300)),
        ["Submitted:coid-new", "Rejected:coid-new"],
        "a terminal reject with words, never a silent drop"
    );
    assert!(places.lock().unwrap().is_empty(), "nothing reached the transport");
    handle.detach();
}

#[test]
fn a_failed_executions_replay_leaves_the_connection_unlearned_too() {
    vike_log::test_init();
    let (events, mut rx) = event_channel(64);
    let mut fake = FakeTransport::new().with_socket_identity();
    fake.script_socket_connect(101);
    fake.on_request_open_orders(|| vec![ScriptedInbound::venue_order(105, "coid-rested", 10.0)]);
    fake.executions_end(SnapshotMode::Failed);
    let mut handle = run_exec_for_test(events, Box::new(fake.clone()));
    assert_eq!(
        seen(&mut rx, Duration::from_millis(400)),
        ["Accepted:coid-rested"],
        "the order is known, but its fills are not replayed: the link is not announced up"
    );
    handle.detach();
}

/// The executions are asked for only when the open orders have ENDED: a replayed execution resolves to
/// an order only if that order is already bound.
#[test]
fn the_executions_replay_waits_for_the_end_of_the_open_orders() {
    vike_log::test_init();
    let (events, mut rx) = event_channel(64);
    let mut fake = FakeTransport::new().with_socket_identity();
    fake.script_socket_connect(101);
    fake.open_orders_end(SnapshotMode::Silent);
    let exec_requests = fake.execution_requests();
    let mut handle = run_exec_for_test(events, Box::new(fake.clone()));
    assert!(seen(&mut rx, Duration::from_millis(400)).is_empty());
    assert_eq!(*exec_requests.lock().unwrap(), 0, "the open orders have not ended yet");

    fake.inject(ScriptedInbound::SnapshotEnd { snapshot: Snapshot::OpenOrders, complete: true });
    assert!(wait_for(|| *exec_requests.lock().unwrap() == 1), "now the executions are asked for");
    assert_eq!(seen(&mut rx, Duration::from_millis(300)), ["link:up"]);
    handle.detach();
}

/// The link is announced up ONCE: a later executions request (the stranded-fill sweep's) ending is
/// not a second learning.
#[test]
fn a_later_executions_request_ending_announces_nothing() {
    vike_log::test_init();
    let (events, mut rx) = event_channel(64);
    let mut fake = FakeTransport::new().with_socket_identity();
    fake.script_socket_connect(101);
    let mut handle = run_exec_for_test(events, Box::new(fake.clone()));
    assert_eq!(seen(&mut rx, Duration::from_millis(400)), ["link:up"]);
    fake.inject(ScriptedInbound::SnapshotEnd { snapshot: Snapshot::Executions, complete: true });
    assert!(seen(&mut rx, Duration::from_millis(300)).is_empty());
    handle.detach();
}

/// A connection that never queues `Connected` (a script that predates the learning, and the cpapi
/// backend) announces no link-up: only a connection that learned says so.
#[test]
fn a_connection_that_learned_nothing_announces_nothing() {
    vike_log::test_init();
    let (events, mut rx) = event_channel(64);
    let mut fake = FakeTransport::new();
    fake.script(ScriptedInbound::NextValidId(101));
    fake.script(ScriptedInbound::AccountsReady);
    let mut handle = run_exec_for_test(events, Box::new(fake.clone()));
    assert!(seen(&mut rx, Duration::from_millis(400)).is_empty());
    handle.detach();
}

/// The death AFTER the connection learned is still announced once, as the link going down.
#[test]
fn the_death_of_a_learned_connection_is_announced_down_once() {
    vike_log::test_init();
    let (tx, mut rx) = event_channel(64);
    let events = tx.routed("ibkr#paper2");
    let mut fake = FakeTransport::new().with_socket_identity();
    fake.script_socket_connect(101);
    let mut handle = run_exec_for_test(events, Box::new(fake.clone()));
    assert_eq!(seen(&mut rx, Duration::from_millis(300)), ["link:up"]);
    fake.inject(ScriptedInbound::StreamDead { reason: "connection reset by peer".into() });
    fake.inject(ScriptedInbound::StreamDead { reason: "stream closed".into() });
    assert_eq!(seen(&mut rx, Duration::from_millis(400)), ["link:down"], "once, not twice");
    handle.detach();
}

/// The route key the transition is stamped with is the account's.
#[test]
fn the_link_transitions_carry_the_accounts_route_key() {
    vike_log::test_init();
    let (tx, mut rx) = event_channel(64);
    let events = tx.routed("ibkr#paper2");
    let mut fake = FakeTransport::new().with_socket_identity();
    fake.script_socket_connect(101);
    let mut handle = run_exec_for_test(events, Box::new(fake.clone()));
    let deadline = Instant::now() + Duration::from_secs(2);
    let mut keys = Vec::new();
    while keys.is_empty() && Instant::now() < deadline {
        match rx.try_recv() {
            Ok(Ingest::ExecLink(u)) => {
                keys.push((u.up, u.route_key.to_string(), u.venue.to_string()))
            }
            Ok(_) => {}
            Err(_) => std::thread::sleep(Duration::from_millis(5)),
        }
    }
    assert_eq!(keys, vec![(true, "ibkr#paper2".to_string(), "ibkr".to_string())]);
    handle.detach();
}

/// **Review of W14b, test gap: a death folded INSIDE the readiness wait.** A submit that arrives before
/// the handshake completes waits for it; a stream that dies in that wait used to be folded without being
/// announced, so the host learned of it by a probe.
#[test]
fn a_death_inside_the_readiness_wait_is_announced_and_the_submit_is_rejected() {
    vike_log::test_init();
    let (events, mut rx) = event_channel(64);
    let mut fake = FakeTransport::new();
    fake.script(ScriptedInbound::NextValidId(101)); // no AccountsReady: the handshake is incomplete
    let places = fake.place_calls();
    let mut handle = run_exec_for_test(events, Box::new(fake.clone()));

    handle.submit(&limit_order("coid-early"));
    std::thread::sleep(Duration::from_millis(150)); // the loop is in its readiness wait now
    fake.inject(ScriptedInbound::StreamDead { reason: "connection reset by peer".into() });

    let got = seen(&mut rx, Duration::from_millis(500));
    assert_eq!(got, ["Submitted:coid-early", "link:down", "Rejected:coid-early"], "got {got:?}");
    assert!(places.lock().unwrap().is_empty(), "nothing reached the transport");
    handle.detach();
}

/// Review of W14b, finding 9: the highest id was handed out twice (a second order took the first one's
/// live id, an AMEND in IB's model). The id space spent, the intent is refused instead.
#[test]
fn when_the_id_space_is_spent_the_next_order_is_refused_not_placed_under_a_live_id() {
    vike_log::test_init();
    let (events, mut rx) = event_channel(64);
    let mut fake = FakeTransport::new();
    fake.script(ScriptedInbound::NextValidId(i32::MAX));
    fake.script(ScriptedInbound::AccountsReady);
    let places = fake.place_calls();
    let mut handle = run_exec_for_test(events, Box::new(fake.clone()));

    handle.submit(&limit_order("coid-last"));
    handle.submit(&limit_order("coid-after"));
    let got = seen(&mut rx, Duration::from_millis(500));
    assert_eq!(
        got,
        ["Submitted:coid-last", "Submitted:coid-after", "Rejected:coid-after"],
        "got {got:?}"
    );
    assert_eq!(places.lock().unwrap().as_slice(), &[i32::MAX], "MAX was used once");
    handle.detach();
}

/// A cancel of an id the connection does not know used to be a silent no-op. The caller believed the
/// order was being cancelled while it rested at IB. It now gets a NON-terminal `OrderCancelRejected`
/// naming the coid, and nothing is sent.
#[test]
fn a_cancel_of_an_unknown_order_is_refused_loudly_not_dropped() {
    vike_log::test_init();
    let (events, mut rx_events) = event_channel(64);

    let mut fake = FakeTransport::new();
    fake.script(ScriptedInbound::NextValidId(101));
    fake.script(ScriptedInbound::AccountsReady);
    let cancels = fake.cancel_calls();
    let mut handle = run_exec_for_test(events, Box::new(fake.clone()));

    handle.cancel("coid-nobody-knows");
    let got = drain(&mut rx_events, 1, Duration::from_secs(2));
    match got.as_slice() {
        [Event::OrderCancelRejected(r)] => {
            assert_eq!(r.client_order_id, "coid-nobody-knows");
            let reason = r.reason.to_string();
            assert!(reason.contains("no venue order"), "the reason says why: {reason}");
            assert!(reason.contains("NOT sent"), "and that nothing was sent: {reason}");
        }
        other => panic!("expected one non-terminal OrderCancelRejected, got {other:?}"),
    }
    assert!(cancels.lock().unwrap().is_empty(), "nothing reached the transport");
    handle.detach();
}

#[test]
fn a_modify_of_an_unknown_order_is_refused_loudly_not_dropped() {
    vike_log::test_init();
    let (events, mut rx_events) = event_channel(64);

    let mut fake = FakeTransport::new();
    fake.script(ScriptedInbound::NextValidId(101));
    fake.script(ScriptedInbound::AccountsReady);
    let modifies = fake.modify_calls();
    let mut handle = run_exec_for_test(events, Box::new(fake.clone()));

    handle.modify(&limit_order("coid-nobody-knows"), Some(5.0), Some(2.0));
    let got = drain(&mut rx_events, 1, Duration::from_secs(2));
    match got.as_slice() {
        [Event::OrderModifyRejected(r)] => {
            assert_eq!(r.client_order_id, "coid-nobody-knows");
            assert!(r.reason.to_string().contains("no venue order"), "{}", r.reason);
        }
        other => panic!("expected one non-terminal OrderModifyRejected, got {other:?}"),
    }
    assert!(modifies.lock().unwrap().is_empty(), "nothing reached the transport");
    handle.detach();
}

/// The control: a cancel of an order this connection DID place is still sent and says nothing extra.
#[test]
fn a_cancel_of_a_known_order_is_sent_and_adds_no_advisory() {
    vike_log::test_init();
    let (events, mut rx_events) = event_channel(64);

    let mut fake = FakeTransport::new();
    fake.script(ScriptedInbound::NextValidId(101));
    fake.script(ScriptedInbound::AccountsReady);
    fake.on_place_order(|order_id| {
        vec![ScriptedInbound::OrderStatus {
            order_id,
            order_ref: "coid-known".into(),
            status: "Submitted".into(),
        }]
    });
    let cancels = fake.cancel_calls();
    let mut handle = run_exec_for_test(events, Box::new(fake.clone()));
    handle.submit(&limit_order("coid-known"));
    assert_eq!(drain(&mut rx_events, 2, Duration::from_secs(2)).len(), 2, "Submitted + Accepted");
    handle.cancel("coid-known");
    assert!(wait_for(|| !cancels.lock().unwrap().is_empty()));
    assert_eq!(cancels.lock().unwrap().as_slice(), &[101]);
    let extra = drain(&mut rx_events, 1, Duration::from_millis(300));
    assert!(extra.is_empty(), "a known cancel adds no advisory, got {extra:?}");
    handle.detach();
}

/// The cpapi pump's resync keeps its old shape: `StreamResync` asks for both at once, rows are the bare
/// `OpenOrder` pair, and nothing is announced (the learning belongs to the socket's `Connected`).
#[test]
fn the_cpapi_style_resync_is_unchanged_and_announces_nothing() {
    vike_log::test_init();
    let (events, mut rx) = event_channel(64);
    let mut fake = FakeTransport::new();
    fake.script(ScriptedInbound::NextValidId(101));
    fake.script(ScriptedInbound::AccountsReady);
    fake.on_request_open_orders(|| {
        vec![ScriptedInbound::OpenOrder { order_id: 55, order_ref: "coid-old".into() }]
    });
    let opens = fake.open_order_requests();
    let execs = fake.execution_requests();
    let mut handle = run_exec_for_test(events, Box::new(fake.clone()));
    fake.inject(ScriptedInbound::StreamResync);
    assert!(wait_for(|| *opens.lock().unwrap() == 1 && *execs.lock().unwrap() == 1));
    assert!(seen(&mut rx, Duration::from_millis(300)).is_empty());
    handle.detach();
}

// ---- review 18: R1 (the suspicious snapshot, the recovered fill), R5 (commands wait for the learning) ----

/// **R1.** The open orders came back, all tagged with an `orderRef`, NONE attributed to this client: either
/// they are all someone else's, or the decoder did not attribute our own. Either way the connection must not
/// claim to have learned: no executions replay, no link-up (the host keeps its hold).
#[test]
fn a_snapshot_of_tagged_foreign_rows_does_not_announce_the_link_up_nor_replay_executions() {
    vike_log::test_init();
    let (events, mut rx) = event_channel(64);
    let mut fake = FakeTransport::new().with_socket_identity();
    fake.script_socket_connect(101);
    fake.on_request_open_orders(|| {
        vec![row_of(55, "coid-ours-but-unattributed", 0, ""), row_of(56, "theirs", 3, FAKE_ACCOUNT)]
    });
    let exec_requests = fake.execution_requests();
    let mut handle = run_exec_for_test(events, Box::new(fake.clone()));
    assert!(seen(&mut rx, Duration::from_millis(500)).is_empty(), "no accept, no link-up");
    assert_eq!(*exec_requests.lock().unwrap(), 0);
    handle.detach();
}

/// Rows made by hand in TWS carry no `orderRef`: that is not suspicious, the connection learned (it has
/// nothing of its own to find), and says so.
#[test]
fn hand_made_rows_alone_still_let_the_connection_announce_the_link_up() {
    vike_log::test_init();
    let (events, mut rx) = event_channel(64);
    let mut fake = FakeTransport::new().with_socket_identity();
    fake.script_socket_connect(101);
    fake.on_request_open_orders(|| {
        vec![row_of(0, "", 0, FAKE_ACCOUNT), row_of(-4, "", 0, FAKE_ACCOUNT)]
    });
    let mut handle = run_exec_for_test(events, Box::new(fake.clone()));
    assert_eq!(seen(&mut rx, Duration::from_millis(500)), ["link:up"]);
    handle.detach();
}

/// **R1, the recovery.** A fill the outage swallowed, for an order that is no longer open: the replay carries
/// it with our client id and our `orderRef`, nothing else explains it, so it is recovered as a PARTIAL. An
/// execution of ANOTHER client's order is not.
#[test]
fn a_fill_the_outage_swallowed_is_recovered_by_its_order_ref_as_a_partial() {
    vike_log::test_init();
    let (events, mut rx) = event_channel(64);
    let mut fake = FakeTransport::new().with_socket_identity();
    fake.script_socket_connect(101);
    fake.on_request_executions(|| {
        let mut rows = fill_of(900, "coid-old", "old1", 25.0);
        let mut theirs = fill_of(901, "theirs", "their1", 3.0);
        if let ScriptedInbound::ExecutionOf { client_id, .. } = &mut theirs[0] {
            *client_id = 3;
        }
        rows.extend(theirs);
        rows
    });
    let mut handle = run_exec_for_test(events, Box::new(fake.clone()));
    assert_eq!(
        seen(&mut rx, Duration::from_millis(500)),
        ["PartiallyFilled:coid-old", "link:up"],
        "recovered as a partial, with no accept invented for an order the venue no longer holds (F4); the other client's execution is not"
    );
    handle.detach();
}

/// **R5.** A submit that arrives while the connection is still learning waits (bounded) for the learning
/// to finish: it would otherwise rest on an id the venue may already use.
#[test]
fn a_submit_during_the_learning_waits_for_it_to_finish() {
    vike_log::test_init();
    let (events, mut rx) = event_channel(64);
    let mut fake = FakeTransport::new().with_socket_identity();
    fake.script_socket_connect(101);
    fake.open_orders_end(SnapshotMode::Silent);
    let places = fake.place_calls();
    let mut handle = run_exec_for_test(events, Box::new(fake.clone()));
    std::thread::sleep(Duration::from_millis(100));

    handle.submit(&limit_order("coid-new"));
    std::thread::sleep(Duration::from_millis(400));
    assert!(places.lock().unwrap().is_empty(), "not placed while the open orders are unknown");

    // The venue lists a resting order of ours at id 106, then ends the snapshot.
    fake.inject(ScriptedInbound::venue_order(106, "coid-rested", 10.0));
    fake.inject(ScriptedInbound::SnapshotEnd { snapshot: Snapshot::OpenOrders, complete: true });
    assert!(wait_for(|| !places.lock().unwrap().is_empty()));
    assert_eq!(
        places.lock().unwrap().as_slice(),
        &[107],
        "placed after the learning, ABOVE the resting order's id"
    );
    let got = seen(&mut rx, Duration::from_millis(300));
    assert!(got.contains(&"Submitted:coid-new".to_string()), "{got:?}");
    handle.detach();
}

/// A cancel of a coid the connection does not know yet waits for the learning, then goes out under IB's id.
#[test]
fn a_cancel_during_the_learning_waits_and_then_reaches_the_order_the_venue_listed() {
    vike_log::test_init();
    let (events, mut rx) = event_channel(64);
    let mut fake = FakeTransport::new().with_socket_identity();
    fake.script_socket_connect(101);
    fake.open_orders_end(SnapshotMode::Silent);
    let cancels = fake.cancel_calls();
    let mut handle = run_exec_for_test(events, Box::new(fake.clone()));
    std::thread::sleep(Duration::from_millis(100));

    handle.cancel("coid-rested");
    std::thread::sleep(Duration::from_millis(300));
    assert!(cancels.lock().unwrap().is_empty());
    fake.inject(ScriptedInbound::venue_order(105, "coid-rested", 10.0));
    fake.inject(ScriptedInbound::SnapshotEnd { snapshot: Snapshot::OpenOrders, complete: true });
    assert!(wait_for(|| !cancels.lock().unwrap().is_empty()));
    assert_eq!(cancels.lock().unwrap().as_slice(), &[105]);
    let got = seen(&mut rx, Duration::from_millis(300));
    assert!(!got.iter().any(|w| w.starts_with("CancelRejected")), "no refusal: {got:?}");
    handle.detach();
}

// ---- W14f, finding 2: one foreign TAGGED order must not block all trading ----

/// A hand-typed note in TWS (the account decoded, client id 0), another API client's tagged order and an order
/// of ours in ANOTHER account are somebody else's orders, not a blind decoder: the connection learns, announces
/// the link up and takes the next submit. (One such row used to fail the learning for good: no link-up, and every
/// submit refused for the life of the process.)
#[test]
fn foreign_orders_whose_owner_decoded_do_not_block_the_connection() {
    vike_log::test_init();
    let (events, mut rx) = event_channel(64);
    let mut fake = FakeTransport::new().with_socket_identity();
    fake.script_socket_connect(101);
    fake.on_request_open_orders(|| {
        vec![
            row_of(0, "hedge-note", 0, FAKE_ACCOUNT),
            row_of(56, "theirs", 3, FAKE_ACCOUNT),
            row_of(57, "coid-elsewhere", vike_ibkr::testing::FAKE_CLIENT_ID, "DU9999999"),
        ]
    });
    let places = fake.place_calls();
    let mut handle = run_exec_for_test(events, Box::new(fake.clone()));
    assert_eq!(seen(&mut rx, Duration::from_millis(500)), ["link:up"]);
    handle.submit(&limit_order("coid-new"));
    assert!(wait_for(|| !places.lock().unwrap().is_empty()), "the next submit is placed");
    assert_eq!(places.lock().unwrap().as_slice(), &[101], "no foreign row moved the id counter");
    handle.detach();
}

/// The shape a BLIND decoder gives every row (client id 0, no account, a coid-shaped ref) is still not a learned
/// venue: no link-up, no executions replay, and the submit is refused with words, not placed.
#[test]
fn rows_with_nothing_decoded_are_still_a_blind_decoder() {
    vike_log::test_init();
    let (events, mut rx) = event_channel(64);
    let mut fake = FakeTransport::new().with_socket_identity();
    fake.script_socket_connect(101);
    fake.on_request_open_orders(|| vec![row_of(55, "coid-a", 0, ""), row_of(56, "coid-b", 0, "")]);
    let places = fake.place_calls();
    let exec_requests = fake.execution_requests();
    let mut handle = run_exec_for_test(events, Box::new(fake.clone()));
    assert!(seen(&mut rx, Duration::from_millis(500)).is_empty(), "no accept, no link-up");
    handle.submit(&limit_order("coid-new"));
    let got = seen(&mut rx, Duration::from_millis(400));
    assert_eq!(got, ["Submitted:coid-new", "Rejected:coid-new"], "{got:?}");
    assert!(places.lock().unwrap().is_empty(), "nothing was placed");
    assert_eq!(*exec_requests.lock().unwrap(), 0);
    handle.detach();
}
