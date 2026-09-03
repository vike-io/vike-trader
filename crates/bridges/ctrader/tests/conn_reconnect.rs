//! Task 6 scripted integration test: the actor thread reconnects and re-authenticates after a
//! dropped socket. `FakeCtrader::start_reconnect_after_handshake` completes the FIRST handshake
//! then closes the connection immediately (simulating a disconnect right as the actor's read loop
//! is about to start); this asserts the actor backs off, opens a SECOND TCP connection to the
//! same fake server, and replays the FULL handshake (a second `APPLICATION_AUTH_REQ`) — proving
//! `conn::reconnect_with_backoff`/`open_and_handshake` wiring end to end, not just the pure retry
//! logic in isolation.

mod common;

use std::sync::Arc;
use std::time::Duration;

use vike_ctrader::conn::{connect_and_auth, ConnConfig};
use vike_ctrader::data::CtraderData;
use vike_data::DataClient;

use common::{FakeCtrader, NoopSink};

#[test]
fn actor_reconnects_and_reauths_after_a_dropped_socket() {
    let server = FakeCtrader::start_reconnect_after_handshake();
    let cfg = ConnConfig::for_test(server.addr(), "cid", "secret", "token");

    // The initial connect completes against the FIRST served connection, which the fake server
    // drops right after finishing the handshake.
    let handle = connect_and_auth(cfg, Arc::new(NoopSink)).expect("initial handshake ok");

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
    let cfg = ConnConfig::for_test(server.addr(), "cid", "secret", "token");

    let handle = connect_and_auth(cfg, Arc::new(NoopSink)).expect("initial handshake ok");
    let mut data = CtraderData::new(handle);
    data.subscribe_quotes("EURUSD").expect("subscribe_quotes ok");

    // Confirm the subscribe actually went out on the first connection before asserting on the
    // reconnect replay, so a failure here can't be mistaken for a replay failure.
    server.wait_until_count_at_least("SUBSCRIBE_SPOTS_REQ", 1, Duration::from_secs(10));

    server.wait_until_count_at_least("APPLICATION_AUTH_REQ", 2, Duration::from_secs(10));
    server.wait_until_count_at_least("ACCOUNT_AUTH_REQ", 2, Duration::from_secs(10));
    server.wait_until_count_at_least("SUBSCRIBE_SPOTS_REQ", 2, Duration::from_secs(10));

    data.shutdown();
}

/// F3: on reconnect the actor issues a `ProtoOAReconcileReq` and rebuilds its coid→orderId
/// correlation map from the pending orders in the response. The fake server drops the first
/// connection right after the handshake, then answers the reconnect's `RECONCILE_REQ` with ONE
/// pending order (`RECONCILE_PENDING_COID` → `RECONCILE_PENDING_ORDER_ID`). Asserts: (1) the server
/// saw a `RECONCILE_REQ`; (2) the adapter's map now resolves the reconciled coid to the right venue
/// orderId, so a subsequent `cancel(coid)` sends a real `CancelOrderReq`; (3) the reconcile also
/// re-emitted an idempotent `OrderAccepted` (never a fill) for the still-pending order onto the
/// ingest lane.
#[test]
fn reconcile_rebuilds_coid_order_map_on_reconnect() {
    use std::time::Instant;

    use vike_ctrader::conn::connect_and_auth_exec;
    use vike_exec::lanes::Ingest;
    use vike_exec::{event_channel, ExecutionClient};
    use vike_model::events::Event;

    use common::{RECONCILE_PENDING_COID, RECONCILE_PENDING_ORDER_ID};

    let server = FakeCtrader::start_reconnect_with_reconcile();
    let cfg = ConnConfig::for_test(server.addr(), "cid", "secret", "token");
    let (events, mut ingest) = event_channel(64);
    let handle = connect_and_auth_exec(cfg, Arc::new(NoopSink), events.clone())
        .expect("initial handshake ok");
    let exec = common::exec_with_no_halt(handle, events, module_path!());

    // The first connection is dropped right after the handshake; the actor backs off, reconnects,
    // and issues the F3 reconcile — which the fake answers with one pending order.
    server.wait_until_count_at_least("RECONCILE_REQ", 1, Duration::from_secs(10));

    // (2) The coid→orderId map is rebuilt from the reconcile response (written on the actor thread
    // right after RECONCILE_RES is read — poll rather than sleep a fixed duration).
    let deadline = Instant::now() + Duration::from_secs(5);
    while exec.venue_order_id(RECONCILE_PENDING_COID).is_none() && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(20));
    }
    assert_eq!(
        exec.venue_order_id(RECONCILE_PENDING_COID),
        Some(RECONCILE_PENDING_ORDER_ID),
        "reconcile must rebuild the coid→orderId map so cancel/modify resolve after a reconnect"
    );

    // A subsequent cancel(coid) now resolves the venue orderId and sends a real CancelOrderReq
    // (rather than the NON-terminal OrderCancelRejected an unresolved coid would produce).
    let mut exec = exec;
    exec.cancel(RECONCILE_PENDING_COID);
    server.wait_until_saw(&["CANCEL_ORDER_REQ"], Duration::from_secs(5));

    // (3) The reconcile re-emitted an idempotent OrderAccepted (NOT a fill) for the pending order.
    let mut got_accept = false;
    let deadline = Instant::now() + Duration::from_secs(2);
    while !got_accept && Instant::now() < deadline {
        while let Ok(ing) = ingest.try_recv() {
            if let Ingest::Event(Event::OrderAccepted(a)) = ing {
                if a.client_order_id == RECONCILE_PENDING_COID {
                    assert_eq!(
                        a.venue_order_id.as_deref(),
                        Some(RECONCILE_PENDING_ORDER_ID.to_string().as_str())
                    );
                    got_accept = true;
                }
            }
        }
        if !got_accept {
            std::thread::sleep(Duration::from_millis(20));
        }
    }
    assert!(got_accept, "reconcile should re-emit an OrderAccepted for the still-pending order");

    drop(exec); // drops the ActorHandle → Shutdown + join
}

/// F3 fix 1: cTrader pushes `EXECUTION_EVENT`s (fills/accepts/cancels) UNCONDITIONALLY once
/// authorized — NOT subscription-gated — so a real fill can arrive during the ≤5s reconcile window
/// that `read_reconcile_res` blocks in. The old code did `Ok(Some(_)) => continue`, silently
/// DROPPING every non-reconcile frame → `Account`/the OMS would diverge ("no order silently
/// vanishes" violated). The fake server here answers the reconnect's `RECONCILE_REQ` with an
/// in-flight `ORDER_FILLED` (a full fill for [`INFLIGHT_FILL_COID`]) sent BEFORE the
/// `RECONCILE_RES`; this asserts that fill is BUFFERED and replayed through the normal inbound path
/// — reaching the ingest lane as an `Event::OrderFilled` — rather than dropped.
#[test]
fn exec_event_not_dropped_during_reconcile() {
    use std::time::Instant;

    use vike_ctrader::conn::connect_and_auth_exec;
    use vike_exec::event_channel;
    use vike_exec::lanes::Ingest;
    use vike_model::events::Event;

    use common::{INFLIGHT_FILL_COID, INFLIGHT_FILL_PRICE};

    let server = FakeCtrader::start_reconnect_with_inflight_fill();
    let cfg = ConnConfig::for_test(server.addr(), "cid", "secret", "token");
    let (events, mut ingest) = event_channel(64);
    let handle =
        connect_and_auth_exec(cfg, Arc::new(NoopSink), events).expect("initial handshake ok");

    // The first connection drops after the handshake; the actor reconnects and issues the reconcile.
    server.wait_until_count_at_least("RECONCILE_REQ", 1, Duration::from_secs(10));

    // The in-flight ORDER_FILLED (sent BEFORE the RECONCILE_RES) must reach the ingest lane — it is
    // buffered during the reconcile window and replayed, not dropped.
    let deadline = Instant::now() + Duration::from_secs(5);
    let mut got_fill = false;
    while !got_fill && Instant::now() < deadline {
        while let Ok(ing) = ingest.try_recv() {
            if let Ingest::Event(Event::OrderFilled(f)) = ing {
                if f.client_order_id == INFLIGHT_FILL_COID {
                    assert_eq!(f.fill.symbol.as_str(), "EURUSD");
                    assert_eq!(f.fill.last_px, INFLIGHT_FILL_PRICE);
                    assert_eq!(f.fill.last_qty, 1.0); // 100 centi / 100
                    got_fill = true;
                }
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
