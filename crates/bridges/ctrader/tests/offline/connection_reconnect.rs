//! Task 6 scripted integration test: the actor thread reconnects and re-authenticates after a
//! dropped socket. `FakeCtrader::start_reconnect_after_handshake` completes the FIRST handshake
//! then closes the connection immediately (simulating a disconnect right as the actor's read loop
//! is about to start); this asserts the actor backs off, opens a SECOND TCP connection to the
//! same fake server, and replays the FULL handshake (a second `APPLICATION_AUTH_REQ`) — proving
//! `conn::reconnect_with_backoff`/`open_and_handshake` wiring end to end, not just the pure retry
//! logic in isolation.

use std::sync::Arc;
use std::time::Duration;

use vike_ctrader::feed::connect;
use vike_ctrader::transport::connection::{ConnectionConfig, connect_and_auth};
use vike_data::DataClient;

use vike_ctrader::test_support::{self, FakeCtrader, NoopSink};

#[test]
fn actor_reconnects_and_reauths_after_a_dropped_socket() {
    let server = FakeCtrader::start_reconnect_after_handshake();
    let config = ConnectionConfig::for_test(server.addr(), "cid", "secret", "token");

    // The initial connect completes against the FIRST served connection, which the fake server
    // drops right after finishing the handshake.
    let handle = connect_and_auth(config, Arc::new(NoopSink)).expect("initial handshake ok");

    // The reconnect happens on the actor's own background thread; poll for a SECOND
    // APPLICATION_AUTH_REQ (the tell-tale of a full re-handshake) rather than sleeping a fixed,
    // possibly-too-short duration.
    server.wait_until_count_at_least("APPLICATION_AUTH_REQ", 2, Duration::from_secs(10));
    server.wait_until_count_at_least("ACCOUNT_AUTH_REQ", 2, Duration::from_secs(10));
    server.wait_until_count_at_least("SYMBOL_BY_ID_REQ", 2, Duration::from_secs(10));

    handle.shutdown();
}

/// Task 6, Fix 2b: an active subscription must be re-issued after a reconnect, not silently
/// dropped. Subscribes to EURUSD quotes on the FIRST (healthy) connection — `track_subscription`
/// records it in the actor's `spot_subs` set BEFORE the write is attempted, so it is tracked
/// regardless of whether the write itself lands before the server drops the socket. The fake
/// server then drops the connection right after replying to that first `SUBSCRIBE_SPOTS_REQ`; once
/// the actor notices, backs off, and reconnects+reauths, it must replay every tracked subscription
/// — a SECOND `SUBSCRIBE_SPOTS_REQ` — on top of the second `APPLICATION_AUTH_REQ`/`ACCOUNT_AUTH_REQ`
/// re-auth pair the existing reconnect test already covers.
#[test]
fn subscription_replays_after_reconnect() {
    let server = FakeCtrader::start_drop_after_subscribe();
    let config = ConnectionConfig::for_test(server.addr(), "cid", "secret", "token");

    let mut data = connect(config, Arc::new(NoopSink)).expect("initial handshake ok");
    data.subscribe_quotes("EURUSD").expect("subscribe_quotes ok");

    // Confirm the subscribe actually went out on the first connection before asserting on the
    // reconnect replay, so a failure here can't be mistaken for a replay failure.
    server.wait_until_count_at_least("SUBSCRIBE_SPOTS_REQ", 1, Duration::from_secs(10));

    server.wait_until_count_at_least("APPLICATION_AUTH_REQ", 2, Duration::from_secs(10));
    server.wait_until_count_at_least("ACCOUNT_AUTH_REQ", 2, Duration::from_secs(10));
    server.wait_until_count_at_least("SUBSCRIBE_SPOTS_REQ", 2, Duration::from_secs(10));

    data.shutdown();
}

/// **A dead data socket says exactly one thing, to the recorder alone: the tape of its spot has a hole from
/// this stamp to the reconnect.** Every status it says carries the tape-hole word, never `quotes` or any
/// other word a strategy holds, which past the strategy lane is the venue's own socket to the link dead-man.
#[test]
fn a_dropped_data_socket_marks_a_tape_hole_and_says_no_other_word() {
    use std::time::Instant;

    use vike_ctrader::test_support::RecordingSink;
    use vike_model::StreamStatus;
    use vike_model::TAPE_HOLE_WORD;

    let server = FakeCtrader::start_drop_after_subscribe();
    let config = ConnectionConfig::for_test(server.addr(), "cid", "secret", "token");
    let sink = Arc::new(RecordingSink::default());
    let before = vike_model::now_ms();
    let mut data = connect(config, sink.clone()).expect("initial handshake ok");
    data.subscribe_quotes("EURUSD").expect("subscribe_quotes ok");

    server.wait_until_count_at_least("SUBSCRIBE_SPOTS_REQ", 2, Duration::from_secs(10));
    let deadline = Instant::now() + Duration::from_secs(5);
    while sink.stream_statuses().len() < 2 && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(10));
    }
    data.shutdown();

    let said = sink.stream_statuses();
    assert_eq!(said.len(), 2, "one opening and one closing: {said:?}");
    assert!(
        said.iter().all(|(venue, symbol, word, _)| venue == "ctrader"
            && symbol == "EURUSD"
            && word == TAPE_HOLE_WORD),
        "{said:?}"
    );
    let StreamStatus::GapStart { at_timestamp_ms: at_ts_ms } = said[0].3 else {
        panic!("the hole opens first: {said:?}");
    };
    assert!(at_ts_ms >= before, "{at_ts_ms} is a stamp of this outage");
    assert_eq!(said[1].3, StreamStatus::Live { gap_started_timestamp_ms: Some(at_ts_ms) });
}

/// A key subscribed while the socket is down had no tape to lose, so a hole closes on the symbols it opened
/// on and on no other. Which side of the drop the subscribe lands on is the scheduler's, so the claim is that
/// every close has its opening, whichever it was. The shutdown joins the actor, so all it said is in.
#[test]
fn a_key_subscribed_during_an_outage_closes_no_hole_it_never_opened() {
    use vike_ctrader::test_support::RecordingSink;
    use vike_model::StreamStatus;

    let server = FakeCtrader::start_reconnect_after_handshake();
    let config = ConnectionConfig::for_test(server.addr(), "cid", "secret", "token");
    let sink = Arc::new(RecordingSink::default());
    let mut data = connect(config, sink.clone()).expect("initial handshake ok");
    // Inside the reconnect's first wait: the fake dropped the socket when the handshake ended.
    std::thread::sleep(Duration::from_millis(200));
    data.subscribe_quotes("EURUSD").expect("subscribe_quotes ok");
    server.wait_until_count_at_least("SUBSCRIBE_SPOTS_REQ", 1, Duration::from_secs(10));
    data.shutdown();

    let said = sink.stream_statuses();
    let closes = |status: &StreamStatus| matches!(status, StreamStatus::Live { .. });
    let opened = said.iter().filter(|(_, _, _, status)| !closes(status)).count();
    assert_eq!(said.len() - opened, opened, "{said:?}");
}

/// **A blackholed data socket's hole starts at the last price it received, not when it was called dead.** On the CI box on
/// 2026-11-09 the prices stopped at 17:51:04 and the hole began at 17:51:43.7, so 39 s of tape read as a quiet market. Here
/// the fake sends one price and goes silent; on the old code the hole began a whole idle bound and the last reads later.
#[test]
fn a_blackholed_data_socket_holes_its_tape_from_the_last_price_it_received() {
    use std::time::Instant;

    use vike_ctrader::test_support::RecordingSink;
    use vike_ctrader::transport::proto::pt;
    use vike_model::StreamStatus;

    let server = FakeCtrader::start_blackhole_after(pt::SUBSCRIBE_SPOTS_REQ);
    let mut config = ConnectionConfig::for_test(server.addr(), "cid", "secret", "token");
    config.read_idle_after = Duration::from_millis(600);
    let sink = Arc::new(RecordingSink::default());
    let mut data = connect(config, sink.clone()).expect("initial handshake ok");
    data.subscribe_quotes("EURUSD").expect("subscribe_quotes ok");

    server.wait_until_count_at_least("SUBSCRIBE_SPOTS_REQ", 2, Duration::from_secs(10));
    let deadline = Instant::now() + Duration::from_secs(5);
    while sink.stream_statuses().len() < 2 && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(10));
    }
    data.shutdown();

    let last_price = sink.quotes().first().expect("the price before the silence").2.local_timestamp;
    let said = sink.stream_statuses();
    let Some(StreamStatus::GapStart { at_timestamp_ms: at_ts_ms }) = said.first().map(|s| s.3)
    else {
        panic!("the hole opens first: {said:?}");
    };
    assert!(
        at_ts_ms <= last_price + 100,
        "the hole starts {} ms after the last price the socket received",
        at_ts_ms - last_price
    );
}

/// A socket that carries no spot has no tape: an exec socket's reconnect says nothing to anybody.
#[test]
fn a_dropped_socket_with_no_spot_marks_no_hole() {
    use vike_ctrader::test_support::RecordingSink;

    let server = FakeCtrader::start_reconnect_after_handshake();
    let config = ConnectionConfig::for_test(server.addr(), "cid", "secret", "token");
    let sink = Arc::new(RecordingSink::default());
    let handle = connect_and_auth(config, sink.clone()).expect("initial handshake ok");
    server.wait_until_count_at_least("SYMBOL_BY_ID_REQ", 2, Duration::from_secs(10));
    handle.shutdown();
    assert!(sink.stream_statuses().is_empty(), "{:?}", sink.stream_statuses());
}

/// F3: on reconnect the actor issues a `ProtoOAReconcileReq` and rebuilds its order client id→orderId
/// correlation map from the pending orders in the response. The fake server drops the first
/// connection right after the handshake, then answers the reconnect's `RECONCILE_REQ` with ONE
/// pending order (`RECONCILE_PENDING_ORDER_CLIENT_ID` → `RECONCILE_PENDING_ORDER_ID`). Asserts: (1) the server
/// saw a `RECONCILE_REQ`; (2) the adapter's map now resolves the reconciled order client id to the right venue
/// orderId, so a subsequent `cancel(order client id)` sends a real `CancelOrderReq`; (3) the reconcile also
/// re-emitted an idempotent `OrderAccepted` (never a fill) for the still-pending order onto the
/// ingest lane.
#[test]
fn reconcile_rebuilds_order_client_id_order_map_on_reconnect() {
    use std::time::Instant;

    use vike_ctrader::transport::connection::connect_and_auth_execution;
    use vike_execution::{ExecutionClient, Ingest, event_channel};
    use vike_model::events::Event;

    use vike_ctrader::test_support::{
        RECONCILE_PENDING_ORDER_CLIENT_ID, RECONCILE_PENDING_ORDER_ID,
    };

    let server = FakeCtrader::start_reconnect_with_reconcile();
    let config = ConnectionConfig::for_test(server.addr(), "cid", "secret", "token");
    let (events, mut ingest) = event_channel(64);
    let handle = connect_and_auth_execution(config, Arc::new(NoopSink), events.clone())
        .expect("initial handshake ok");
    let exec =
        test_support::execution_with_no_halt(handle, events, &std::env::temp_dir(), module_path!());

    // The first connection is dropped right after the handshake; the actor backs off, reconnects,
    // and issues the F3 reconcile — which the fake answers with one pending order.
    server.wait_until_count_at_least("RECONCILE_REQ", 1, Duration::from_secs(10));

    // (2) The order client id→orderId map is rebuilt from the reconcile response (written on the actor thread
    // right after RECONCILE_RES is read — poll rather than sleep a fixed duration).
    let deadline = Instant::now() + Duration::from_secs(5);
    while exec.venue_order_id(RECONCILE_PENDING_ORDER_CLIENT_ID).is_none()
        && Instant::now() < deadline
    {
        std::thread::sleep(Duration::from_millis(20));
    }
    assert_eq!(
        exec.venue_order_id(RECONCILE_PENDING_ORDER_CLIENT_ID),
        Some(RECONCILE_PENDING_ORDER_ID),
        "reconcile must rebuild the order client id→orderId map so cancel/modify resolve after a reconnect"
    );

    // A subsequent cancel(order client id) now resolves the venue orderId and sends a real CancelOrderReq
    // (rather than the NON-terminal OrderCancelRejected an unresolved order client id would produce).
    let mut exec = exec;
    exec.cancel(RECONCILE_PENDING_ORDER_CLIENT_ID);
    server.wait_until_saw(&["CANCEL_ORDER_REQ"], Duration::from_secs(5));

    // (3) The reconcile re-emitted an idempotent OrderAccepted (NOT a fill) for the pending order.
    let mut got_accept = false;
    let deadline = Instant::now() + Duration::from_secs(2);
    while !got_accept && Instant::now() < deadline {
        while let Ok(ing) = ingest.try_recv() {
            if let Ingest::Event(Event::OrderAccepted(a)) = ing
                && a.order_client_id == RECONCILE_PENDING_ORDER_CLIENT_ID
            {
                assert_eq!(
                    a.venue_order_id.as_deref(),
                    Some(RECONCILE_PENDING_ORDER_ID.to_string().as_str())
                );
                got_accept = true;
            }
        }
        if !got_accept {
            std::thread::sleep(Duration::from_millis(20));
        }
    }
    assert!(got_accept, "reconcile should re-emit an OrderAccepted for the still-pending order");

    drop(exec); // drops the ActorHandle → Shutdown + join
}

/// A RESTARTED process connects for the FIRST time while the venue still holds the orders the last
/// process rested (a bracket's stop-loss and take-profit stay across a restart). The connect-time
/// seed already reads the venue's `RECONCILE_RES`; it must also fill the order client id→orderId map from its
/// pending orders, or `cancel(order client id)` answers "no venue orderId" for an order that plainly rests. The
/// seed re-emits NO `OrderAccepted` (the reconnect path owns that: it has a before state). The
/// default fake answers EVERY `RECONCILE_REQ` with one pending order, so this is the restart case.
#[test]
fn the_connect_seed_fills_the_order_client_id_order_map_so_a_resting_order_cancels_after_a_restart()
{
    use vike_ctrader::transport::connection::connect_and_auth_execution;
    use vike_execution::{ExecutionClient, event_channel};
    use vike_model::events::Event;

    use vike_ctrader::test_support::{
        RECONCILE_PENDING_ORDER_CLIENT_ID, RECONCILE_PENDING_ORDER_ID,
    };

    let server = FakeCtrader::start_scripted();
    let config = ConnectionConfig::for_test(server.addr(), "cid", "secret", "token");
    let (events, mut ingest) = event_channel(64);
    let handle = connect_and_auth_execution(config, Arc::new(NoopSink), events.clone())
        .expect("initial handshake ok");
    let mut exec =
        test_support::execution_with_no_halt(handle, events, &std::env::temp_dir(), module_path!());

    // The seed runs BEFORE `connect_and_auth_execution` returns, so no polling: the map is filled or not.
    assert_eq!(
        exec.venue_order_id(RECONCILE_PENDING_ORDER_CLIENT_ID),
        Some(RECONCILE_PENDING_ORDER_ID),
        "the connect seed must fill the order client id→orderId map from the venue's pending orders"
    );

    exec.cancel(RECONCILE_PENDING_ORDER_CLIENT_ID);
    server.wait_until_saw(&["CANCEL_ORDER_REQ"], Duration::from_secs(5));

    let seen: Vec<Event> = vike_ctrader::drain!(ingest);
    assert!(
        !seen.iter().any(|e| matches!(e, Event::OrderCancelRejected(_))),
        "the cancel of a resting order must reach the venue, not be refused locally: {seen:?}"
    );
    assert!(
        !seen.iter().any(|e| matches!(e, Event::OrderAccepted(_))),
        "the connect seed must not re-emit OrderAccepted: {seen:?}"
    );

    drop(exec);
}

/// The `ctidTraderAccountId` guard of the same seed: an answer that is about ANOTHER account (here
/// carrying a pending order under `RECONCILE_PENDING_ORDER_CLIENT_ID`) teaches this connection NOTHING. An order client id
/// it learned from a stranger's book would let `cancel` send the stranger's venue order id on our
/// socket, and an order we never placed would look like one we hold.
#[test]
fn the_connect_seed_learns_no_order_from_another_accounts_answer() {
    use vike_ctrader::transport::connection::connect_and_auth_execution;
    use vike_execution::{ExecutionClient, event_channel};
    use vike_model::events::Event;

    use vike_ctrader::test_support::RECONCILE_PENDING_ORDER_CLIENT_ID;

    let server = FakeCtrader::start_close_preseeded_reconcile_for_another_account(&[]);
    let config = ConnectionConfig::for_test(server.addr(), "cid", "secret", "token");
    let (events, mut ingest) = event_channel(64);
    let handle = connect_and_auth_execution(config, Arc::new(NoopSink), events.clone())
        .expect("initial handshake ok");
    let mut exec =
        test_support::execution_with_no_halt(handle, events, &std::env::temp_dir(), module_path!());

    // The seed has run (and been answered, for the wrong account) before the connect returned.
    assert!(server.count_seen("RECONCILE_REQ") >= 1, "the seed must have asked the venue");
    assert_eq!(
        exec.venue_order_id(RECONCILE_PENDING_ORDER_CLIENT_ID),
        None,
        "an answer for ANOTHER account must teach the order map nothing"
    );

    exec.cancel(RECONCILE_PENDING_ORDER_CLIENT_ID);
    let seen: Vec<Event> = vike_ctrader::drain!(ingest);
    assert!(
        seen.iter().any(|e| matches!(e, Event::OrderCancelRejected(_))),
        "a cancel of an order client id only a stranger holds is refused locally: {seen:?}"
    );
    assert_eq!(server.count_seen("CANCEL_ORDER_REQ"), 0, "and nothing reached the venue");

    drop(exec);
}

/// F3 fix 1: cTrader pushes `EXECUTION_EVENT`s (fills/accepts/cancels) UNCONDITIONALLY once
/// authorized — NOT subscription-gated — so a real fill can arrive during the ≤5s reconcile window
/// that `read_reconcile_res` blocks in. The old code did `Ok(Some(_)) => continue`, silently
/// DROPPING every non-reconcile frame → `Account`/the OMS would diverge ("no order silently
/// vanishes" violated). The fake server here answers the reconnect's `RECONCILE_REQ` with an
/// in-flight `ORDER_FILLED` (a full fill for [`INFLIGHT_FILL_ORDER_CLIENT_ID`]) sent BEFORE the
/// `RECONCILE_RES`; this asserts that fill is BUFFERED and replayed through the normal inbound path
/// — reaching the ingest lane as an `Event::OrderFilled` — rather than dropped.
#[test]
fn execution_event_not_dropped_during_reconcile() {
    use std::time::Instant;

    use vike_ctrader::transport::connection::connect_and_auth_execution;
    use vike_execution::Ingest;
    use vike_execution::event_channel;
    use vike_model::events::Event;

    use vike_ctrader::test_support::{INFLIGHT_FILL_ORDER_CLIENT_ID, INFLIGHT_FILL_PRICE};

    let server = FakeCtrader::start_reconnect_with_inflight_fill();
    let config = ConnectionConfig::for_test(server.addr(), "cid", "secret", "token");
    let (events, mut ingest) = event_channel(64);
    let handle = connect_and_auth_execution(config, Arc::new(NoopSink), events)
        .expect("initial handshake ok");

    // The first connection drops after the handshake; the actor reconnects and issues the reconcile.
    server.wait_until_count_at_least("RECONCILE_REQ", 1, Duration::from_secs(10));

    // The in-flight ORDER_FILLED (sent BEFORE the RECONCILE_RES) must reach the ingest lane — it is
    // buffered during the reconcile window and replayed, not dropped.
    let deadline = Instant::now() + Duration::from_secs(5);
    let mut got_fill = false;
    while !got_fill && Instant::now() < deadline {
        while let Ok(ing) = ingest.try_recv() {
            if let Ingest::Event(Event::OrderFilled(f)) = ing
                && f.order_client_id == INFLIGHT_FILL_ORDER_CLIENT_ID
            {
                assert_eq!(f.fill.symbol.as_str(), "EURUSD");
                assert_eq!(f.fill.last_price, INFLIGHT_FILL_PRICE);
                assert_eq!(f.fill.last_qty, 1.0); // 100 centi / 100
                got_fill = true;
            }
        }
        if !got_fill {
            std::thread::sleep(Duration::from_millis(20));
        }
    }
    assert!(
        got_fill,
        "an EXECUTION_EVENT arriving during the reconcile window must be buffered + replayed, \
         not dropped"
    );

    handle.shutdown();
}

/// A socket that goes silent with no FIN (a blackhole) reads as a quiet market for ever, and the old actor read RUNNING until
/// the kernel gave up. Past `read_idle_after` with no frame, not even the answer to a question, it reconnects.
#[test]
fn a_blackholed_socket_is_called_dead_at_the_read_idle_deadline() {
    let server =
        FakeCtrader::start_blackhole_after(vike_ctrader::transport::proto::pt::RECONCILE_REQ);
    let mut config = ConnectionConfig::for_test(server.addr(), "cid", "secret", "token");
    config.read_idle_after = Duration::from_millis(600);
    let (events, _ingest) = vike_execution::event_channel(64);
    let handle = vike_ctrader::transport::connection::connect_and_auth_execution(
        config,
        Arc::new(NoopSink),
        events,
    )
    .expect("initial handshake ok");
    server.wait_until_count_at_least("APPLICATION_AUTH_REQ", 2, Duration::from_secs(5));
    handle.shutdown();
}

/// A live socket with nothing to say is NOT called dead: the actor asks it something before the deadline, and its answer
/// is a frame. Goes red if the question is dropped or the deadline stops resetting on a frame.
#[test]
fn a_quiet_socket_that_answers_is_left_alone() {
    let server = FakeCtrader::start_scripted();
    let mut config = ConnectionConfig::for_test(server.addr(), "cid", "secret", "token");
    config.read_idle_after = Duration::from_millis(600);
    let (events, _ingest) = vike_execution::event_channel(64);
    let handle = vike_ctrader::transport::connection::connect_and_auth_execution(
        config,
        Arc::new(NoopSink),
        events,
    )
    .expect("initial handshake ok");
    std::thread::sleep(Duration::from_secs(3));
    assert_eq!(server.count_seen("APPLICATION_AUTH_REQ"), 1, "one session, never reconnected");
    assert!(
        server.count_seen("TRADER_REQ") >= 3,
        "it was asked: {}",
        server.count_seen("TRADER_REQ")
    );
    handle.shutdown();
}

mod unheard {
    use std::sync::Arc;
    use std::time::{Duration, Instant};

    use vike_ctrader::drain;
    use vike_ctrader::execution::ExecutionClientCtrader;
    use vike_ctrader::test_support::{
        self, FakeCtrader, NoopSink, UNHEARD_DEAL_ID, UNHEARD_ORDER_ID, UnheardOrder, market,
    };
    use vike_ctrader::transport::connection::{ConnectionConfig, connect_and_auth_execution};
    use vike_execution::{ExecutionClient, Ingest, event_channel};
    use vike_model::OrderRequest;
    use vike_model::events::Event;

    type Lane = tokio::sync::mpsc::Receiver<Ingest>;

    fn start(
        seed: &[(i32, i64)],
        outcome: UnheardOrder,
    ) -> (FakeCtrader, ExecutionClientCtrader, Lane) {
        let server = FakeCtrader::start_unheard(seed, outcome);
        let config = ConnectionConfig::for_test(server.addr(), "cid", "secret", "token");
        let (events, rx) = event_channel(64);
        let handle = connect_and_auth_execution(config, Arc::new(NoopSink), events.clone())
            .expect("auth ok");
        let exec = test_support::execution_with_no_halt(
            handle,
            events,
            &std::env::temp_dir(),
            module_path!(),
        );
        (server, exec, rx)
    }

    /// The lane's events of `order_client_id`, read until `last` arrives or 10 s pass.
    fn lifecycle(rx: &mut Lane, order_client_id: &str, last: &'static str) -> Vec<&'static str> {
        let mut got: Vec<&'static str> = Vec::new();
        let deadline = Instant::now() + Duration::from_secs(10);
        while !got.contains(&last) && Instant::now() < deadline {
            got.extend(drain!(rx).iter().filter_map(|e| kind_of(e, order_client_id)));
            std::thread::sleep(Duration::from_millis(20));
        }
        got
    }

    fn kind_of(e: &Event, order_client_id: &str) -> Option<&'static str> {
        let (kind, of) = match e {
            Event::OrderSubmitted(x) => ("Submitted", &x.order_client_id),
            Event::OrderAccepted(x) => ("Accepted", &x.order_client_id),
            Event::Fill(x) => ("Fill", &x.order_client_id),
            Event::OrderFilled(x) => ("Filled", &x.order_client_id),
            Event::OrderCanceled(x) => ("Canceled", &x.order_client_id),
            Event::OrderRejected(x) => ("Rejected", &x.order_client_id),
            _ => return None,
        };
        (of == order_client_id).then_some(kind)
    }

    fn reduce(order_client_id: &str) -> OrderRequest {
        OrderRequest { reduce_only: true, ..market(order_client_id, -1, 1000.0) }
    }

    /// **The market order written, the socket dead, the venue filled it.** It is in no pending list and no event comes
    /// for it, so on the old code it stayed `Submitted` until the ladder's `OrderRejected` over a live position. Asked
    /// about, it books the venue's own fill under the venue's own deal id, which the live path and the reconcile key on.
    #[test]
    fn a_market_order_that_filled_while_nobody_listened_books_its_fill() {
        let (server, mut exec, mut rx) = start(&[], UnheardOrder::Filled);
        exec.submit(&market("unheard-1", 1, 1000.0));
        let mut got: Vec<Event> = Vec::new();
        let deadline = Instant::now() + Duration::from_secs(10);
        while !got.iter().any(|e| matches!(e, Event::OrderFilled(_))) && Instant::now() < deadline {
            got.extend(drain!(rx));
            std::thread::sleep(Duration::from_millis(20));
        }
        let kinds: Vec<_> = got.iter().filter_map(|e| kind_of(e, "unheard-1")).collect();
        assert_eq!(kinds, vec!["Submitted", "Accepted", "Fill", "Filled"], "{got:?}");
        for e in &got {
            match e {
                Event::Fill(f) => assert_eq!(f.trade_id, UNHEARD_DEAL_ID.to_string().as_str()),
                Event::OrderAccepted(a) => {
                    assert_eq!(
                        a.venue_order_id.as_deref(),
                        Some(UNHEARD_ORDER_ID.to_string().as_str())
                    );
                }
                _ => {}
            }
        }
        assert_eq!(server.count_seen("ORDER_LIST_REQ"), 1, "one look");
        drop(exec);
    }

    /// A refused order is absent from the list exactly like one the venue never received (measured on the demo), so
    /// absent is NO verdict: nothing is emitted, and the stuck-order ladder keeps the order.
    #[test]
    fn an_order_the_list_does_not_hold_gets_no_terminal() {
        let (server, mut exec, mut rx) = start(&[], UnheardOrder::NotListed);
        exec.submit(&market("unheard-2", 1, 1000.0));
        server.wait_until_count_at_least("ORDER_LIST_REQ", 1, Duration::from_secs(10));
        std::thread::sleep(Duration::from_millis(300));
        let kinds: Vec<_> = drain!(rx).iter().filter_map(|e| kind_of(e, "unheard-2")).collect();
        assert_eq!(kinds, vec!["Submitted"]);
        assert_eq!(server.count_seen("DEAL_LIST_REQ"), 0, "nothing executed, no deal to read");
        drop(exec);
    }

    /// A market order the venue cancelled while nobody listened ends as what the list says.
    #[test]
    fn an_order_the_list_holds_as_cancelled_is_cancelled() {
        let (_server, mut exec, mut rx) = start(&[], UnheardOrder::Cancelled);
        exec.submit(&market("unheard-3", 1, 1000.0));
        assert_eq!(
            lifecycle(&mut rx, "unheard-3", "Canceled"),
            vec!["Submitted", "Accepted", "Canceled"]
        );
        drop(exec);
    }

    /// The venue says FILLED and its deals cannot be read: the accept alone, because the ladder's reject would be
    /// false and the fill is the reconcile's.
    #[test]
    fn a_filled_order_whose_deals_cannot_be_read_is_accepted_and_not_rejected() {
        let (server, mut exec, mut rx) = start(&[], UnheardOrder::FilledDealsRefused);
        exec.submit(&market("unheard-4", 1, 1000.0));
        assert_eq!(lifecycle(&mut rx, "unheard-4", "Accepted"), vec!["Submitted", "Accepted"]);
        assert_eq!(server.count_seen("DEAL_LIST_REQ"), 1);
        drop(exec);
    }

    /// A close that filled while nobody listened: the position is gone, and the closing order is listed under the
    /// close's order client id. It books its fill, where the engine would have read a reject over a closed position.
    #[test]
    fn a_close_that_filled_while_nobody_listened_books_its_fill() {
        let (_server, mut exec, mut rx) = start(&[(1, 100_000)], UnheardOrder::Filled);
        exec.submit(&reduce("unheard-5"));
        assert_eq!(
            lifecycle(&mut rx, "unheard-5", "Filled"),
            vec!["Submitted", "Accepted", "Fill", "Filled"]
        );
        drop(exec);
    }
}
