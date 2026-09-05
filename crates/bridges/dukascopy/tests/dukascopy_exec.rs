//! DukascopyExecutionClient integration tests against the scripted fake bridge
//! (src/bin/fake_jforex_bridge.rs) — no Java, no network. The fake bridge speaks the
//! real stdio protocol, so these tests cover: handshake (ready/fatal/silent-death),
//! the Rust-emitted OrderSubmitted, venue-derived Accepted/Filled/Canceled flowing
//! through the ingest lane, dead-child synthetic rejection, and shutdown/reap.

use std::time::Duration;

use vike_dukascopy::{DukascopyConfig, DukascopyExecutionClient};
use vike_exec::{ExecutionClient, Ingest, event_channel};
use vike_model::OrderRequest;
use vike_model::events::Event;

/// Path of the fake bridge binary (cargo builds crate bins for integration tests).
const BRIDGE: &str = env!("CARGO_BIN_EXE_fake_jforex_bridge");

fn config() -> DukascopyConfig {
    DukascopyConfig {
        login: "test-login".into(),
        password: "test-password".into(),
        server: String::new(),
    }
}

fn order(coid: &str) -> OrderRequest {
    OrderRequest {
        combo_legs: Vec::new(),
        client_order_id: coid.into(),
        venue: "dukascopy".into(),
        symbol: "EURUSD".into(),
        side: 1,
        qty: 1000.0,
        order_type: "market".into(),
        price: None,
        trigger_price: None,
        reduce_only: false,
        time_in_force: Default::default(),
        gtd_expiry: None,
        ts: 1,
        parent_order_id: None,
        linked_order_ids: vec![],
        order_list_id: None,
        contingency_type: None,
        weight: 0.0,
        stop: None,
        trail: None,
        extreme: None,
        on_close: false,
        margin_mode: None,
        trigger_by: None,
    }
}

/// Receive the next Event from the ingest lane, failing loudly after 10s.
fn recv_event(rx: &mut tokio::sync::mpsc::Receiver<Ingest>) -> Event {
    let rt = tokio::runtime::Builder::new_current_thread().enable_time().build().unwrap();
    let ingest = rt
        .block_on(async { tokio::time::timeout(Duration::from_secs(10), rx.recv()).await })
        .expect("timed out waiting for event")
        .expect("ingest channel closed");
    match ingest {
        Ingest::Event(ev) => ev,
        other => panic!("expected Ingest::Event, got {other:?}"),
    }
}

#[test]
fn ladder_flows_submitted_accepted_filled_canceled() {
    let (events, mut rx) = event_channel(64);
    let mut client = DukascopyExecutionClient::spawn_with_program(BRIDGE, &[], &config(), events)
        .expect("ready");

    client.submit(&order("c1"));

    // Rust-side synchronous OrderSubmitted first…
    match recv_event(&mut rx) {
        Event::OrderSubmitted(e) => assert_eq!(e.client_order_id, "c1"),
        other => panic!("expected OrderSubmitted, got {other:?}"),
    }
    // …then the venue-derived lifecycle from the (fake) sidecar.
    match recv_event(&mut rx) {
        Event::OrderAccepted(e) => {
            assert_eq!(e.client_order_id, "c1");
            assert!(e.venue_order_id.is_some());
        }
        other => panic!("expected OrderAccepted, got {other:?}"),
    }
    // Dual-publish contract: the bare FillEvent (which the core Account folds into
    // position/PnL) arrives FIRST, then the OrderFilled wrap (which the FSM applies).
    match recv_event(&mut rx) {
        Event::Fill(fill) => {
            assert_eq!(fill.client_order_id, "c1");
            assert_eq!(fill.venue, "dukascopy"); // core routes on this exact string
            assert_eq!(fill.symbol, "EURUSD"); // canonical, not EUR/USD
            assert_eq!(fill.side, 1);
            assert_eq!(fill.last_qty, 1000.0); // units, not JForex millions
        }
        other => panic!("expected bare Fill first, got {other:?}"),
    }
    match recv_event(&mut rx) {
        Event::OrderFilled(e) => {
            assert_eq!(e.client_order_id, "c1");
            assert_eq!(e.fill.venue, "dukascopy");
            assert_eq!(e.fill.symbol, "EURUSD");
            assert_eq!(e.fill.side, 1);
            assert_eq!(e.fill.last_qty, 1000.0);
        }
        other => panic!("expected OrderFilled wrap, got {other:?}"),
    }

    client.cancel("c1");
    match recv_event(&mut rx) {
        Event::OrderCanceled(e) => assert_eq!(e.client_order_id, "c1"),
        other => panic!("expected OrderCanceled, got {other:?}"),
    }

    client.detach(); // shutdown + reap; must not hang or panic
}

#[test]
fn fatal_handshake_is_unavailable() {
    let (events, _rx) = event_channel(8);
    let err =
        DukascopyExecutionClient::spawn_with_program(BRIDGE, &["fatal".into()], &config(), events)
            .err()
            .expect("fatal handshake must fail spawn");
    assert_eq!(format!("{err:?}"), "Unavailable");
}

#[test]
fn silent_child_death_is_unavailable() {
    let (events, _rx) = event_channel(8);
    // "silent" exits without printing anything: reader hits EOF, handshake channel
    // disconnects, spawn returns Unavailable immediately (not after the 300s timeout).
    let err =
        DukascopyExecutionClient::spawn_with_program(BRIDGE, &["silent".into()], &config(), events)
            .err()
            .expect("silent death must fail spawn");
    assert_eq!(format!("{err:?}"), "Unavailable");
}

#[test]
fn dead_child_synthesizes_order_rejected() {
    let (events, mut rx) = event_channel(64);
    let mut client = DukascopyExecutionClient::spawn_with_program(
        BRIDGE,
        &["ready-die".into()],
        &config(),
        events,
    )
    .expect("ready-die mode still handshakes ready");

    // The child exits right after `ready`. A submit is terminated by EITHER path:
    // the failed stdin write ("bridge unavailable") or the reader's EOF drain of
    // in-flight coids ("bridge died"). One write can still slip into the pipe buffer
    // after the reader already drained, so keep a modest retry for scheduling slack.
    let mut saw_rejected = false;
    'outer: for i in 0..10 {
        client.submit(&order(&format!("c{i}")));
        std::thread::sleep(Duration::from_millis(20));
        // Drain whatever arrived; stop at the first OrderRejected.
        loop {
            match try_recv_event(&mut rx) {
                Some(Event::OrderRejected(e)) => {
                    assert!(
                        e.reason == "bridge unavailable" || e.reason == "bridge died",
                        "unexpected rejection reason: {}",
                        e.reason
                    );
                    saw_rejected = true;
                    break 'outer;
                }
                Some(_) => continue, // OrderSubmitted etc.
                None => break,
            }
        }
    }
    assert!(saw_rejected, "dead child never produced a synthetic OrderRejected");
}

#[test]
fn pre_ready_ghost_events_are_dropped() {
    let (events, mut rx) = event_channel(64);
    // "ghost" emits an event envelope BEFORE `ready` — a protocol violation the
    // reader must drop (never pump into ingest), while the handshake still succeeds.
    let mut client =
        DukascopyExecutionClient::spawn_with_program(BRIDGE, &["ghost".into()], &config(), events)
            .expect("ghost mode still handshakes ready");

    client.submit(&order("c1"));
    match recv_event(&mut rx) {
        Event::OrderSubmitted(e) => assert_eq!(e.client_order_id, "c1"),
        other => panic!("expected OrderSubmitted, got {other:?}"),
    }
    // The very next event must be c1's Accepted — NOT the pre-ready ghost event.
    match recv_event(&mut rx) {
        Event::OrderAccepted(e) => assert_eq!(e.client_order_id, "c1"),
        other => panic!("ghost event leaked into ingest: {other:?}"),
    }
    client.detach();
}

#[test]
fn reader_eof_rejects_accepted_but_unterminated_order() {
    let (events, mut rx) = event_channel(64);
    // "accept-die": the submit is Accepted (non-terminal) and the child then dies —
    // the reader's EOF drain must synthesize the terminal rejection for it.
    let mut client = DukascopyExecutionClient::spawn_with_program(
        BRIDGE,
        &["accept-die".into()],
        &config(),
        events,
    )
    .expect("accept-die mode still handshakes ready");

    client.submit(&order("c1"));
    match recv_event(&mut rx) {
        Event::OrderSubmitted(e) => assert_eq!(e.client_order_id, "c1"),
        other => panic!("expected OrderSubmitted, got {other:?}"),
    }
    match recv_event(&mut rx) {
        Event::OrderAccepted(e) => assert_eq!(e.client_order_id, "c1"),
        other => panic!("expected OrderAccepted, got {other:?}"),
    }
    match recv_event(&mut rx) {
        Event::OrderRejected(e) => {
            assert_eq!(e.client_order_id, "c1");
            assert_eq!(e.reason, "bridge died");
        }
        other => panic!("expected synthetic OrderRejected from EOF drain, got {other:?}"),
    }
    client.detach();
}

/// Non-blocking receive helper for the dead-child polling loop.
fn try_recv_event(rx: &mut tokio::sync::mpsc::Receiver<Ingest>) -> Option<Event> {
    match rx.try_recv() {
        Ok(Ingest::Event(ev)) => Some(ev),
        Ok(other) => panic!("expected Ingest::Event, got {other:?}"),
        Err(_) => None,
    }
}
