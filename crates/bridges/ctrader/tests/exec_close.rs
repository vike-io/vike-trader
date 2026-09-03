//! Scripted integration test for close-by-position-id (the hedging-flatten fix): a `CtraderExec`
//! opens a position against the stateful fake server (`common::serve_close`), then a REDUCING
//! submit must route to `ProtoOAClosePositionReq` (NOT an opposite `ProtoOANewOrderReq`) for the
//! tracked position id + volume, and the close's execution events must fold to the correct canonical
//! `OrderAccepted → (OrderPartiallyFilled →) OrderFilled` for the reduce coid — the emitter split
//! end to end through the position-id correlation. Covers full close, partial close, and (hedging)
//! FIFO across two same-symbol positions. See `tests/exec.rs` for the plain open path.

mod common;

use std::sync::Arc;
use std::time::{Duration, Instant};

use vike_ctrader::conn::{connect_and_auth_exec, ConnConfig};
use vike_exec::lanes::Ingest;
use vike_exec::{event_channel, Account, BalanceMode, ExecutionClient, ManagedOrder, OrderStatus};
use vike_model::events::{Event, OrderRejected};
use vike_model::OrderRequest;

use common::{CloseServePolicy, FakeCtrader, NoopSink, CLOSE_POSITION_ID_BASE};

/// A EURUSD market order in `qty` UNITS (the fake's grid is unconstrained, so any qty round-trips).
fn market(coid: &str, side: i32, qty: f64) -> OrderRequest {
    OrderRequest {
        client_order_id: coid.into(),
        venue: "ctrader".into(),
        symbol: "EURUSD".into(),
        side,
        qty,
        order_type: "market".into(),
        ..Default::default()
    }
}

/// A `reduce_only` EURUSD market order.
fn reduce(coid: &str, side: i32, qty: f64) -> OrderRequest {
    OrderRequest { reduce_only: true, ..market(coid, side, qty) }
}

macro_rules! drain {
    ($rx:expr) => {{
        let mut out: Vec<Event> = Vec::new();
        while let Ok(ing) = $rx.try_recv() {
            if let Ingest::Event(e) = ing {
                out.push(e);
            }
        }
        out
    }};
}

/// Poll the ingest lane until `want` events have accumulated (or a 5s deadline), returning them all.
fn collect(rx: &mut tokio::sync::mpsc::Receiver<Ingest>, want: usize) -> Vec<Event> {
    let deadline = Instant::now() + Duration::from_secs(5);
    let mut got: Vec<Event> = Vec::new();
    while got.len() < want && Instant::now() < deadline {
        got.extend(drain!(rx));
        if got.len() < want {
            std::thread::sleep(Duration::from_millis(20));
        }
    }
    got
}

/// Drain and DISCARD every event for `coid` until its terminal `OrderFilled` is seen (used to wait
/// out an OPEN before submitting the reduce, so the position is tracked first).
fn wait_open_filled(rx: &mut tokio::sync::mpsc::Receiver<Ingest>, coid: &str) {
    let deadline = Instant::now() + Duration::from_secs(5);
    while Instant::now() < deadline {
        for e in drain!(rx) {
            if let Event::OrderFilled(f) = e {
                if f.client_order_id == coid {
                    return;
                }
            }
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    panic!("open {coid} never filled");
}

/// Drain until an `Event::OrderRejected` for `coid` arrives (returns it), or panic on a 5s deadline.
fn wait_for_reject(rx: &mut tokio::sync::mpsc::Receiver<Ingest>, coid: &str) -> OrderRejected {
    let deadline = Instant::now() + Duration::from_secs(5);
    while Instant::now() < deadline {
        for e in drain!(rx) {
            if let Event::OrderRejected(r) = e {
                if r.client_order_id == coid {
                    return r;
                }
            }
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    panic!("no OrderRejected for {coid}");
}

fn kinds(events: &[Event]) -> Vec<&'static str> {
    events
        .iter()
        .map(|e| match e {
            Event::OrderSubmitted(_) => "Submitted",
            Event::OrderAccepted(_) => "Accepted",
            Event::Fill(_) => "Fill",
            Event::OrderPartiallyFilled(_) => "PartiallyFilled",
            Event::OrderFilled(_) => "Filled",
            Event::OrderRejected(_) => "Rejected",
            _ => "other",
        })
        .collect()
}

/// The `client_order_id` an event carries (bare `Event::Fill` included), for grouping a mixed
/// event stream by coid — `close_all` interleaves several independent close lifecycles on one lane.
fn coid_of(e: &Event) -> Option<&str> {
    match e {
        Event::OrderSubmitted(x) => Some(&x.client_order_id),
        Event::OrderAccepted(x) => Some(&x.client_order_id),
        Event::OrderPartiallyFilled(x) => Some(&x.client_order_id),
        Event::OrderFilled(x) => Some(&x.client_order_id),
        Event::OrderRejected(x) => Some(&x.client_order_id),
        Event::Fill(f) => Some(&f.client_order_id),
        _ => None,
    }
}

/// Open a EURUSD long, then a plain opposite SELL for the SAME size must FULLY CLOSE it via one
/// `ProtoOAClosePositionReq` (not an opposite NewOrder) — the hedging-flatten fix — and fold to
/// `Submitted, Accepted, Filled` for the reduce coid with the closing-side fill.
#[test]
fn opposite_order_full_closes_via_close_position_req() {
    let server = FakeCtrader::start_close_scripted();
    let cfg = ConnConfig::for_test(server.addr(), "cid", "secret", "token");
    let (events, mut rx) = event_channel(64);
    let handle = connect_and_auth_exec(cfg, Arc::new(NoopSink), events.clone()).expect("auth ok");
    let mut exec = common::exec_with_no_halt(handle, events, module_path!());

    exec.submit(&market("open-1", 1, 1000.0)); // BUY 1000 units → opens position
    wait_open_filled(&mut rx, "open-1");

    exec.submit(&market("flat-1", -1, 1000.0)); // plain opposite SELL → must CLOSE, not hedge

    // Dual-publish: a bare `Event::Fill` (for the Account fold) precedes the `OrderFilled` wrap.
    let got = collect(&mut rx, 4);
    assert_eq!(kinds(&got), vec!["Submitted", "Accepted", "Fill", "Filled"], "events: {got:?}");

    // A close-by-position-id was sent for the opened position, at its full volume — NOT a NewOrder.
    let closes = server.wait_for_closes(1, Duration::from_secs(5));
    assert_eq!(closes, vec![(CLOSE_POSITION_ID_BASE, 100_000)], "one full close leg");
    assert_eq!(server.count_seen("CLOSE_POSITION_REQ"), 1);
    assert_eq!(server.count_seen("NEW_ORDER_REQ"), 1, "only the OPEN used a NewOrder");

    // The bare `Event::Fill` (Account fold) carries the closing side (SELL → -1) for the full size.
    let bare: Vec<_> = got
        .iter()
        .filter_map(|e| match e {
            Event::Fill(f) => Some(f),
            _ => None,
        })
        .collect();
    assert_eq!(bare.len(), 1, "one bare Fill for the Account fold");
    assert_eq!(bare[0].side, -1, "reducing side");
    assert_eq!(bare[0].last_qty, 1000.0);
    // The wrap carries the same fill.
    match got.iter().find(|e| matches!(e, Event::OrderFilled(_))).unwrap() {
        Event::OrderFilled(f) => {
            assert_eq!(f.client_order_id, "flat-1");
            assert_eq!(f.fill.side, -1);
            assert_eq!(f.fill.last_qty, 1000.0);
        }
        _ => unreachable!(),
    }

    drop(exec);
}

/// A `reduce_only` SELL smaller than the open long PARTIALLY closes it: one `ProtoOAClosePositionReq`
/// for the reduce volume, the order fully filled for that amount, the position left open.
#[test]
fn reduce_only_partial_closes_position() {
    let server = FakeCtrader::start_close_scripted();
    let cfg = ConnConfig::for_test(server.addr(), "cid", "secret", "token");
    let (events, mut rx) = event_channel(64);
    let handle = connect_and_auth_exec(cfg, Arc::new(NoopSink), events.clone()).expect("auth ok");
    let mut exec = common::exec_with_no_halt(handle, events, module_path!());

    exec.submit(&market("open-2", 1, 3000.0)); // BUY 3000 units (300_000 centi)
    wait_open_filled(&mut rx, "open-2");

    exec.submit(&reduce("cut-2", -1, 1000.0)); // reduce_only SELL 1000 → partial close 100_000

    let got = collect(&mut rx, 4);
    assert_eq!(kinds(&got), vec!["Submitted", "Accepted", "Fill", "Filled"], "events: {got:?}");

    let closes = server.wait_for_closes(1, Duration::from_secs(5));
    assert_eq!(closes, vec![(CLOSE_POSITION_ID_BASE, 100_000)], "one partial close leg");

    match got.iter().find(|e| matches!(e, Event::OrderFilled(_))).unwrap() {
        Event::OrderFilled(f) => {
            assert_eq!(f.client_order_id, "cut-2");
            assert_eq!(f.fill.side, -1);
            assert_eq!(f.fill.last_qty, 1000.0, "order fully fills for the reduced amount");
        }
        _ => unreachable!(),
    }

    drop(exec);
}

/// HEDGING FIFO: two same-symbol long positions, then a flatten spanning both must close the OLDER
/// first, as two `ProtoOAClosePositionReq` legs, folding to `Submitted, Accepted, PartiallyFilled,
/// Filled` (one core order, filled across two legs). The two close fills net the whole exposure flat.
#[test]
fn fifo_flatten_across_two_hedged_positions() {
    let server = FakeCtrader::start_close_scripted();
    let cfg = ConnConfig::for_test(server.addr(), "cid", "secret", "token");
    let (events, mut rx) = event_channel(64);
    let handle = connect_and_auth_exec(cfg, Arc::new(NoopSink), events.clone()).expect("auth ok");
    let mut exec = common::exec_with_no_halt(handle, events, module_path!());

    // Two BUYs → two OPEN long positions (P1 opened first = FIFO-oldest, then P2).
    exec.submit(&market("open-a", 1, 600.0)); // P1 = BASE, 60_000 centi
    wait_open_filled(&mut rx, "open-a");
    exec.submit(&market("open-b", 1, 400.0)); // P2 = BASE+1, 40_000 centi
    wait_open_filled(&mut rx, "open-b");

    exec.submit(&market("flat-ab", -1, 1000.0)); // flatten both → FIFO close P1 then P2

    // Each leg dual-publishes a bare `Event::Fill` before its wrap: Fill,Partial then Fill,Filled.
    let got = collect(&mut rx, 6);
    assert_eq!(
        kinds(&got),
        vec!["Submitted", "Accepted", "Fill", "PartiallyFilled", "Fill", "Filled"],
        "events: {got:?}"
    );

    // Two close legs, OLDEST position first (P1 = BASE), each at its own volume.
    let closes = server.wait_for_closes(2, Duration::from_secs(5));
    assert_eq!(
        closes,
        vec![(CLOSE_POSITION_ID_BASE, 60_000), (CLOSE_POSITION_ID_BASE + 1, 40_000)],
        "FIFO: older position closed first"
    );
    assert_eq!(server.count_seen("NEW_ORDER_REQ"), 2, "only the two OPENs used NewOrder");

    // Both fills are the closing side; cumulative closed qty = the full 1000 units flattened.
    let closed_qty: f64 = got
        .iter()
        .filter_map(|e| match e {
            Event::OrderPartiallyFilled(w) => Some((w.fill.side, w.fill.last_qty)),
            Event::OrderFilled(w) => Some((w.fill.side, w.fill.last_qty)),
            _ => None,
        })
        .map(|(side, qty)| {
            assert_eq!(side, -1, "every close leg fills SELL-side");
            qty
        })
        .sum();
    assert_eq!(closed_qty, 1000.0, "net exposure flattened across both legs");

    drop(exec);
}

/// The core-Account correctness the whole feature exists for: a close's bare `Event::Fill`, folded
/// through `Account`, REDUCES the open position to FLAT (it does not open an opposing one). Seeds a
/// long from the opening fill, folds the close fill, asserts the net position size is ~0.
#[test]
fn close_fill_folds_through_account_and_reduces_position_to_flat() {
    let server = FakeCtrader::start_close_scripted();
    let cfg = ConnConfig::for_test(server.addr(), "cid", "secret", "token");
    let (events, mut rx) = event_channel(64);
    let handle = connect_and_auth_exec(cfg, Arc::new(NoopSink), events.clone()).expect("auth ok");
    let mut exec = common::exec_with_no_halt(handle, events, module_path!());

    exec.submit(&market("open-3", 1, 1000.0)); // establish a long the bridge tracks
    wait_open_filled(&mut rx, "open-3");
    exec.submit(&market("flat-3", -1, 1000.0)); // opposite SELL → routes to close-position
    let got = collect(&mut rx, 4);

    // The close emits a bare `Event::Fill` carrying the REDUCING side (SELL → -1) for the full size.
    let close_fill = got
        .iter()
        .find_map(|e| match e {
            Event::Fill(f) => Some(f.clone()),
            _ => None,
        })
        .expect("close dual-publishes a bare Event::Fill");
    assert_eq!(close_fill.side, -1, "closing a long is a SELL fill");
    assert_eq!(close_fill.last_qty, 1000.0);

    // Fold the OPENING long (same fill, side flipped +1) then the close through a fresh Account: it
    // must net FLAT. If the close were an opposing OPEN it would double the exposure instead.
    let mut account = Account::new(1.0, "ctrader", None, BalanceMode::Delta);
    let mut open_fill = close_fill.clone();
    open_fill.side = 1;
    // A DISTINCT `trade_id`: this is a synthetic OPENING execution, not the close being replayed, and
    // `Account::apply_fill` is idempotent per `trade_id` (the dedup ledger lives on the account now).
    // Cloning the close's id made the second fold a refusal — and, because the side differs, the
    // account correctly reported it as an id COLLISION rather than a re-delivery.
    open_fill.trade_id =
        vike_model::events::TradeId::prefixed("synthetic-open-", &close_fill.trade_id);
    account.apply_fill(&open_fill);
    let long: f64 = account.positions.values().map(|p| p.size).sum();
    assert_eq!(long, 1000.0, "seeded long from the opening fill");

    account.apply_fill(&close_fill);
    let net: f64 = account.positions.values().map(|p| p.size).sum();
    assert!(net.abs() < 1e-9, "close REDUCES the position to flat, got net size {net}");

    drop(exec);
}

/// CRITICAL 1 — a SECOND reduce on a position whose first reduce is still IN FLIGHT must NOT
/// overwrite the first coid's mapping: it is rejected cleanly as "already closing", and no second
/// close is sent (the first mapping stays intact so its fill still attributes to the first coid).
/// The `Hold` policy leaves the first close un-answered, so the position stays in flight
/// deterministically. (The first coid's own terminalization is covered by the fill ladders above.)
#[test]
fn concurrent_reduce_on_same_position_rejects_second_without_overwriting() {
    let server = FakeCtrader::start_close_with_policy(CloseServePolicy::Hold);
    let cfg = ConnConfig::for_test(server.addr(), "cid", "secret", "token");
    let (events, mut rx) = event_channel(64);
    let handle = connect_and_auth_exec(cfg, Arc::new(NoopSink), events.clone()).expect("auth ok");
    let mut exec = common::exec_with_no_halt(handle, events, module_path!());

    exec.submit(&market("open-c", 1, 1000.0));
    wait_open_filled(&mut rx, "open-c");

    // First reduce: registers the close for the position and sends it (the fake HOLDS it).
    exec.submit(&market("reduce-1", -1, 1000.0));
    let sent = server.wait_for_closes(1, Duration::from_secs(5));
    assert_eq!(sent, vec![(CLOSE_POSITION_ID_BASE, 100_000)], "first reduce's close is in flight");

    // Second reduce on the SAME still-in-flight position → rejected, no overwrite, no second close.
    exec.submit(&market("reduce-2", -1, 1000.0));
    let rej = wait_for_reject(&mut rx, "reduce-2");
    assert!(rej.reason.contains("already closing"), "reason: {}", rej.reason);
    assert_eq!(
        server.close_requests().len(),
        1,
        "the second reduce sent NO close — the first coid's mapping is never overwritten"
    );

    drop(exec);
}

/// CRITICAL 2 — a FIFO close where leg-1 FILLS then leg-2 REJECTS must terminalize the coid as
/// `OrderFilled` (for the volume already closed), NOT `OrderRejected`: `OrderRejected` from
/// `PartiallyFilled` is an illegal FSM transition that the engine DROPS, stranding the order.
/// Verified both at the event level and by folding the whole stream through the real `ManagedOrder`
/// FSM (it must reach a legal `Filled`).
#[test]
fn leg_reject_after_partial_terminalizes_as_filled_not_dropped() {
    let server = FakeCtrader::start_close_with_policy(CloseServePolicy::FillThenReject);
    let cfg = ConnConfig::for_test(server.addr(), "cid", "secret", "token");
    let (events, mut rx) = event_channel(64);
    let handle = connect_and_auth_exec(cfg, Arc::new(NoopSink), events.clone()).expect("auth ok");
    let mut exec = common::exec_with_no_halt(handle, events, module_path!());

    exec.submit(&market("open-a", 1, 600.0));
    wait_open_filled(&mut rx, "open-a");
    exec.submit(&market("open-b", 1, 400.0));
    wait_open_filled(&mut rx, "open-b");

    exec.submit(&market("flat-fr", -1, 1000.0)); // 2-leg close: leg-1 fills, leg-2 rejects
    let got = collect(&mut rx, 5);

    assert_eq!(
        kinds(&got),
        vec!["Submitted", "Accepted", "Fill", "PartiallyFilled", "Filled"],
        "terminal is Filled (partial-then-done), not a dropped Rejected: {got:?}"
    );
    assert!(
        !got.iter().any(|e| matches!(e, Event::OrderRejected(_))),
        "must NOT reject after partial progress (would be a dropped illegal transition)"
    );

    // Fold the whole lifecycle through the real FSM: it must reach a LEGAL Filled terminal, proving
    // the coid is not stranded. (Bare `Event::Fill` is not an FSM lifecycle event — apply ignores it.)
    let mut mo = ManagedOrder::new(OrderRequest {
        client_order_id: "flat-fr".into(),
        venue: "ctrader".into(),
        symbol: "EURUSD".into(),
        side: -1,
        qty: 1000.0,
        order_type: "market".into(),
        ..Default::default()
    });
    for e in &got {
        let _ = mo.apply(e); // bare Fill errors harmlessly; the lifecycle events advance the FSM
    }
    assert_eq!(
        mo.status,
        OrderStatus::Filled,
        "FSM reaches Filled, not stranded at PartiallyFilled"
    );

    drop(exec);
}

/// IMPORTANT 3 — a wire-level `ERROR_RES` refusing a close must both TERMINATE the coid and FORGET
/// it from the tracker, leaving no stale `by_position` entry. Proven by a follow-up reduce on the
/// SAME position succeeding (routing to a real close + fill): if the first coid's entries were
/// stale, the retry would instead be rejected as "already closing".
#[test]
fn wire_error_res_on_close_rejects_and_forgets_so_a_retry_can_close() {
    let server = FakeCtrader::start_close_with_policy(CloseServePolicy::ErrorResThenFill);
    let cfg = ConnConfig::for_test(server.addr(), "cid", "secret", "token");
    let (events, mut rx) = event_channel(64);
    let handle = connect_and_auth_exec(cfg, Arc::new(NoopSink), events.clone()).expect("auth ok");
    let mut exec = common::exec_with_no_halt(handle, events, module_path!());

    exec.submit(&market("open-e", 1, 1000.0));
    wait_open_filled(&mut rx, "open-e");

    // First reduce: its close is refused with a protocol ERROR_RES → terminal reject + tracker forget.
    exec.submit(&market("reduce-e1", -1, 1000.0));
    let rej = wait_for_reject(&mut rx, "reduce-e1");
    assert!(!rej.reason.is_empty(), "carries the venue error text");

    // The forget freed the position: a RETRY must route to a real close (and fill), NOT be rejected
    // as already-closing — which would prove a stale by_position entry survived the wire error.
    exec.submit(&market("reduce-e2", -1, 1000.0));
    let got = collect(&mut rx, 4);
    assert!(
        got.iter().any(|e| matches!(e, Event::OrderFilled(f) if f.client_order_id == "reduce-e2")),
        "retry closes cleanly (tracker forgot the failed coid): {got:?}"
    );
    assert!(
        !got.iter()
            .any(|e| matches!(e, Event::OrderRejected(r) if r.client_order_id == "reduce-e2")),
        "retry must not be rejected as already-closing"
    );
    // Two close requests total: the refused one and the successful retry.
    let closes = server.wait_for_closes(2, Duration::from_secs(5));
    assert_eq!(closes.len(), 2, "the retry issued a fresh close");

    drop(exec);
}

/// CRITICAL 1 (edge) — a PLAIN reduce whose free opposing exposure is smaller than the request but
/// whose TOTAL opposing exposure covers it (the rest LOCKED in another coid's in-flight close) must
/// be REJECTED, NOT mis-read as a flip that opens a hedge. Repro: two longs (600 older + 400); the
/// first reduce (SELL 600) holds P1 in flight; a SELL 700 then sees only P2 (400) free — 700 > 400
/// yet 700 <= 1000 total, so it is a reduce the free portion can't cover, not a real flip.
#[test]
fn plain_reduce_partly_locked_in_flight_rejects_not_hedges() {
    let server = FakeCtrader::start_close_with_policy(CloseServePolicy::Hold);
    let cfg = ConnConfig::for_test(server.addr(), "cid", "secret", "token");
    let (events, mut rx) = event_channel(64);
    let handle = connect_and_auth_exec(cfg, Arc::new(NoopSink), events.clone()).expect("auth ok");
    let mut exec = common::exec_with_no_halt(handle, events, module_path!());

    exec.submit(&market("open-p1", 1, 600.0)); // P1 = BASE (older)
    wait_open_filled(&mut rx, "open-p1");
    exec.submit(&market("open-p2", 1, 400.0)); // P2 = BASE + 1
    wait_open_filled(&mut rx, "open-p2");

    // First reduce closes the OLDER P1 (600), held in flight.
    exec.submit(&market("reduce-1", -1, 600.0));
    let sent = server.wait_for_closes(1, Duration::from_secs(5));
    assert_eq!(sent, vec![(CLOSE_POSITION_ID_BASE, 60_000)], "reduce-1 closes P1 (held)");

    // Plain SELL 700: free opposing is only P2 (400); 700 > 400 but 700 <= 1000 TOTAL → reject, not
    // a hedge-opening NewOrder.
    exec.submit(&market("reduce-2", -1, 700.0));
    let rej = wait_for_reject(&mut rx, "reduce-2");
    assert!(rej.reason.contains("closing"), "reason: {}", rej.reason);
    assert_eq!(server.close_requests().len(), 1, "reduce-2 sent no close (only reduce-1's)");
    assert_eq!(
        server.count_seen("NEW_ORDER_REQ"),
        2,
        "reduce-2 opened NO hedge — only the two opening NewOrders were sent"
    );

    drop(exec);
}

/// CRITICAL 1 (flip preserved) — a GENUINE flip (a plain reduce LARGER than TOTAL opposing exposure)
/// still opens the other way via a plain NewOrder, unchanged. Ensures the partial-in-flight reject
/// above did not break the flip path.
#[test]
fn genuine_flip_past_total_opposing_still_opens() {
    let server = FakeCtrader::start_close_scripted(); // Fill policy
    let cfg = ConnConfig::for_test(server.addr(), "cid", "secret", "token");
    let (events, mut rx) = event_channel(64);
    let handle = connect_and_auth_exec(cfg, Arc::new(NoopSink), events.clone()).expect("auth ok");
    let mut exec = common::exec_with_no_halt(handle, events, module_path!());

    exec.submit(&market("open-f", 1, 600.0)); // long 600
    wait_open_filled(&mut rx, "open-f");

    // Plain SELL 1000 > TOTAL opposing (600) → a genuine flip → opens via NewOrder (NOT a close).
    exec.submit(&market("flip-f", -1, 1000.0));
    let got = collect(&mut rx, 3);
    assert!(
        got.iter().any(|e| matches!(e, Event::OrderFilled(f) if f.client_order_id == "flip-f")),
        "genuine flip opens (NewOrder fill under its own coid): {got:?}"
    );
    assert!(server.close_requests().is_empty(), "a genuine flip does NOT close by position id");
    assert_eq!(server.count_seen("NEW_ORDER_REQ"), 2, "the open + the flip both used NewOrder");

    drop(exec);
}

/// THE connect-time position seed, end to end: a FRESH mount against a venue that already holds a
/// position must know about it, so a plain opposite order CLOSES rather than opening a hedge.
///
/// ⚠ This is the bug that forced `close_all` into existence. `ProtoOAReconcileReq` used to be
/// issued only on RECONNECT, so a process that mounted, never traded and never lost its socket kept
/// an empty position map for its whole life — and `plan_reduce` against an empty map sees no
/// opposing exposure and routes a flatten as a NEW ORDER.
///
/// ⚠ **MEASURED, not claimed** (the CI box, 2026-08-08). Stop calling
/// `crates/bridges/ctrader/src/conn.rs`'s `seed_positions_at_connect` at the handshake and this
/// binary runs `12 tests: 10 passed, 2 failed` — THIS test, with a `NEW_ORDER_REQ` where a
/// `CLOSE_POSITION_REQ` belongs (the hedge it used to stack), plus
/// `the_exec_close_suite_is_indifferent_to_an_engaged_halt_sentinel`, which re-runs this binary in a
/// child process and so inherits the same failure.
#[test]
fn a_fresh_mount_seeds_the_position_book_so_a_reduce_closes_instead_of_hedging() {
    // ONE pre-existing EURUSD long of 1000 units — a position this process never saw open.
    let server = FakeCtrader::start_close_preseeded(&[(1, 100_000)]);
    let cfg = ConnConfig::for_test(server.addr(), "cid", "secret", "token");
    let (events, _rx) = event_channel(64);
    let handle = connect_and_auth_exec(cfg, Arc::new(NoopSink), events.clone()).expect("auth ok");
    let mut exec = common::exec_with_no_halt(handle, events, module_path!());

    // A plain opposite SELL for exactly the open size. With a seeded book this is a pure reduce.
    exec.submit(&market("reduce-after-mount", -1, 1000.0));

    let closes = server.wait_for_closes(1, Duration::from_secs(5));
    assert_eq!(
        closes,
        vec![(CLOSE_POSITION_ID_BASE, 100_000)],
        "the reduce must close the pre-existing position by id"
    );
    assert_eq!(
        server.count_seen("NEW_ORDER_REQ"),
        0,
        "a hedge was opened against a position the mount should already have known about"
    );
    assert_eq!(server.wait_until_flat(Duration::from_secs(5)), 0, "the venue ends flat");

    drop(exec);
}

/// …and what that seed COSTS when the venue does not answer: the mount is bounded by
/// `crates/bridges/ctrader/src/conn.rs`'s `SEED_TIMEOUT`, NOT by the reconnect twin's
/// `RECONCILE_TIMEOUT`.
///
/// ⚠ This is a real operator-facing number, not tidiness. cTrader is the only venue whose exec
/// client connects SYNCHRONOUSLY at mount, so a venue that completes the handshake and then goes
/// quiet on this ONE request stalls a daemon that has not started yet — silently, since a mount
/// that looks hung is a mount somebody kills. `FakeCtrader::start_mute_reconcile` is exactly that
/// venue: the full handshake, then silence for `RECONCILE_REQ` with the socket left open, which is
/// the only shape that costs the whole deadline (a rejection returns on the spot, a dead socket at
/// EOF).
///
/// The two asserts are a floor and a ceiling, and both carry weight. The FLOOR proves the seed
/// really was attempted and really did wait for its bound — a build that skipped the request
/// entirely would sail under any ceiling. The CEILING sits BETWEEN the two candidate bounds rather
/// than just above `SEED_TIMEOUT`, so what it pins is the DECISION that this call has its own,
/// tighter bound: measured on the CI box this mount takes **2.03s–2.08s** (`SEED_TIMEOUT` rounded up to
/// the socket's 1s read granularity — `crates/bridges/ctrader/src/conn.rs`'s `SEED_TIMEOUT` doc
/// explains why) against **5.120s** under `RECONCILE_TIMEOUT`, and 3.5s separates them with ~1.4s
/// of slack on either side so a loaded box does not redden it.
///
/// ⚠ **MUTATION PROVEN** (the CI box, 2026-08-08): point `read_reconcile_res`'s seed call at
/// `RECONCILE_TIMEOUT` instead of `SEED_TIMEOUT` and this test fails on the ceiling —
/// `a mute venue stalled the SYNCHRONOUS mount for 5.120162692s`. The mount is left CORRECT either
/// way — it still succeeds, and the book still reports UNFETCHED — which is exactly why the cost
/// has to be tested rather than eyeballed: nothing else about the run looks different.
#[test]
fn a_mute_venue_costs_the_mount_the_seed_bound_not_the_reconnect_one() {
    let server = FakeCtrader::start_mute_reconcile();
    let cfg = ConnConfig::for_test(server.addr(), "cid", "secret", "token");
    let (events, _rx) = event_channel(64);

    let started = Instant::now();
    let handle = connect_and_auth_exec(cfg, Arc::new(NoopSink), events).expect("auth ok");
    let mount = started.elapsed();

    assert!(
        mount >= vike_ctrader::conn::SEED_TIMEOUT,
        "the mount returned in {mount:?}, faster than SEED_TIMEOUT — the seed cannot have been \
         requested at all (the venue never answers, so waiting the bound is the only way out)"
    );
    assert!(
        mount < Duration::from_millis(3500),
        "a mute venue stalled the SYNCHRONOUS mount for {mount:?}; the seed must be bounded by \
         SEED_TIMEOUT ({}ms), not by the reconnect twin's RECONCILE_TIMEOUT (5s)",
        vike_ctrader::conn::SEED_TIMEOUT.as_millis()
    );

    // The mount still SUCCEEDS, with an honest book: best-effort means the venue's silence costs
    // latency and evidence, never the venue itself (it must not fall back to paper over this).
    assert!(
        !handle.positions.lock().expect("book lock").is_fetched(),
        "a seed that was never answered must leave the book UNFETCHED, not silently 'flat'"
    );
    handle.shutdown();
}

/// `close_all` flattens a PRE-EXISTING hedged book — positions this session's map never tracked (the
/// demo-account "stacked hedged positions a fresh connect doesn't know" case). Given the raw
/// `(position_id, volume)` legs a reconcile fetched, it issues one `ProtoOAClosePositionReq` per
/// position (side-agnostic: a long closes SELL, a short closes BUY), each folding to a proper
/// `Submitted → Accepted → Fill → Filled`, opening NOTHING — and the venue ends with ZERO open
/// positions.
///
/// ⚠ Its original justification — "a reduce ORDER cannot do this: against an empty position map it
/// would open a hedge" — no longer holds now that the book is SEEDED at connect (see the test
/// above). `close_all` survives on its narrower merits: it flattens a whole HEDGED book in one
/// call, where a reduce order can only ever net one side at a time, and it needs no symbol
/// resolution because close-by-position-id is side- and symbol-agnostic.
#[test]
fn close_all_flattens_preexisting_hedged_book() {
    // A 6-position HEDGED book: 3 longs + 3 shorts, all EURUSD, 1000 units (100_000 centi) each.
    let seed: Vec<(i32, i64)> =
        vec![(1, 100_000), (-1, 100_000), (1, 100_000), (-1, 100_000), (1, 100_000), (-1, 100_000)];
    let server = FakeCtrader::start_close_preseeded(&seed);
    assert_eq!(server.open_position_count(), 6, "seeded a 6-position hedged book");

    let cfg = ConnConfig::for_test(server.addr(), "cid", "secret", "token");
    let (events, mut rx) = event_channel(64);
    let handle = connect_and_auth_exec(cfg, Arc::new(NoopSink), events.clone()).expect("auth ok");
    let exec = common::exec_with_no_halt(handle, events, module_path!());

    // The legs a reconcile would hand us: sequential ids from CLOSE_POSITION_ID_BASE, full volume.
    let legs: Vec<(i64, i64)> = seed
        .iter()
        .enumerate()
        .map(|(i, &(_, v))| (CLOSE_POSITION_ID_BASE + i as i64, v))
        .collect();

    let coids = exec.close_all(&legs);
    assert_eq!(coids.len(), 6, "one close order issued per position");

    // Six ProtoOAClosePositionReq — one per seeded position id at its full volume — and no opens.
    let mut closes = server.wait_for_closes(6, Duration::from_secs(5));
    closes.sort();
    let mut want = legs.clone();
    want.sort();
    assert_eq!(closes, want, "one full close per position id");
    assert_eq!(server.count_seen("NEW_ORDER_REQ"), 0, "close_all opens nothing");

    // Each coid folds to a proper Submitted (emitter split) → Accepted → Fill → Filled lifecycle.
    let got = collect(&mut rx, 6 * 4);
    for coid in &coids {
        let per: Vec<Event> =
            got.iter().filter(|e| coid_of(e) == Some(coid.as_str())).cloned().collect();
        assert_eq!(
            kinds(&per),
            vec!["Submitted", "Accepted", "Fill", "Filled"],
            "coid {coid} lifecycle: {per:?}"
        );
    }

    // Six bare Event::Fill (#568 dual-publish that nets Account toward flat): 3 SELL (long closes) +
    // 3 BUY (short closes), proving close-by-position-id is side-agnostic across the hedged book.
    let close_fill_sides: Vec<i32> = got
        .iter()
        .filter_map(|e| match e {
            Event::Fill(f) => Some(f.side),
            _ => None,
        })
        .collect();
    assert_eq!(close_fill_sides.len(), 6, "one bare Fill per closed position");
    assert_eq!(close_fill_sides.iter().filter(|&&s| s == -1).count(), 3, "3 long-closing SELLs");
    assert_eq!(close_fill_sides.iter().filter(|&&s| s == 1).count(), 3, "3 short-closing BUYs");

    // TRULY FLAT: every pre-existing position closed by id — zero remain at the venue.
    assert_eq!(
        server.wait_until_flat(Duration::from_secs(5)),
        0,
        "all pre-existing positions closed at the venue"
    );

    drop(exec);
}

/// This binary's verdict must not change when an operator HALT sentinel is engaged —
/// `common::assert_indifferent_to_an_engaged_halt_sentinel` carries the argument and the mechanism.
#[test]
fn the_exec_close_suite_is_indifferent_to_an_engaged_halt_sentinel() {
    common::assert_indifferent_to_an_engaged_halt_sentinel();
}
