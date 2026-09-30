use super::*;
use crate::event_mapper::{IbCommissionReport, IbExecDetails, IbOrderStatus};
use crate::transport::FakeTransport;
use vike_model::events::Event;

/// The grace window these tests age against — a literal, not the production constant, because
/// what is under test is the WIRING (does a stale entry reach `request_executions`), not the
/// value of the window. `now_ms` is opaque and monotonic, so any scale works.
const GRACE: i64 = 1_000;

/// A mapper holding ONE execution buffered awaiting its commission — the stranded shape.
fn mapper_with_buffered_exec() -> EventMapper {
    let mut ids = IdRegistry::default();
    ids.on_next_valid_id(101);
    let order_id = ids.next_order_id();
    ids.bind(order_id, "coid-A");
    let mut m = EventMapper::new(ids);
    m.on_submit(order_id, 10.0);
    let _ = m.on_order_status(IbOrderStatus {
        order_id,
        order_ref: "coid-A".into(),
        status: "Submitted".into(),
        filled: 0.0,
        avg_fill_price: 0.0,
    });
    let evs = m.on_exec_details(IbExecDetails {
        order_id,
        order_ref: "coid-A".into(),
        exec_id: "e1".into(),
        symbol: "AAPL.SMART.USD".into(),
        side_buy: true,
        shares: 10.0,
        price: 190.0,
        ts: 7,
    });
    assert!(evs.is_empty(), "the fill must not emit before its commission joins");
    m
}

#[test]
fn sweep_asks_the_venue_to_replay_executions_only_once_the_grace_window_passes() {
    let mut m = mapper_with_buffered_exec();
    let mut t = FakeTransport::new();
    let calls = t.execution_requests();

    // Inside the window: no request. (The very first sweep can only STAMP the entry — the
    // mapper is clockless, so nothing knows how old it is until a sweep observes it.)
    sweep_stranded_fills(&mut m, &mut t, 0, GRACE);
    sweep_stranded_fills(&mut m, &mut t, GRACE - 1, GRACE);
    assert_eq!(*calls.lock().unwrap(), 0, "a fresh buffered exec must not provoke a replay");

    // Past it: exactly one account-wide re-request.
    sweep_stranded_fills(&mut m, &mut t, GRACE, GRACE);
    assert_eq!(*calls.lock().unwrap(), 1);
}

#[test]
fn sweep_is_silent_and_free_when_nothing_is_buffered() {
    // The steady state — the overwhelming majority of passes. No `pending` entry means no
    // request at any age, so the loop's 1s cadence costs the venue nothing.
    let mut ids = IdRegistry::default();
    ids.on_next_valid_id(101);
    let mut m = EventMapper::new(ids);
    let mut t = FakeTransport::new();
    let calls = t.execution_requests();
    for tick in 0..10 {
        sweep_stranded_fills(&mut m, &mut t, tick * GRACE * 10, GRACE);
    }
    assert_eq!(*calls.lock().unwrap(), 0);
}

#[test]
fn sweep_stops_re_requesting_once_the_retry_budget_is_spent() {
    let mut m = mapper_with_buffered_exec();
    let mut t = FakeTransport::new();
    let calls = t.execution_requests();
    let mut now = 0;
    // Well past the budget: the ladder must plateau, never hammer the venue forever.
    for _ in 0..(MAX_PENDING_RECOVERY_ATTEMPTS + 5) {
        sweep_stranded_fills(&mut m, &mut t, now, GRACE);
        now += GRACE;
    }
    assert_eq!(*calls.lock().unwrap(), MAX_PENDING_RECOVERY_ATTEMPTS);
}

#[test]
fn a_resync_re_requests_open_orders_and_executions() {
    // A filled order is not an OPEN order, so the open-order snapshot structurally cannot
    // recover a fill lost across the blip — the executions replay is what does.
    let mut m = mapper_with_buffered_exec();
    let mut t = FakeTransport::new();
    let calls = t.execution_requests();
    let evs = fold_inbound(&mut m, &mut t, IbInbound::StreamResync);
    assert!(evs.is_empty(), "the resync itself emits nothing");
    assert_eq!(*calls.lock().unwrap(), 1);
}

#[test]
fn the_replay_a_reconnect_triggers_emits_the_stranded_fill_with_the_real_commission() {
    // End to end through `fold_inbound`: the commission was lost, the socket blipped, the
    // resync replays BOTH halves, and the ordinary join emits the fill — carrying IBKR's own
    // commission figure, never a fabricated 0.0.
    let mut m = mapper_with_buffered_exec();
    let mut t = FakeTransport::new();
    assert!(fold_inbound(&mut m, &mut t, IbInbound::StreamResync).is_empty());

    // What `reqExecutions` delivers: the execution, then its commission report.
    let replayed_exec = IbInbound::ExecDetails(IbExecDetails {
        order_id: 101,
        order_ref: "coid-A".into(),
        exec_id: "e1".into(),
        symbol: "AAPL.SMART.USD".into(),
        side_buy: true,
        shares: 10.0,
        price: 190.0,
        ts: 7,
    });
    assert!(fold_inbound(&mut m, &mut t, replayed_exec).is_empty());
    let replayed_commission = IbInbound::Commission(IbCommissionReport {
        exec_id: "e1".into(),
        commission: 1.25,
        currency: "USD".into(),
    });
    match fold_inbound(&mut m, &mut t, replayed_commission).as_slice() {
        [Event::OrderFilled(f)] => {
            assert_eq!(f.client_order_id, "coid-A");
            assert_eq!(f.fill.trade_id, "e1");
            assert_eq!(f.fill.last_qty, 10.0);
            assert_eq!(f.fill.commission, 1.25);
        }
        other => panic!("expected exactly one OrderFilled, got {other:?}"),
    }

    // A SECOND reconnect replays the same pair again — now inert, so the recovery cannot
    // become a double-fill generator.
    assert!(fold_inbound(&mut m, &mut t, IbInbound::StreamResync).is_empty());
    let again = IbInbound::ExecDetails(IbExecDetails {
        order_id: 101,
        order_ref: "coid-A".into(),
        exec_id: "e1".into(),
        symbol: "AAPL.SMART.USD".into(),
        side_buy: true,
        shares: 10.0,
        price: 190.0,
        ts: 7,
    });
    assert!(fold_inbound(&mut m, &mut t, again).is_empty());
    let again_c = IbInbound::Commission(IbCommissionReport {
        exec_id: "e1".into(),
        commission: 1.25,
        currency: "USD".into(),
    });
    assert!(fold_inbound(&mut m, &mut t, again_c).is_empty());
}

/// The death notice latches the registry closed and — unlike the resync it used to be spelled
/// as — issues NO venue requests. `request_open_orders`/`request_executions` are answered
/// THROUGH the pump that has just exited, so on a dead stream they are writes into a socket
/// nobody reads: at best a logged error, at worst a kernel-buffered `Ok` that looks like it
/// worked.
#[test]
fn a_stream_death_latches_the_venue_and_requests_nothing() {
    let mut m = mapper_with_buffered_exec();
    let mut t = FakeTransport::new();
    let calls = t.execution_requests();

    let evs = fold_inbound(
        &mut m,
        &mut t,
        IbInbound::StreamDead { reason: "connection reset by peer".into() },
    );
    assert!(
        evs.is_empty(),
        "the death itself emits nothing — in-flight state is UNKNOWN, not terminal"
    );
    assert_eq!(
        *calls.lock().unwrap(),
        0,
        "a dead stream must provoke NO replay request — the resync arm is the other variant"
    );
    assert!(m.ids().stream_dead());
    assert_eq!(m.ids().stream_death_reason(), Some("connection reset by peer"));
    assert!(!m.ids().is_connected());
}

/// The stranded-fill sweep must stop once the stream is dead. Its whole mechanism is "ask IBKR
/// to re-deliver, and read the answer off the pump" — with the pump gone the answer can never
/// arrive, so an ungated sweep spends the entire finite retry budget on writes that cannot be
/// answered, and the ladder is exhausted for a remount that could still have used it.
#[test]
fn the_stranded_fill_sweep_stops_once_the_stream_is_dead() {
    let mut m = mapper_with_buffered_exec();
    let mut t = FakeTransport::new();
    let calls = t.execution_requests();

    // Alive: the ladder runs (this is `sweep_asks_the_venue_…`'s established behaviour).
    sweep_stranded_fills(&mut m, &mut t, 0, GRACE);
    sweep_stranded_fills(&mut m, &mut t, GRACE, GRACE);
    assert_eq!(*calls.lock().unwrap(), 1, "control: the ladder runs on a live stream");

    // Dead: every further pass must be a no-op, however far the clock is advanced and however
    // much retry budget is left. Without the gate this loop spends the rest of the budget.
    let _ = fold_inbound(&mut m, &mut t, IbInbound::StreamDead { reason: "reset".into() });
    let mut now = GRACE;
    for _ in 0..(MAX_PENDING_RECOVERY_ATTEMPTS + 5) {
        now += GRACE;
        sweep_stranded_fills(&mut m, &mut t, now, GRACE);
    }
    assert_eq!(
        *calls.lock().unwrap(),
        1,
        "a dead stream must end the ladder — no further replay requests may be issued"
    );
}

/// The whole point of splitting the variant: the two conditions must NOT fold the same way.
/// `StreamResync` re-requests state; `StreamDead` latches the venue closed and requests nothing.
#[test]
fn resync_and_death_fold_differently() {
    let mut resync_m = mapper_with_buffered_exec();
    let mut resync_t = FakeTransport::new();
    let resync_calls = resync_t.execution_requests();
    let _ = fold_inbound(&mut resync_m, &mut resync_t, IbInbound::StreamResync);

    let mut dead_m = mapper_with_buffered_exec();
    let mut dead_t = FakeTransport::new();
    let dead_calls = dead_t.execution_requests();
    let _ = fold_inbound(&mut dead_m, &mut dead_t, IbInbound::StreamDead { reason: "x".into() });

    assert_eq!(*resync_calls.lock().unwrap(), 1, "resync re-requests the day's executions");
    assert_eq!(*dead_calls.lock().unwrap(), 0, "death requests nothing");
    assert!(!resync_m.ids().stream_dead(), "a resync must NOT latch the venue closed");
    assert!(dead_m.ids().stream_dead());
}

/// The orders named in the death log are the ones whose venue state became unknowable — the
/// list an operator takes to TWS. Nothing is synthesized for them, so NAMING them is the only
/// thing the bridge can truthfully do.
#[test]
fn the_death_notice_can_name_the_orders_left_in_limbo() {
    let m = mapper_with_buffered_exec();
    assert_eq!(m.live_client_order_ids(), vec!["coid-A"]);
}
