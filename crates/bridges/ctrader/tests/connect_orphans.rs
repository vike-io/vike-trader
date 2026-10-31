//! The connect-time look at resting exits that OUTLIVED their position (0.1.97): a bound exit whose position
//! closed while no bridge was watching is dangerous (the venue does not cancel it, and one that reaches the market
//! OPENS a position under the reused position id), and the reconcile answer cannot tell it from a plain pending
//! order (both name a position the answer does not list as open). The exec start therefore asks the venue for the
//! orders of that position (`ProtoOAOrderListByPositionIdReq`): a FILLED order other than the exit itself proves
//! the position existed and is gone, and the exit is flagged (`PositionBook::orphaned_exits`, an `error` line) and
//! NEVER cancelled by the bridge. Anything the look cannot prove leaves the order as it was.
//!
//! Against the stateful fake venue (`FakeCtrader::start_resting_exit`): no position open, one resting STOP naming
//! `GONE_POSITION_ID`, and an `ExitLookup` that scripts the venue's answer for that position.

mod common;

use std::sync::Arc;
use std::time::Duration;

use vike_ctrader::conn::{ActorHandle, ConnConfig, connect_and_auth_exec};
use vike_ctrader::positions::{ExitKind, OrphanedExit};
use vike_exec::{Ingest, event_channel};

use common::{
    ExitLookup, FakeCtrader, GONE_POSITION_ID, NoopSink, RESTING_EXIT_COID, RESTING_EXIT_ORDER_ID,
};

/// An exec start against `server`; the receiver is returned so the ingest lane stays open for the test's life.
fn start(server: &FakeCtrader) -> (ActorHandle, tokio::sync::mpsc::Receiver<Ingest>) {
    let cfg = ConnConfig::for_test(server.addr(), "cid", "secret", "token");
    let (events, rx) = event_channel(64);
    let handle = connect_and_auth_exec(cfg, Arc::new(NoopSink), events).expect("auth ok");
    (handle, rx)
}

fn orphans(handle: &ActorHandle) -> Vec<OrphanedExit> {
    handle.positions.lock().expect("book lock").orphaned_exits().to_vec()
}

/// **The point.** The position's orders include a filled entry and a filled closing order: the exit outlived it.
/// It is flagged with everything an operator needs to cancel it by hand, and the bridge cancels NOTHING (the actor
/// is given a second of loops to prove it).
#[test]
fn an_exit_that_outlived_its_position_is_flagged_and_never_cancelled() {
    let server = FakeCtrader::start_resting_exit(ExitLookup::Outlived);
    let (handle, _rx) = start(&server);

    let found = orphans(&handle);
    let [exit] = &found[..] else { panic!("exactly one orphaned exit expected, got {found:?}") };
    assert_eq!(exit.order_id, RESTING_EXIT_ORDER_ID);
    assert_eq!(exit.position_id, GONE_POSITION_ID);
    assert_eq!(exit.coid.as_deref(), Some(RESTING_EXIT_COID));
    assert_eq!((exit.kind, exit.side, exit.volume), (ExitKind::Stop, -1, 100_000));
    assert_eq!(exit.price, Some(1.05));
    assert_eq!(server.count_seen("ORDER_LIST_BY_POSITION_ID_REQ"), 1, "one look-up, unbounded");

    std::thread::sleep(Duration::from_millis(1500));
    assert_eq!(
        server.count_seen("CANCEL_ORDER_REQ"),
        0,
        "the bridge must never cancel it by itself"
    );
    assert_eq!(server.count_seen("NEW_ORDER_REQ"), 0);
    handle.shutdown();
}

/// A plain pending order's own empty position never had a fill: its list is the pending order alone. Not flagged.
#[test]
fn a_plain_pending_order_is_left_alone() {
    let server = FakeCtrader::start_resting_exit(ExitLookup::PlainPending);
    let (handle, _rx) = start(&server);
    assert!(
        orphans(&handle).is_empty(),
        "a plain pending order was flagged: {:?}",
        orphans(&handle)
    );
    assert_eq!(server.count_seen("ORDER_LIST_BY_POSITION_ID_REQ"), 1);
    handle.shutdown();
}

/// A venue that refuses the look-up in BOTH forms (no bounds, then a one-week window) proves nothing: nothing is
/// flagged, and nothing is cancelled either.
#[test]
fn a_refused_look_up_proves_nothing() {
    let server = FakeCtrader::start_resting_exit(ExitLookup::Refused);
    let (handle, _rx) = start(&server);
    assert!(orphans(&handle).is_empty());
    assert_eq!(
        server.count_seen("ORDER_LIST_BY_POSITION_ID_REQ"),
        2,
        "the unbounded form, then the one-week window"
    );
    assert_eq!(server.count_seen("CANCEL_ORDER_REQ"), 0);
    handle.shutdown();
}

/// A venue that refuses the unbounded form and answers the windowed one is read through the second form.
#[test]
fn the_windowed_form_is_tried_when_the_unbounded_one_is_refused() {
    let server = FakeCtrader::start_resting_exit(ExitLookup::RefusesTheUnboundedForm);
    let (handle, _rx) = start(&server);
    assert_eq!(orphans(&handle).len(), 1);
    assert_eq!(server.count_seen("ORDER_LIST_BY_POSITION_ID_REQ"), 2);
    handle.shutdown();
}

/// A truncated list (`hasMore`) that carries no fill proves nothing either way: not flagged.
#[test]
fn a_truncated_list_without_a_fill_is_not_a_proof() {
    let server = FakeCtrader::start_resting_exit(ExitLookup::Truncated);
    let (handle, _rx) = start(&server);
    assert!(orphans(&handle).is_empty());
    handle.shutdown();
}
