//! The socket backend over a DOUBLE of the ibapi session ([`IbSession`]): the production connect sequence
//! (`SocketTransport::start`), the pump and the snapshot end markers, with no gateway. What these cannot
//! show is ibapi itself — that is the attended run against a real gateway.

use std::sync::mpsc::{Receiver, Sender, channel};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use ibapi::orders::{OrderData, OrderState, OrderStatusKind};

use super::*;
use crate::transport::OwnOrders;

enum Feed {
    Update(Box<OrderUpdate>),
    Fail(String),
    Close,
}

/// An order-update stream the test feeds; each item takes `delay` to deliver (a pump busier than the
/// test, so an end marker can be ready while rows are still queued).
struct FakeUpdates {
    rx: Receiver<Feed>,
    delay: Duration,
}

impl Updates for FakeUpdates {
    fn poll(&mut self, within: Duration) -> Polled {
        match self.rx.recv_timeout(within) {
            Ok(Feed::Update(u)) => {
                std::thread::sleep(self.delay);
                Polled::Update(u)
            }
            Ok(Feed::Fail(e)) => Polled::Failed(e),
            Ok(Feed::Close) | Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
                Polled::Closed
            }
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => Polled::Idle,
        }
    }
}

/// A snapshot the test ends by sending on the channel (`true` = the venue ended it).
struct FakeHandle(Receiver<bool>);

impl SnapshotHandle for FakeHandle {
    fn wait(self: Box<Self>, within: Duration) -> bool {
        self.0.recv_timeout(within).unwrap_or(false)
    }
}

#[derive(Default)]
struct Calls {
    executions_for: Vec<i32>,
    open_order_requests: u32,
}

struct FakeSession {
    next_valid: Result<i32, String>,
    local: i32,
    updates: Mutex<Option<Box<dyn Updates>>>,
    open_orders: Mutex<Option<Result<Box<dyn SnapshotHandle>, String>>>,
    executions: Mutex<Option<Result<Box<dyn SnapshotHandle>, String>>>,
    calls: Mutex<Calls>,
}

impl FakeSession {
    fn new(next_valid: Result<i32, String>) -> Self {
        Self {
            next_valid,
            local: 101,
            updates: Mutex::new(None),
            open_orders: Mutex::new(None),
            executions: Mutex::new(None),
            calls: Mutex::new(Calls::default()),
        }
    }
}

impl IbSession for FakeSession {
    fn local_next_order_id(&self) -> i32 {
        self.local
    }
    fn next_valid_order_id(&self) -> Result<i32, String> {
        self.next_valid.clone()
    }
    fn submit_order(&self, _: i32, _: &Contract, _: &Order) -> Result<(), String> {
        Ok(())
    }
    fn cancel_order(&self, _: i32) -> Result<(), String> {
        Ok(())
    }
    fn all_open_orders(&self) -> Result<Box<dyn SnapshotHandle>, String> {
        self.calls.lock().unwrap().open_order_requests += 1;
        self.open_orders.lock().unwrap().take().unwrap_or_else(|| Err("not scripted".into()))
    }
    fn executions(&self, client_id: i32) -> Result<Box<dyn SnapshotHandle>, String> {
        self.calls.lock().unwrap().executions_for.push(client_id);
        self.executions.lock().unwrap().take().unwrap_or_else(|| Err("not scripted".into()))
    }
    fn order_updates(&self) -> Result<Box<dyn Updates>, String> {
        self.updates.lock().unwrap().take().ok_or_else(|| "no stream".to_string())
    }
}

fn feed(delay: Duration) -> (Sender<Feed>, Box<dyn Updates>) {
    let (tx, rx) = channel();
    (tx, Box::new(FakeUpdates { rx, delay }))
}

fn snapshot() -> (Sender<bool>, Box<dyn SnapshotHandle>) {
    let (tx, rx) = channel();
    (tx, Box::new(FakeHandle(rx)))
}

fn open_order(order_id: i32, order_ref: &str) -> Box<OrderUpdate> {
    Box::new(OrderUpdate::OpenOrder(OrderData {
        order_id,
        order: Order {
            client_id: 7,
            account: "DU1234567".into(),
            order_ref: order_ref.into(),
            total_quantity: 10.0,
            ..Order::default()
        },
        order_state: OrderState { status: OrderStatusKind::Submitted, ..OrderState::default() },
        ..OrderData::default()
    }))
}

fn start(session: Arc<FakeSession>) -> SocketTransport {
    SocketTransport::start(session, 7, "DU1234567").expect("the transport starts")
}

fn next(t: &mut SocketTransport) -> IbInbound {
    let deadline = Instant::now() + Duration::from_secs(3);
    while Instant::now() < deadline {
        if let Some(i) = t.next_recv(Duration::from_millis(20)) {
            return i;
        }
    }
    panic!("no inbound within 3s");
}

/// **The production connect call site.** `SocketTransport::start` is what `connect` runs after the
/// ibapi handshake: the seed it queues is what makes the exec loop learn the venue. A revert to the old
/// two-item seed fails here, which the tests of `socket_connect_seed` alone could not catch.
#[test]
fn the_connect_sequence_queues_ib_next_valid_id_the_readiness_and_the_order_to_learn_the_venue() {
    let session = Arc::new(FakeSession::new(Ok(4242)));
    let (_feed, updates) = feed(Duration::ZERO);
    *session.updates.lock().unwrap() = Some(updates);
    let mut t = start(session);
    assert!(matches!(next(&mut t), IbInbound::NextValidId(4242)));
    assert!(matches!(next(&mut t), IbInbound::AccountsReady));
    assert!(matches!(next(&mut t), IbInbound::Connected), "the third item makes the loop learn");
}

#[test]
fn an_unreadable_next_valid_id_falls_back_to_the_local_floor() {
    let session = Arc::new(FakeSession::new(Err("timeout".into())));
    let (_feed, updates) = feed(Duration::ZERO);
    *session.updates.lock().unwrap() = Some(updates);
    let mut t = start(session);
    assert!(matches!(next(&mut t), IbInbound::NextValidId(101)));
}

#[test]
fn the_transport_says_whose_orders_are_its_own() {
    let session = Arc::new(FakeSession::new(Ok(1)));
    let (_feed, updates) = feed(Duration::ZERO);
    *session.updates.lock().unwrap() = Some(updates);
    let t = start(session);
    assert_eq!(
        t.ownership(),
        Some(OwnOrders { client_id: 7, account: "DU1234567".to_string() }),
        "the client id and the account it was started with"
    );
}

/// An `openOrder` is translated with the identity of its owner and its quantity: the exec loop needs
/// both to tell an order of this client from a stranger's and to seed the order's fill tracking.
#[test]
fn an_open_order_row_carries_its_owner_its_account_its_quantity_and_its_status() {
    let Some(IbInbound::VenueOrder(row)) = translate(*open_order(55, "coid-1"), &own()) else {
        panic!("an OpenOrder translates to a VenueOrder row");
    };
    assert_eq!(row.order_id, 55);
    assert_eq!(row.order_ref, "coid-1");
    assert_eq!(row.client_id, 7);
    assert_eq!(row.account, "DU1234567");
    assert_eq!(row.total_qty, 10.0);
    assert_eq!(row.status, "Submitted");
}

/// The end of a snapshot travels a different queue than its rows. It is released into the channel by
/// the pump, behind every row that was already on the stream, even when the request ended while the
/// pump was still working through them.
#[test]
fn the_end_of_a_snapshot_is_released_behind_every_row_of_it() {
    let session = Arc::new(FakeSession::new(Ok(1)));
    let (feed_tx, updates) = feed(Duration::from_millis(40)); // a slow pump
    *session.updates.lock().unwrap() = Some(updates);
    let (end_tx, handle) = snapshot();
    *session.open_orders.lock().unwrap() = Some(Ok(handle));
    let mut t = start(session);
    for _ in 0..3 {
        next(&mut t); // the seed
    }
    for id in [11, 12, 13] {
        feed_tx.send(Feed::Update(open_order(id, &format!("coid-{id}")))).unwrap();
    }
    t.request_open_orders();
    end_tx.send(true).unwrap(); // the venue ended it while the pump is still on row 1 or 2

    let mut order = Vec::new();
    for _ in 0..4 {
        order.push(match next(&mut t) {
            IbInbound::VenueOrder(r) => format!("row {}", r.order_id),
            IbInbound::SnapshotEnd { snapshot: Snapshot::OpenOrders, complete: true } => {
                "end".to_string()
            }
            _ => "other".to_string(),
        });
    }
    assert_eq!(order, ["row 11", "row 12", "row 13", "end"], "the marker comes last");
}

/// A request that cannot even be written is an incomplete snapshot, said at once.
#[test]
fn a_request_that_cannot_be_written_is_an_incomplete_snapshot() {
    let session = Arc::new(FakeSession::new(Ok(1)));
    let (_feed, updates) = feed(Duration::ZERO);
    *session.updates.lock().unwrap() = Some(updates);
    *session.open_orders.lock().unwrap() = Some(Err("connection reset".into()));
    let mut t = start(session);
    for _ in 0..3 {
        next(&mut t);
    }
    t.request_open_orders();
    assert!(matches!(
        next(&mut t),
        IbInbound::SnapshotEnd { snapshot: Snapshot::OpenOrders, complete: false }
    ));
}

/// A request the venue never ends is reported incomplete after its bound, not waited on for ever.
#[test]
fn a_request_the_venue_never_ends_is_reported_incomplete_after_its_bound() {
    let session = Arc::new(FakeSession::new(Ok(1)));
    let (_feed, updates) = feed(Duration::ZERO);
    *session.updates.lock().unwrap() = Some(updates);
    let (_never, handle) = snapshot();
    *session.executions.lock().unwrap() = Some(Ok(handle));
    let mut t = start(session);
    for _ in 0..3 {
        next(&mut t);
    }
    t.snapshot_deadline = Duration::from_millis(150);
    t.request_executions();
    assert!(matches!(
        next(&mut t),
        IbInbound::SnapshotEnd { snapshot: Snapshot::Executions, complete: false }
    ));
}

#[test]
fn the_executions_request_is_scoped_to_this_client_id() {
    let session = Arc::new(FakeSession::new(Ok(1)));
    let (_feed, updates) = feed(Duration::ZERO);
    *session.updates.lock().unwrap() = Some(updates);
    let (end_tx, handle) = snapshot();
    *session.executions.lock().unwrap() = Some(Ok(handle));
    let mut t = start(Arc::clone(&session));
    t.request_executions();
    end_tx.send(true).unwrap();
    assert_eq!(session.calls.lock().unwrap().executions_for, vec![7]);
}

/// A terminal error on the stream is `StreamDead` once, and the pump ends.
#[test]
fn a_terminal_stream_error_is_a_stream_death_and_ends_the_pump() {
    let session = Arc::new(FakeSession::new(Ok(1)));
    let (feed_tx, updates) = feed(Duration::ZERO);
    *session.updates.lock().unwrap() = Some(updates);
    let mut t = start(session);
    for _ in 0..3 {
        next(&mut t);
    }
    feed_tx.send(Feed::Fail("connection reset by peer".into())).unwrap();
    match next(&mut t) {
        IbInbound::StreamDead { reason } => assert!(reason.contains("reset"), "{reason}"),
        _ => panic!("expected the stream death"),
    }
    assert!(t.next_recv(Duration::from_millis(100)).is_none(), "the pump is done");
}

#[test]
fn a_clean_end_of_the_stream_is_a_stream_death_too() {
    let session = Arc::new(FakeSession::new(Ok(1)));
    let (feed_tx, updates) = feed(Duration::ZERO);
    *session.updates.lock().unwrap() = Some(updates);
    let mut t = start(session);
    for _ in 0..3 {
        next(&mut t);
    }
    feed_tx.send(Feed::Close).unwrap();
    assert!(matches!(next(&mut t), IbInbound::StreamDead { .. }));
}

/// An idle stream that says when it is DROPPED, as ibapi's `Subscription` drops its hold on the message
/// bus (and so on the socket) when the pump lets go of it.
struct WatchedUpdates {
    dropped: Sender<()>,
}

impl Updates for WatchedUpdates {
    fn poll(&mut self, within: Duration) -> Polled {
        std::thread::sleep(within);
        Polled::Idle
    }
}

impl Drop for WatchedUpdates {
    fn drop(&mut self) {
        let _ = self.dropped.send(());
    }
}

/// **Dropping the transport releases the stream** (and with it ibapi's message bus and the socket). The
/// pump of an IDLE stream never sends, so it never sees its receiver gone: before the stop flag it ran
/// for the life of the process, held the connection open after the session was retired, and the gateway
/// refused every later start with "client id already in use".
#[test]
fn dropping_the_transport_ends_an_idle_pump_and_releases_its_stream() {
    let session = Arc::new(FakeSession::new(Ok(1)));
    let (dropped_tx, dropped_rx) = channel();
    *session.updates.lock().unwrap() = Some(Box::new(WatchedUpdates { dropped: dropped_tx }));
    let t = start(session);
    assert!(
        dropped_rx.recv_timeout(Duration::from_millis(400)).is_err(),
        "a live transport keeps its stream"
    );
    drop(t);
    // One poll (IDLE_POLL, 250 ms) plus slack.
    assert!(
        dropped_rx.recv_timeout(Duration::from_secs(3)).is_ok(),
        "the pump must end and drop the order-update stream after the transport is dropped"
    );
}

// ---- wait_for_end: the one place a quiet wait is told from the end of a subscription ---------------

#[test]
fn a_read_that_answers_at_once_with_nothing_is_the_end() {
    assert!(
        wait_for_end(Duration::from_secs(2), |_| Wait::NoneYet, || true),
        "an ended subscription"
    );
}

#[test]
fn a_read_that_waits_out_its_step_with_nothing_is_not_the_end() {
    let began = Instant::now();
    let ended = wait_for_end(
        Duration::from_millis(300),
        |step| {
            std::thread::sleep(step);
            Wait::NoneYet
        },
        || true,
    );
    assert!(!ended, "it only timed out");
    assert!(began.elapsed() >= Duration::from_millis(300), "and it waited its whole bound");
}

#[test]
fn rows_do_not_end_it_and_a_failure_fails_it() {
    let mut rows = 3;
    let ended = wait_for_end(
        Duration::from_secs(2),
        |_| {
            if rows > 0 {
                rows -= 1;
                Wait::Row
            } else {
                Wait::NoneYet
            }
        },
        || true,
    );
    assert!(ended, "three rows, then the end");
    assert!(!wait_for_end(Duration::from_secs(2), |_| Wait::Failed, || true));
}

fn own() -> OwnOrders {
    OwnOrders { client_id: 7, account: "DU1234567".to_string() }
}

// ---- review 18: R3 (foreign statuses and executions), R4 (the master client), R6 (a disconnect) ------

fn status_of(order_id: i32, client_id: i32) -> OrderUpdate {
    OrderUpdate::OrderStatus(ibapi::orders::OrderStatus {
        order_id,
        client_id,
        status: OrderStatusKind::Cancelled,
        ..ibapi::orders::OrderStatus::default()
    })
}

fn execution_of(order_id: i32, client_id: i32, order_reference: &str) -> OrderUpdate {
    OrderUpdate::ExecutionData(ibapi::orders::ExecutionData {
        execution: ibapi::orders::Execution {
            order_id,
            client_id,
            execution_id: "e1".into(),
            order_reference: order_reference.into(),
            shares: 1.0,
            ..ibapi::orders::Execution::default()
        },
        ..ibapi::orders::ExecutionData::default()
    })
}

/// **R3.** `reqAllOpenOrders` makes TWS send an `orderStatus` for EVERY client's open order. Ids are per API
/// client: another client's order whose id equals one of ours must not become OUR order's `Cancelled`.
#[test]
fn a_status_or_execution_of_another_api_client_is_dropped_before_it_can_meet_our_ids() {
    assert!(translate(status_of(101, 3), &own()).is_none(), "another client's status");
    assert!(translate(execution_of(101, 3, "theirs"), &own()).is_none(), "another client's fill");
    // Ours, and the undecoded/manual 0, pass: 0 is never taken as proof of a stranger.
    let Some(IbInbound::OrderStatus(s)) = translate(status_of(101, 7), &own()) else {
        panic!("our own status passes");
    };
    assert_eq!((s.order_id, s.client_id), (101, 7));
    assert!(matches!(translate(status_of(101, 0), &own()), Some(IbInbound::OrderStatus(_))));
    let Some(IbInbound::ExecDetails(e)) = translate(execution_of(101, 7, "coid-1"), &own()) else {
        panic!("our own execution passes");
    };
    assert_eq!((e.client_id, e.order_ref.as_str()), (7, "coid-1"));
}

#[test]
fn the_master_client_is_refused_for_an_exec_account() {
    let refusal = refuse_master_client(0).unwrap_err().to_string();
    assert!(
        refusal.contains("CLIENT_ID credential field") && refusal.contains("1 or more"),
        "the refusal names the key and the fix: {refusal}"
    );
    assert!(refuse_master_client(1).is_ok());
    assert!(refuse_master_client(7).is_ok());
}

/// **R6.** An end seen while the session is DOWN is not an end: ibapi answers a read of a torn-down
/// subscription exactly like one that ended.
#[test]
fn an_end_seen_while_the_session_is_down_is_not_a_completed_snapshot() {
    assert!(wait_for_end(Duration::from_secs(1), |_| Wait::NoneYet, || true));
    assert!(!wait_for_end(Duration::from_secs(1), |_| Wait::NoneYet, || false));
}

/// The handle the real session builds (`ClientSession::snapshot`) is this struct: its `wait` is the
/// production path, over a scripted read and a scripted liveness.
#[test]
fn the_snapshot_handle_reports_completed_only_for_a_connected_end() {
    let connected = Arc::new(std::sync::atomic::AtomicBool::new(true));
    let probe = Arc::clone(&connected);
    let handle: Box<dyn SnapshotHandle> = Box::new(SnapshotRead {
        read: |_| Wait::NoneYet,
        connected: Arc::new(move || probe.load(std::sync::atomic::Ordering::SeqCst)),
    });
    assert!(handle.wait(Duration::from_secs(1)));
    connected.store(false, std::sync::atomic::Ordering::SeqCst);
    let probe = Arc::clone(&connected);
    let handle: Box<dyn SnapshotHandle> = Box::new(SnapshotRead {
        read: |_| Wait::NoneYet,
        connected: Arc::new(move || probe.load(std::sync::atomic::Ordering::SeqCst)),
    });
    assert!(!handle.wait(Duration::from_secs(1)), "a disconnect is not completed");
}

#[test]
fn a_read_is_classified_by_what_it_found() {
    assert!(matches!(classify::<u8, ()>(Some(Ok(1))), Wait::Row));
    assert!(matches!(classify::<u8, ()>(Some(Err(()))), Wait::Failed));
    assert!(matches!(classify::<u8, ()>(None), Wait::NoneYet));
}
