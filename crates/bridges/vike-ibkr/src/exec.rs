//! The exec loop: drains `ExecCommand` (submit/cancel/modify/shutdown), maps to transport calls,
//! and pumps transport inbound through the `EventMapper` onto the `EventSender` lane. Reused by the
//! socket backend (Task 9) and the `FakeTransport` double. `OrderSubmitted` is emitted
//! synchronously at submit (the vike venue-adapter contract); the venue-side
//! Accepted/Filled/Rejected/Canceled events fold in from the transport inbound.

use std::sync::mpsc::{Receiver, TryRecvError};
use std::time::{Duration, Instant};

use vike_bridge_core::exec_actor::{cancel_batch_undeclared, ExecCommand};
use vike_exec::EventSender;
use vike_model::events::{Event, OrderRejected, OrderSubmitted};

use crate::contract::parse_simplified;
use crate::event_mapper::{
    EventMapper, MAX_PENDING_RECOVERY_ATTEMPTS, PENDING_COMMISSION_GRACE_MS,
};
use crate::id_registry::IdRegistry;
use crate::order::map_order_request;
use crate::transport::{IbInbound, IbkrTransport};

/// How often the exec loop asks the mapper whether any buffered execution has outlived its
/// commissionReport (`event_mapper`'s module doc trap 5). One `HashMap` walk over a map that is
/// empty in steady state, on a loop that already wakes every 20ms — the cadence only needs to be
/// fine enough that the effective grace window is `PENDING_COMMISSION_GRACE_MS` to within one tick.
const PENDING_SWEEP_INTERVAL: Duration = Duration::from_secs(1);

/// How long a submit waits for the venue handshake (`NextValidId` + `AccountsReady`) before being
/// refused. The readiness gate exists because `IdRegistry::next_order_id` floors at 101 whether or
/// not IB has ever told us its next valid id, so a submit that beats the handshake picks a number
/// the server may already have used — but the seeds are pushed onto the inbound channel by
/// `connect` BEFORE it returns, so in practice the wait is microseconds and this bound only ever
/// engages on a genuinely half-open transport.
///
/// It is a bounded WAIT rather than an instant refusal precisely because the loop drains one
/// inbound per iteration: an instant gate would reject the first order of a session on a race with
/// the loop's own startup. It is bounded rather than unbounded because
/// `crates/bridges/vike-ibkr/tests/ibkr_smoke.rs`'s item 4 asks a human to confirm this handshake
/// cannot hang or deadlock the mount — a `while !ready {}` is exactly the hang it asks about.
const READINESS_WAIT: Duration = Duration::from_secs(5);

/// Run the exec loop until Shutdown (or the command channel closes). `transport` is already
/// connected. Emits venue events on `events`. Blocking, single-threaded — designed to own the
/// `ExecActor` command thread.
pub fn run_exec(
    rx: Receiver<ExecCommand>,
    events: EventSender,
    mut transport: Box<dyn IbkrTransport>,
) {
    let mut mapper = EventMapper::new(IdRegistry::default());
    // Monotonic millisecond origin for the stranded-fill sweep. `EventMapper::sweep_pending` uses
    // only DIFFERENCES of the stamp it is given, so an `Instant` origin is the right source —
    // a wall clock could jump backwards mid-session and freeze the ladder.
    let started = Instant::now();
    let mut last_sweep = started;
    loop {
        // 1) Drain one ready inbound (short blocking poll so we also service the command channel).
        if let Some(inbound) = transport.next_recv(Duration::from_millis(20)) {
            for ev in fold_inbound(&mut mapper, transport.as_mut(), inbound) {
                let _ = events.blocking_send(ev);
            }
        }
        // 2) Poll one command (non-blocking).
        match rx.try_recv() {
            Ok(ExecCommand::Submit(req)) => {
                // vike contract: OrderSubmitted synchronously at submit.
                let _ = events.blocking_send(Event::OrderSubmitted(OrderSubmitted {
                    client_order_id: req.client_order_id.clone(),
                    ts: req.ts,
                }));
                if let Some(reason) = submit_refusal(&mut mapper, transport.as_mut(), &events) {
                    // The venue cannot take this order — synth the terminal HERE rather than place
                    // it and hope. Never touches the transport: on the dead-stream arm the socket is
                    // gone, and a write into it would return `Ok` from the kernel buffer while the
                    // order became unreachable in both directions (`IbInbound::StreamDead`).
                    tracing::error!(
                        coid = %req.client_order_id,
                        symbol = %req.symbol,
                        %reason,
                        "ibkr: REFUSING submit — rejecting the intent instead of placing it"
                    );
                    let _ = events.blocking_send(Event::OrderRejected(OrderRejected {
                        client_order_id: req.client_order_id.clone(),
                        reason: reason.into(),
                        ts: req.ts,
                    }));
                } else if let Some(contract) = parse_simplified(&req.symbol) {
                    let order_id = mapper.ids_mut().next_order_id();
                    mapper.ids_mut().bind(order_id, &req.client_order_id);
                    // Track as unacked so an id-less async rejection (ibapi drops the order id from
                    // order-rejection Notices) can still be attributed back to this order.
                    mapper.on_submit(order_id, req.qty);
                    // price_magnifier 1: Phase-1 STK/CASH; bonds resolve theirs via contractDetails
                    // (Task 9+). No magnifier scaling needed for the mapped equity/forex path.
                    let spec = map_order_request(&req, &contract, 1);
                    transport.place_order(order_id, &spec, &contract);
                } else {
                    // Unmappable symbol → synth terminal reject (the intent must never vanish).
                    let _ = events.blocking_send(Event::OrderRejected(OrderRejected {
                        client_order_id: req.client_order_id.clone(),
                        reason: format!("unmappable IBKR symbol: {}", req.symbol).into(),
                        ts: req.ts,
                    }));
                }
            }
            Ok(ExecCommand::Cancel { client_order_id: coid, .. }) => {
                if mapper.ids().stream_dead() {
                    // Deliberately NOT synthesized as `OrderCanceled`: the stream died, so the order
                    // may well still be RESTING at IB. Claiming a cancel we never delivered would
                    // tell the platform it is flat while the venue holds live exposure — the exact
                    // inversion of the vanishing-order bug, and the more dangerous direction.
                    tracing::error!(
                        coid = %coid,
                        "ibkr: CANNOT CANCEL — the order-update stream is dead, so this cancel \
                         reached no venue. The order's true state is UNKNOWN; cancel it in TWS."
                    );
                } else if let Some(order_id) = mapper.ids_mut().order_id_of(&coid) {
                    transport.cancel_order(order_id);
                }
                // Unknown coid: nothing to cancel here — the ExecActor already surfaces a dead-lane
                // reject; a resolved-but-gone order is handled by the venue's terminal.
            }
            // No bulk-cancel path here, and this venue declares none — so `ExecActor` fanned the
            // batch out into the per-id `Cancel`s above before it ever reached this channel, and
            // this arm is unreachable. It exists because a new command variant is exhaustive; the
            // shared helper refuses every id NON-terminally rather than letting a future mis-wiring
            // drop them silently.
            Ok(ExecCommand::CancelBatch { client_order_ids, .. }) => {
                cancel_batch_undeclared(&events, &client_order_ids)
            }
            Ok(ExecCommand::Modify { order, new_qty, new_price }) => {
                // Resolve the resting order's numeric id from its coid; unknown ⇒ nothing to amend.
                // Backends whose `modify_order` is the default no-op (socket) keep the resting
                // order's terms — nothing is synthesized (supports_modify=false). cpapi overrides
                // `modify_order` with a native amend.
                if mapper.ids().stream_dead() {
                    tracing::error!(
                        coid = %order.client_order_id,
                        "ibkr: CANNOT MODIFY — the order-update stream is dead; amend it in TWS."
                    );
                } else if let Some(order_id) = mapper.ids_mut().order_id_of(&order.client_order_id)
                {
                    if let Some(contract) = parse_simplified(&order.symbol) {
                        // Rebuild the spec from the (possibly resized/repriced) order.
                        let mut amended = (*order).clone();
                        if let Some(q) = new_qty {
                            amended.qty = q;
                        }
                        if let Some(px) = new_price {
                            amended.price = Some(px);
                        }
                        let spec = map_order_request(&amended, &contract, 1);
                        transport.modify_order(order_id, &spec, &contract);
                    }
                }
            }
            Ok(ExecCommand::Shutdown) | Err(TryRecvError::Disconnected) => break,
            Err(TryRecvError::Empty) => {}
        }
        // 3) Stranded-fill sweep: an execution whose commissionReport never arrived emits NO fill
        //    at all (the join is the only emitter), so on a never-cancelled order it would be held
        //    forever and the platform's position/realized PnL would silently run short. Ask IBKR to
        //    re-deliver the day's executions instead of inventing a commission.
        //    The dead-stream gate lives INSIDE `sweep_stranded_fills` rather than in this condition,
        //    so it sits in the function that has unit tests around it and a mutation to it is
        //    directly visible.
        if last_sweep.elapsed() >= PENDING_SWEEP_INTERVAL {
            last_sweep = Instant::now();
            sweep_stranded_fills(
                &mut mapper,
                transport.as_mut(),
                started.elapsed().as_millis() as i64,
                PENDING_COMMISSION_GRACE_MS,
            );
        }
    }
}

/// One stranded-fill sweep pass: age `pending`, re-request the day's executions if anything is past
/// `grace_ms` with retry budget left, and escalate whatever has run out. Split out of the loop (and
/// taking `now_ms`/`grace_ms` as parameters rather than reading a clock) so it is directly
/// unit-testable — see this module's tests.
///
/// `now_ms` is an opaque MONOTONIC stamp; only differences of it are used.
fn sweep_stranded_fills(
    mapper: &mut EventMapper,
    transport: &mut dyn IbkrTransport,
    now_ms: i64,
    grace_ms: i64,
) {
    // A dead stream ends the ladder. `request_executions` is answered THROUGH the pump, so once the
    // pump has exited the replay can never arrive — an ungated sweep would spend the whole finite
    // retry budget on writes nobody can answer, exhausting the one mechanism that could still have
    // recovered those fills after a remount. The executions stay BUFFERED either way (nothing is
    // evicted here), so this forfeits nothing but the futile requests.
    if mapper.ids().stream_dead() {
        return;
    }
    let sweep = mapper.sweep_pending(now_ms, grace_ms);
    if sweep.is_empty() {
        return; // the steady state
    }
    if !sweep.recover.is_empty() {
        tracing::warn!(
            exec_ids = ?sweep.recover,
            grace_ms,
            "ibkr: execution(s) buffered past the commission grace window — re-requesting the \
             day's executions so the fill can be emitted with the REAL commission"
        );
        // ONE account-wide request covers every id in `recover`.
        transport.request_executions();
    }
    for s in sweep.stranded {
        tracing::error!(
            exec_id = %s.exec_id,
            coid = %s.client_order_id,
            order_id = s.order_id,
            symbol = %s.symbol,
            side = s.side,
            qty = s.shares,
            price = s.price,
            attempts = MAX_PENDING_RECOVERY_ATTEMPTS,
            "ibkr: FILL NOT EMITTED — no commissionReport for this execution and the venue replay \
             did not recover it, so position and realized PnL are SHORT by this fill. The \
             execution is still held: a late commissionReport will still emit it correctly."
        );
    }
}

/// Why a submit must be refused right now, or `None` when the venue can take it.
///
/// Two refusals, and they are NOT the same condition — which is the whole point of splitting
/// `IbInbound::StreamResync` into [`crate::transport::IbInbound::StreamResync`] and
/// [`crate::transport::IbInbound::StreamDead`]:
///
/// * **the stream is DEAD** — terminal, immediate, no wait. Nothing will re-establish it, so there
///   is nothing to wait for.
/// * **the handshake has not landed yet** — transient, so this drains inbound for up to
///   [`READINESS_WAIT`] (folding normally, so nothing is dropped on the floor) before deciding.
///   Folding is what makes the wait productive: the very inbounds it is waiting for are the ones it
///   is draining.
///
/// This is the caller `crates/bridges/vike-ibkr/src/id_registry.rs`'s `is_connected` was written
/// for and did not have. It shipped `#[allow(dead_code)]` under a comment promising a "Task 10"
/// caller, while `crates/bridges/vike-ibkr/tests/ibkr_smoke.rs`'s module doc asked a human to
/// confirm against a real Gateway that "the bridge does not accept `submit` before the Gateway has
/// signalled account readiness" — a gate that did not exist, so that item could only ever be
/// confirmed by mistake.
fn submit_refusal(
    mapper: &mut EventMapper,
    transport: &mut dyn IbkrTransport,
    events: &EventSender,
) -> Option<String> {
    if let Some(reason) = mapper.ids().stream_death_reason() {
        return Some(format!(
            "ibkr order-update stream is DEAD ({reason}) — the venue accepts no further orders on \
             this mount; restart it to reconnect"
        ));
    }
    if mapper.ids().is_connected() {
        return None;
    }
    let deadline = Instant::now() + READINESS_WAIT;
    while Instant::now() < deadline {
        if let Some(inbound) = transport.next_recv(Duration::from_millis(20)) {
            for ev in fold_inbound(mapper, transport, inbound) {
                let _ = events.blocking_send(ev);
            }
        }
        if let Some(reason) = mapper.ids().stream_death_reason() {
            return Some(format!("ibkr order-update stream died during the handshake ({reason})"));
        }
        if mapper.ids().is_connected() {
            return None;
        }
    }
    Some(format!(
        "ibkr venue handshake incomplete after {}s (no nextValidId and/or no account list) — \
         refusing rather than placing an order under a guessed order id",
        READINESS_WAIT.as_secs()
    ))
}

/// Fold one normalized inbound through the `EventMapper` into canonical events. The resync, death
/// and id/account signals emit nothing (they mutate the id registry / trigger resync).
fn fold_inbound(
    mapper: &mut EventMapper,
    transport: &mut dyn IbkrTransport,
    inbound: IbInbound,
) -> Vec<Event> {
    match inbound {
        IbInbound::NextValidId(id) => {
            mapper.ids_mut().on_next_valid_id(id);
            vec![]
        }
        IbInbound::AccountsReady => {
            mapper.ids_mut().set_accounts_ready(true);
            vec![]
        }
        IbInbound::OrderStatus(s) => mapper.on_order_status(s),
        IbInbound::ExecDetails(e) => mapper.on_exec_details(e),
        IbInbound::Commission(c) => mapper.on_commission_report(c),
        IbInbound::Error { code, order_id, msg } => mapper.on_error(code, order_id, &msg),
        IbInbound::StreamDead { reason } => {
            // TERMINAL. Latch the venue closed: every later submit is refused with a synthetic
            // reject (`submit_refusal`), cancel/modify are refused loudly, and the stranded-fill
            // sweep stops writing into a socket nobody is reading.
            //
            // ⚠ Why this rather than the reconnect the old `Reconnected` name promised. Reconnecting
            // the socket backend is IMPLEMENTABLE — ibapi's blocking `Client` would have to be
            // rebuilt behind the `Arc` that `place_order`/`cancel_order` hold, a new
            // `order_update_stream` opened, and the coid⇄orderId map rebuilt from `all_open_orders`
            // through `rebind_open_order`, which exists for exactly that. What is missing is not the
            // code, it is the EVIDENCE: no induced-disconnect run has ever been driven against a
            // real Gateway (the socket smoke has never even observed a fill — see `socket.rs`'s
            // module doc), IB re-issues `nextValidId` on a fresh connection so the numbering
            // restarts, and an order placed into a transport that is mid-reconnect is precisely the
            // order this bug loses today. A half-proven reconnect fails in the same direction as the
            // bug and hides it again behind a name that sounds fine.
            //
            // So the venue STOPS, and says so. Reconnect stays available as a separately-provable
            // change; it is not a prerequisite for not lying about the failure.
            //
            // Nothing is synthesized for orders already in flight. Their venue state is genuinely
            // unknown — a fabricated terminal here would tell the platform it is flat while IB may
            // hold live exposure. They are NAMED instead, so an operator has the coid list to take
            // to TWS, and the platform-level answer to an unknown venue state is the reconcile pass.
            let live: Vec<&str> = mapper.live_client_order_ids();
            tracing::error!(
                %reason,
                live_orders = live.len(),
                coids = ?live,
                "ibkr: ORDER-UPDATE STREAM DEAD — this mount will accept NO further orders. Nothing \
                 reconnects: the socket backend has no reconnect and this is its terminal state. \
                 The orders named above were in flight and their venue state is UNKNOWN — check \
                 them in TWS; a remount + reconcile pass is the recovery."
            );
            mapper.ids_mut().mark_stream_dead(&reason);
            vec![]
        }
        IbInbound::StreamResync => {
            // Reconnect resync (Task 10): IB does not replay open orders/executions on its own, so
            // re-request the open-order snapshot; each row lands back here as `IbInbound::OpenOrder`
            // and rebuilds the coid⇄orderId map via `rebind_open_order`.
            transport.request_open_orders();
            // …and the day's EXECUTIONS, which the open-order snapshot structurally cannot cover: a
            // filled order is not an open order. A socket blip between an execDetails and its
            // commissionReport is the dominant way a fill strands unemitted (`event_mapper` module
            // doc trap 5), and a blip that swallows an execDetails outright loses the fill even
            // more completely — both are recovered here, at the exact moment the loss happened,
            // with the REAL commission rather than a fabricated one. Unconditional: the replay is
            // idempotent (the mapper's `emitted` index drops re-delivered halves, and
            // `vike_exec::ExecutionEngine`'s `seen_trade_ids` dedups downstream), and the bridge
            // cannot know what the blip swallowed — `pending` being empty is not evidence of
            // nothing missing, since a lost execDetails leaves no trace at all.
            transport.request_executions();
            vec![]
        }
        IbInbound::OpenOrder { order_id, order_ref } => {
            // Forward any synthesized OrderAccepted: a mid-flight order whose accept was lost in the
            // socket blip is unstuck here (no-order-vanishes). External orders return empty.
            mapper.rebind_open_order(order_id, &order_ref)
        }
    }
}

/// Production connect: build the socket transport and hand it to the exec loop. With the
/// `ibkr-socket` feature ON, this connects the vendored blocking `ibapi` client to the Gateway/TWS
/// named by `cfg`; with it OFF, it degrades to `Unavailable` — the app root then keeps the venue
/// paper, exactly like the dukascopy missing-sidecar gate. (`events` is reserved for a future
/// connect-time synthetic emit; the transport delivers all venue events through its own pump.)
#[cfg(feature = "ibkr-socket")]
pub(crate) fn connect_socket(
    cfg: &crate::config::IbkrConfig,
    _events: &EventSender,
) -> Result<Box<dyn IbkrTransport>, crate::error::IbkrError> {
    let transport = crate::transport::SocketTransport::connect(cfg)?;
    Ok(Box::new(transport))
}

/// Feature-off variant: no socket backend compiled → always `Unavailable` (stay paper).
#[cfg(not(feature = "ibkr-socket"))]
pub(crate) fn connect_socket(
    _cfg: &crate::config::IbkrConfig,
    _events: &EventSender,
) -> Result<Box<dyn IbkrTransport>, crate::error::IbkrError> {
    Err(crate::error::IbkrError::Unavailable)
}

/// Production connect for the cpapi backend: build the `CpapiTransport` (REST + WS pump to a
/// running, browser-authenticated Client Portal Gateway) and hand it to the exec loop. With the
/// `ibkr-cpapi` feature OFF, or an unreachable/unauthenticated gateway, it degrades to
/// `Unavailable` → the venue stays paper (same gate as the socket path).
#[cfg(feature = "ibkr-cpapi")]
pub(crate) fn connect_cpapi(
    cfg: &crate::config::IbkrConfig,
    _events: &EventSender,
) -> Result<Box<dyn IbkrTransport>, crate::error::IbkrError> {
    let transport = crate::transport::CpapiTransport::connect(cfg)?;
    Ok(Box::new(transport))
}

/// Feature-off variant: no cpapi backend compiled → always `Unavailable` (stay paper).
#[cfg(not(feature = "ibkr-cpapi"))]
pub(crate) fn connect_cpapi(
    _cfg: &crate::config::IbkrConfig,
    _events: &EventSender,
) -> Result<Box<dyn IbkrTransport>, crate::error::IbkrError> {
    Err(crate::error::IbkrError::Unavailable)
}

#[cfg(test)]
mod tests {
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
        let _ =
            fold_inbound(&mut dead_m, &mut dead_t, IbInbound::StreamDead { reason: "x".into() });

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
}
