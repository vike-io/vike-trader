//! The exec loop: drains `ExecCommand` (submit/cancel/modify/shutdown), maps to transport calls,
//! and pumps transport inbound through the `EventMapper` onto the `EventSender` lane. Reused by the
//! socket backend (Task 9) and the `FakeTransport` double. `OrderSubmitted` is emitted
//! synchronously at submit (the vike venue-adapter contract); the venue-side
//! Accepted/Filled/Rejected/Canceled events fold in from the transport inbound.
//!
//! ## Learning the venue (a fresh socket connection)
//!
//! `IbInbound::Connected` makes the loop LEARN the venue: it asks for the open orders, then (when the
//! venue says that snapshot ended) for the day's executions, then (when those ended) announces the
//! link as UP (`ExecLink{up: true}`). The learned state is what lets this connection cancel and follow
//! an order a previous connection of this client placed. A request that fails or does not end leaves the
//! connection NOT learned and the link NOT announced up: the host decides what that means (the gateway
//! daemon holds the session). A failed learning is asked for again (`LEARN_RETRY_BACKOFF`: three attempts in all)
//! before it is given up. The executions replay is held back until the HOST has handed the client over (`Taught`:
//! its first `adopt_restored`), so the fills it brings carry the symbols the host taught. The loop never waits for
//! any of it; a submit waits a bounded time for the open orders (`LEARN_WAIT`).

use std::sync::mpsc::{Receiver, TryRecvError};
use std::time::{Duration, Instant};

use ustr::Ustr;
use vike_bridge_core::exec_actor::{ExecCommand, cancel_batch_undeclared};
use vike_exec::{EventSender, ExecLinkUpdate};
use vike_model::OrderRequest;
use vike_model::events::{
    Event, OrderCancelRejected, OrderModifyRejected, OrderRejected, OrderSubmitted,
};

use crate::contract::parse_simplified;
use crate::exec::event_mapper::{
    CANCEL_COMMISSION_WAIT_MS, EventMapper, LearnStep, Learning, MAX_PENDING_RECOVERY_ATTEMPTS,
    PENDING_COMMISSION_GRACE_MS,
};
use crate::exec::id_registry::IdRegistry;
use crate::order::map_order_request;
use crate::transport::{IbInbound, IbkrTransport, Snapshot, is_undecoded_tagged};

pub(crate) mod event_mapper;
pub(crate) mod id_registry;

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
/// cannot hang or deadlock the start — a `while !ready {}` is exactly the hang it asks about.
const READINESS_WAIT: Duration = Duration::from_secs(5);

/// The most a fresh connection's LEARNING of the venue (the open orders, then the day's executions) may
/// take before the exec loop gives it up as failed. The transport bounds each request itself and says so
/// (`IbInbound::SnapshotEnd { complete: false }`); this is the backstop for a transport that never says
/// anything, so a silent request cannot leave the bridge "learning" for ever.
const LEARN_DEADLINE: Duration = Duration::from_secs(30);

/// The most a SUBMIT, or a cancel/modify of a coid the connection does not know yet, waits for the learning
/// to finish (review 18, R5). A submit placed before the connection knows the orders it left would rest on
/// an id the venue may already use (an id sequence that restarted below a resting order's id would AMEND
/// it), and a cancel right after a restart must not be refused for a window it can simply wait out. Bounded
/// and small: past it a submit is refused terminally while the venue's open orders are still unknown (`EventMapper::orders_unknown`) and a
/// cancel of an unknown coid is answered with the advisory and retried once when the learning ends, never held for ever.
const LEARN_WAIT: Duration = Duration::from_secs(3);

/// How long the loop holds the learning's executions replay for the HOST to hand the client over (its first
/// `adopt_restored`, even an empty one), so the fills the replay brings of orders the host knew carry the
/// symbols it teaches. A fail-safe only: the gatewayhub teaches at once, right after the start returns, and a
/// host that never does gets its replay (untaught, with an `error!`) after this long. Counted from the moment
/// the replay is due, so a slow start (the grid pre-fetch dials the gateway before the client is handed over)
/// is inside it.
const TEACH_WAIT: Duration = Duration::from_secs(20);

/// The waits before each RETRY of a learning that failed (a request errored, did not end, or returned rows that
/// could not be attributed): three attempts in all, then the connection gives up and stays closed to orders
/// (its host holds the session). Bounded: nothing here retries for ever.
const LEARN_RETRY_BACKOFF: &[Duration] = &[Duration::from_secs(2), Duration::from_secs(5)];

/// The reason on the terminal rejection of a submit placed while the connection does not know the venue's open
/// orders (learning still running after `LEARN_WAIT`, or failed in its first phase). ONE line (`concat!` of
/// literals: a `\` line continuation here once turned into literal newlines and runs of spaces in the reason;
/// `exec_tests.rs`'s `the_unlearned_refusal_is_one_plain_line` pins it).
const UNLEARNED_REASON: &str = concat!(
    "ibkr: this connection has not learned the orders this client already has at the venue, so an order id ",
    "handed out now could be a resting order's and would amend it; the order was NOT sent. If the learning is ",
    "still running, retry when the connection reports itself up; if it FAILED (the journal has an ibkr error ",
    "line saying why) it stays closed to orders: clear foreign or stale orders in TWS, then restart the ",
    "gatewayhub to rebuild the session"
);

/// The reason on the non-terminal refusal of a cancel or amend of a client order id this connection
/// does not know. It says what is true: nothing was sent, and the order's state is not ours to claim.
const UNKNOWN_ORDER_REASON: &str = "ibkr: no venue order is known for this client order id on \
     this connection (it did not rest here, it finished, or the connection has not learned the \
     venue's open orders); the request was NOT sent";

/// The waits of one run of the loop; a test shortens them.
#[derive(Clone, Copy)]
struct LoopTimes {
    /// The backstop for a learning nothing ends.
    learn_deadline: Duration,
    /// How long a command waits for the learning.
    learn_wait: Duration,
    /// How long the executions replay waits for the host to hand the client over.
    teach_wait: Duration,
    /// The waits before each retry of a failed learning.
    retry: &'static [Duration],
}

impl LoopTimes {
    const PRODUCTION: LoopTimes = LoopTimes {
        learn_deadline: LEARN_DEADLINE,
        learn_wait: LEARN_WAIT,
        teach_wait: TEACH_WAIT,
        retry: LEARN_RETRY_BACKOFF,
    };
}

/// Run the exec loop until Shutdown (or the command channel closes). `transport` is already
/// connected. Emits venue events on `events`. Blocking, single-threaded — designed to own the
/// `ExecActor` command thread.
pub(crate) fn run_exec(
    rx: Receiver<ExecCommand>,
    events: EventSender,
    transport: Box<dyn IbkrTransport>,
    taught: Taught,
) {
    run_loop_with(rx, events, transport, taught, LoopTimes::PRODUCTION);
}

/// [`run_exec`] with the learning backstop and the commands' wait for it as parameters (a test shortens them),
/// no retry of a failed learning and the production wait for the host's handover.
#[cfg(test)]
fn run_loop(
    rx: Receiver<ExecCommand>,
    events: EventSender,
    transport: Box<dyn IbkrTransport>,
    taught: Taught,
    learn_deadline: Duration,
    learn_wait: Duration,
) {
    let times = LoopTimes { learn_deadline, learn_wait, teach_wait: TEACH_WAIT, retry: &[] };
    run_loop_with(rx, events, transport, taught, times);
}

fn run_loop_with(
    rx: Receiver<ExecCommand>,
    events: EventSender,
    mut transport: Box<dyn IbkrTransport>,
    taught: Taught,
    times: LoopTimes,
) {
    let LoopTimes { learn_deadline, learn_wait, .. } = times;
    let mut mapper = EventMapper::new(IdRegistry::default());
    // Since when the executions replay has been waiting for the host to hand the client over.
    let mut owed_since: Option<Instant> = None;
    // The retries of a failed learning used so far, and when the next one is due.
    let mut retries_used: usize = 0;
    let mut retry_at: Option<Instant> = None;
    let mut gave_up_said = false;
    // Monotonic millisecond origin for the stranded-fill sweep. `EventMapper::sweep_pending` uses
    // only DIFFERENCES of the stamp it is given, so an `Instant` origin is the right source —
    // a wall clock could jump backwards mid-session and freeze the ladder.
    let started = Instant::now();
    let mut last_sweep = started;
    // Since when the connection has been LEARNING the venue (open orders, then executions), for the
    // backstop below.
    let mut learning_since: Option<Instant> = None;
    // Cancels and amends of a coid the connection did not know while it was still learning the venue: retried
    // ONCE when the learning ends.
    let mut deferred: Vec<Deferred> = Vec::new();
    loop {
        // 0) What the host taught about restored orders (`IbkrExecutionClient::adopt_restored`). Whether the
        //    host has handed the client over is read BEFORE the pairs are taken: a handover seen here has its
        //    pairs parked already.
        let handed_over = taught.handed_over();
        for (coid, symbol) in taught.take() {
            mapper.register_symbol(&coid, &symbol);
        }
        //    The learning's executions replay waits for that handover: the fills it brings are booked under
        //    the symbols taught here, and a replay folded before them is dropped by the engine for good.
        mapper.hold_executions(!handed_over);
        if mapper.take_owed_executions() {
            transport.request_executions();
        }
        let awaiting_teach = mapper.executions_owed();
        taught.set_awaiting(awaiting_teach);
        if awaiting_teach {
            let since = *owed_since.get_or_insert_with(Instant::now);
            if since.elapsed() >= times.teach_wait {
                tracing::error!(
                    "ibkr: the host did not hand this client over (no `adopt_restored`) within {}s of the \
                     open orders being known: the executions replay goes ahead without the symbols of the \
                     orders the host knew, so their fills may be dropped by the engine; reconcile finds them",
                    times.teach_wait.as_secs_f32()
                );
                taught.release();
            }
        } else {
            owed_since = None;
        }
        // 1) Drain one ready inbound (short blocking poll so we also service the command channel).
        if let Some(inbound) = transport.next_recv(Duration::from_millis(20)) {
            deliver(&mut mapper, transport.as_mut(), &events, inbound);
        }
        // The backstop for a transport that never ends a snapshot request (it normally says so itself):
        // learning that has not finished in `learn_deadline` is FAILED, not pending for ever.
        let learning_now = matches!(mapper.learning(), Learning::OpenOrders | Learning::Executions);
        if !learning_now || awaiting_teach {
            learning_since = None;
        } else if learning_since.get_or_insert_with(Instant::now).elapsed() >= learn_deadline {
            learning_since = None;
            mapper.abandon_learning();
            tracing::error!(
                "ibkr: the venue's open orders were NOT learned: no snapshot answer in {}s. Orders \
                 this client placed before this connection can be neither cancelled nor followed \
                 here; check them in TWS.",
                learn_deadline.as_secs_f32()
            );
        }
        // A learning that FAILED is tried again, a few times with growing waits, before the connection gives it
        // up: a request that errored or an unreadable snapshot is often gone a moment later.
        if mapper.learning() == Learning::Failed && !mapper.ids().stream_dead() {
            match times.retry.get(retries_used) {
                Some(wait) => {
                    let due = *retry_at.get_or_insert_with(|| Instant::now() + *wait);
                    if Instant::now() >= due {
                        retry_at = None;
                        retries_used += 1;
                        tracing::warn!(
                            attempt = retries_used + 1,
                            of = times.retry.len() + 1,
                            "ibkr: learning the venue's open orders AGAIN after a failed attempt"
                        );
                        mapper.begin_learning();
                        transport.request_open_orders();
                    }
                }
                None if !gave_up_said => {
                    gave_up_said = true;
                    tracing::error!(
                        attempts = times.retry.len() + 1,
                        "ibkr: GAVE UP learning the venue's open orders: this connection stays closed to \
                         orders and cannot manage what this client placed before it. Clear foreign or stale \
                         orders in TWS and restart the gatewayhub to rebuild the session"
                    );
                }
                None => {}
            }
        }
        retry_deferred(&mut mapper, transport.as_mut(), &events, &mut deferred);
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
                } else {
                    settle_learning(&mut mapper, transport.as_mut(), &events, learn_wait);
                    if mapper.orders_unknown() {
                        // The wait is over and the connection still does not know which orders of this
                        // client rest at the venue (learning is running, or failed on its first request).
                        // An id handed out now could be a resting order's, and in IB's model that is an
                        // AMEND of it. Refuse the intent, terminally and with words, instead.
                        tracing::error!(
                            coid = %req.client_order_id,
                            learning = ?mapper.learning(),
                            "ibkr: REFUSING submit - this connection has not learned the venue's open orders"
                        );
                        let _ = events.blocking_send(Event::OrderRejected(OrderRejected {
                            client_order_id: req.client_order_id.clone(),
                            reason: UNLEARNED_REASON.into(),
                            ts: req.ts,
                        }));
                        continue;
                    }
                    match parse_simplified(&req.symbol) {
                        Ok(contract) => {
                            let Some(order_id) = mapper.ids_mut().next_order_id() else {
                                // The id space is spent: placing under a reused id would AMEND a live
                                // order. Refuse the intent instead, terminally.
                                tracing::error!(
                                    coid = %req.client_order_id,
                                    "ibkr: REFUSING submit — this connection has no order id left"
                                );
                                let _ = events.blocking_send(Event::OrderRejected(OrderRejected {
                                    client_order_id: req.client_order_id.clone(),
                                    reason: "ibkr: this connection has used every order id it can \
                                             allocate; restart it"
                                        .into(),
                                    ts: req.ts,
                                }));
                                continue;
                            };
                            mapper.ids_mut().bind(order_id, &req.client_order_id);
                            // Track as unacked so an id-less async rejection (ibapi drops the order
                            // id from order-rejection Notices) can still be attributed back here.
                            mapper.on_submit(order_id, req.qty);
                            // The fills of this order carry THIS symbol, not the instrument IB echoes on
                            // the execution (see `EventMapper::fill_symbol`).
                            mapper.register_symbol(&req.client_order_id, &req.symbol);
                            // price_magnifier 1: bonds resolve theirs via contractDetails (Task
                            // 9+). No magnifier scaling on the equity/forex/derivative paths.
                            let spec = map_order_request(&req, &contract, 1);
                            transport.place_order(order_id, &spec, &contract);
                        }
                        // The canonical does not name exactly one contract → synth the terminal
                        // reject (the intent must never vanish) and carry the PARSER's own
                        // sentence through as the reason, so the operator is told which field
                        // could not be read and which spellings this venue accepts. ⚠ This arm
                        // used to be the SILENT EQUITY DEFAULT's other half: it fired only on a
                        // malformed canonical, because `parse_simplified` answered `Some(STK)` for
                        // every well-formed one it did not recognise — so `ESZ5.GLOBEX.USD` was
                        // placed as a stock rather than arriving here.
                        Err(why) => {
                            tracing::error!(
                                coid = %req.client_order_id,
                                symbol = %req.symbol,
                                reason = %why,
                                "ibkr: REFUSING submit — the symbol does not name one contract"
                            );
                            let _ = events.blocking_send(Event::OrderRejected(OrderRejected {
                                client_order_id: req.client_order_id.clone(),
                                reason: why.into(),
                                ts: req.ts,
                            }));
                        }
                    }
                }
            }
            Ok(ExecCommand::Cancel { client_order_id: coid, .. }) => {
                if mapper.ids().order_id_of(&coid).is_none() {
                    // Not known (yet): if the connection is still learning the venue, wait for it.
                    settle_learning(&mut mapper, transport.as_mut(), &events, learn_wait);
                }
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
                } else {
                    // Unknown coid: NOTHING is sent now, and the caller is told so. It used to be a silent
                    // no-op (the ExecActor surfaces only a dead-LANE reject), which left the caller
                    // believing an order was being cancelled while it may rest at IB. The advisory is
                    // NON-terminal: the order's state is not ours to claim.
                    if mapper.recently_finished(&coid) {
                        // A race the cancel lost: the order reached its terminal on this connection moments
                        // ago. Not the alarm an unknown order is.
                        tracing::info!(
                            coid = %coid,
                            "ibkr: cancel of an order that has just finished - nothing to cancel"
                        );
                    } else {
                        tracing::error!(
                            coid = %coid,
                            learning = ?mapper.learning(),
                            "ibkr: CANNOT CANCEL — no venue order is known for this client order id; \
                             nothing was sent"
                        );
                    }
                    let _ = events.blocking_send(Event::OrderCancelRejected(OrderCancelRejected {
                        client_order_id: coid.clone(),
                        reason: UNKNOWN_ORDER_REASON.into(),
                        ts: 0,
                    }));
                    // The connection is still learning (the wait ran out): ONE retry when it ends, which is
                    // when an order the venue still holds becomes known.
                    if still_learning(&mapper) {
                        deferred.push(Deferred::Cancel(coid));
                    }
                }
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
                if mapper.ids().order_id_of(&order.client_order_id).is_none() {
                    settle_learning(&mut mapper, transport.as_mut(), &events, learn_wait);
                }
                match amend(&mut mapper, transport.as_mut(), &order, new_qty, new_price) {
                    Amended::Unknown => {
                        tracing::error!(
                            coid = %order.client_order_id,
                            learning = ?mapper.learning(),
                            "ibkr: CANNOT MODIFY — no venue order is known for this client order id; \
                             nothing was sent"
                        );
                        let _ =
                            events.blocking_send(Event::OrderModifyRejected(OrderModifyRejected {
                                client_order_id: order.client_order_id.clone(),
                                reason: UNKNOWN_ORDER_REASON.into(),
                                ts: order.ts,
                            }));
                        if still_learning(&mapper) {
                            deferred.push(Deferred::Modify { order, new_qty, new_price });
                        }
                    }
                    Amended::Handled => {}
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
            sweep_tick(
                &mut mapper,
                transport.as_mut(),
                &events,
                started.elapsed().as_millis() as i64,
                PENDING_COMMISSION_GRACE_MS,
                CANCEL_COMMISSION_WAIT_MS,
            );
        }
    }
}

/// What the HOST taught this client about orders it restored (`ExecutionClient::adopt_restored`: the pairs of
/// client order id and the unified symbol the engine registered the order under). The call comes from another
/// thread than the exec loop's, so it is parked here and the loop drains it ([`run_loop_with`]'s head). The
/// symbols let the fills of an order this connection did not place itself (rebound from the venue's open
/// orders after a rebuild) carry the symbol the engine knows the order by.
///
/// It is also the HANDOVER: the first `adopt_restored` (even an empty one) tells the loop the host has the
/// client in hand and has taught what it knows. A client built for a host ([`Taught::gated`]) holds the
/// learning's executions replay until then, so the replay never runs ahead of the teaching; one built with
/// [`Taught::default`] (the test door) has no such wait.
#[derive(Clone)]
pub(crate) struct Taught(std::sync::Arc<TaughtShared>);

struct TaughtShared {
    orders: std::sync::Mutex<Vec<(String, String)>>,
    handed_over: std::sync::atomic::AtomicBool,
    /// The loop is holding the executions replay for the handover (a test reads it).
    awaiting: std::sync::atomic::AtomicBool,
}

impl Default for Taught {
    fn default() -> Self {
        Taught::new(true)
    }
}

impl Taught {
    fn new(handed_over: bool) -> Self {
        Taught(std::sync::Arc::new(TaughtShared {
            orders: std::sync::Mutex::new(Vec::new()),
            handed_over: std::sync::atomic::AtomicBool::new(handed_over),
            awaiting: std::sync::atomic::AtomicBool::new(false),
        }))
    }

    /// A handover still to come: the executions replay of the learning waits for the host's first teaching.
    pub(crate) fn gated() -> Self {
        Taught::new(false)
    }

    /// Park what the host taught and mark the client handed over. Returns at once (the host's contract: no
    /// network, no waiting).
    pub(crate) fn teach(&self, orders: &[(String, String)]) {
        let mut parked = self.0.orders.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        parked.extend(orders.iter().cloned());
        drop(parked);
        // After the pairs are parked: a loop that reads `true` finds them.
        self.0.handed_over.store(true, std::sync::atomic::Ordering::SeqCst);
    }

    fn take(&self) -> Vec<(String, String)> {
        let mut parked = self.0.orders.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        std::mem::take(&mut *parked)
    }

    fn handed_over(&self) -> bool {
        self.0.handed_over.load(std::sync::atomic::Ordering::SeqCst)
    }

    /// The fail-safe: the host did not come, go ahead without it.
    fn release(&self) {
        self.0.handed_over.store(true, std::sync::atomic::Ordering::SeqCst);
    }

    fn set_awaiting(&self, awaiting: bool) {
        self.0.awaiting.store(awaiting, std::sync::atomic::Ordering::SeqCst);
    }

    /// Whether the loop holds the executions replay for the host's handover now.
    #[cfg(any(test, feature = "test-support"))]
    pub(crate) fn is_awaiting(&self) -> bool {
        self.0.awaiting.load(std::sync::atomic::Ordering::SeqCst)
    }
}

/// A cancel or amend of a coid the connection did not know while it was still learning the venue, kept for
/// ONE retry when the learning ends (the caller has already been told the request was not sent).
enum Deferred {
    Cancel(String),
    Modify { order: Box<OrderRequest>, new_qty: Option<f64>, new_price: Option<f64> },
}

/// Whether the connection is in the middle of learning the venue.
fn still_learning(mapper: &EventMapper) -> bool {
    matches!(mapper.learning(), Learning::OpenOrders | Learning::Executions)
}

/// What [`amend`] did.
enum Amended {
    /// No venue order is known for the coid: nothing was sent, and the caller owes the advisory.
    Unknown,
    /// Sent, or refused loudly (dead stream, unreadable symbol): nothing more is owed.
    Handled,
}

/// Amend the resting order of `order` in place. Backends whose `modify_order` is the default no-op (socket)
/// keep the resting order's terms.
fn amend(
    mapper: &mut EventMapper,
    transport: &mut dyn IbkrTransport,
    order: &OrderRequest,
    new_qty: Option<f64>,
    new_price: Option<f64>,
) -> Amended {
    if mapper.ids().stream_dead() {
        tracing::error!(
            coid = %order.client_order_id,
            "ibkr: CANNOT MODIFY — the order-update stream is dead; amend it in TWS."
        );
        return Amended::Handled;
    }
    let Some(order_id) = mapper.ids_mut().order_id_of(&order.client_order_id) else {
        return Amended::Unknown;
    };
    match parse_simplified(&order.symbol) {
        Ok(contract) => {
            // Rebuild the spec from the (possibly resized/repriced) order.
            let mut amended = order.clone();
            if let Some(q) = new_qty {
                amended.qty = q;
            }
            if let Some(px) = new_price {
                amended.price = Some(px);
            }
            let spec = map_order_request(&amended, &contract, 1);
            transport.modify_order(order_id, &spec, &contract);
        }
        // An amend whose symbol does not name one contract keeps the RESTING order
        // exactly as it is — the safe direction — but it must not do so in silence:
        // the caller believes the order was repriced. Nothing terminal is
        // synthesized, for the same reason the dead-stream arm above synthesizes
        // nothing: the order is genuinely still live at IB.
        Err(why) => tracing::error!(
            coid = %order.client_order_id,
            symbol = %order.symbol,
            reason = %why,
            "ibkr: CANNOT MODIFY — the symbol does not name one contract; the \
             resting order is UNCHANGED"
        ),
    }
    Amended::Handled
}

/// The ONE retry of the cancels and amends [`Deferred`] while the connection learned the venue, once that is
/// over: an order the venue still held is known now. Nothing is sent twice for an order that became known and
/// was cancelled in between (the second cancel of a gone order is the usual unknown-order advisory).
fn retry_deferred(
    mapper: &mut EventMapper,
    transport: &mut dyn IbkrTransport,
    events: &EventSender,
    deferred: &mut Vec<Deferred>,
) {
    if deferred.is_empty() || still_learning(mapper) {
        return;
    }
    let learned = mapper.learning() == Learning::Done;
    for d in std::mem::take(deferred) {
        match d {
            Deferred::Cancel(coid) => {
                if !learned {
                    tracing::error!(
                        coid = %coid,
                        "ibkr: the cancel of this order was not retried: the connection never learned the \
                         venue's open orders; cancel it in TWS"
                    );
                } else if mapper.ids().stream_dead() {
                    tracing::error!(coid = %coid, "ibkr: the deferred cancel was not sent: the stream is dead");
                } else if let Some(order_id) = mapper.ids().order_id_of(&coid) {
                    tracing::info!(coid = %coid, order_id, "ibkr: retrying a cancel deferred during the learning");
                    transport.cancel_order(order_id);
                } else {
                    tracing::info!(
                        coid = %coid,
                        "ibkr: a cancel deferred during the learning: the venue does not hold the order \
                         (it finished); nothing to cancel"
                    );
                }
            }
            Deferred::Modify { order, new_qty, new_price } => {
                if !learned {
                    tracing::error!(
                        coid = %order.client_order_id,
                        "ibkr: the amend of this order was not retried: the connection never learned the \
                         venue's open orders; amend it in TWS"
                    );
                } else if let Amended::Unknown =
                    amend(mapper, transport, &order, new_qty, new_price)
                {
                    tracing::info!(
                        coid = %order.client_order_id,
                        "ibkr: an amend deferred during the learning: the venue does not hold the order"
                    );
                }
            }
        }
    }
    let _ = events;
}

/// One pass of the loop's periodic upkeep: the cancels held back for a commission that is not coming go out
/// (also with the stream dead: no commission can arrive then), then the stranded-fill sweep. Takes the clock
/// and both bounds as parameters so a test drives it without waiting.
fn sweep_tick(
    mapper: &mut EventMapper,
    transport: &mut dyn IbkrTransport,
    events: &EventSender,
    now_ms: i64,
    grace_ms: i64,
    cancel_wait_ms: i64,
) {
    for ev in mapper.release_held_cancels(now_ms, cancel_wait_ms) {
        let _ = events.blocking_send(ev);
    }
    sweep_stranded_fills(mapper, transport, now_ms, grace_ms);
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
    // recovered those fills on a rebuilt connection. The executions stay BUFFERED either way (nothing is
    // evicted here), so this forfeits nothing but the futile requests.
    if mapper.ids().stream_dead() {
        return;
    }
    // Not while the connection LEARNS the venue: the sweep's own executions request would end into the
    // learning's `SnapshotEnd` (`EventMapper::snapshot_ended` cannot tell the two requests apart) and close the
    // learning before ITS replay finished. The entries stay buffered and are swept once the learning is over.
    if still_learning(mapper) {
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
///   is draining. They go through [`deliver`] like every other inbound, so a stream that dies INSIDE
///   the wait is disclosed to the host like one that dies in the loop.
///
/// This is the caller `crates/bridges/vike-ibkr/src/exec/id_registry.rs`'s `is_connected` was written
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
            deliver(mapper, transport, events, inbound);
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

/// While the connection is still LEARNING the venue, fold inbound for at most `within` or until it has
/// finished (or the stream died), so a command that depends on what it learns sees it. Everything folded
/// goes through [`deliver`]. A no-op once the learning is over (or was never started).
fn settle_learning(
    mapper: &mut EventMapper,
    transport: &mut dyn IbkrTransport,
    events: &EventSender,
    within: Duration,
) {
    let learning =
        |m: &EventMapper| matches!(m.learning(), Learning::OpenOrders | Learning::Executions);
    if !learning(mapper) {
        return;
    }
    let deadline = Instant::now() + within;
    while learning(mapper) && !mapper.ids().stream_dead() && Instant::now() < deadline {
        if let Some(inbound) = transport.next_recv(Duration::from_millis(20)) {
            deliver(mapper, transport, events, inbound);
        }
    }
}

/// Fold one inbound and send what it produced. The ONE door every inbound takes (the loop's own drain
/// and the submit gate's readiness wait both use it), so the link transitions are disclosed from one
/// place: the fold that turns the stream dead tells the lane (`up: false`), once, and the fold that
/// finishes LEARNING the venue tells it the link is up (`up: true`), once.
fn deliver(
    mapper: &mut EventMapper,
    transport: &mut dyn IbkrTransport,
    events: &EventSender,
    inbound: IbInbound,
) {
    let was_dead = mapper.ids().stream_dead();
    let was_done = mapper.learning() == Learning::Done;
    for ev in fold_inbound(mapper, transport, inbound) {
        let _ = events.blocking_send(ev);
    }
    if !was_dead && mapper.ids().stream_dead() {
        announce_link(events, false);
    }
    if !was_done && mapper.learning() == Learning::Done && !mapper.ids().stream_dead() {
        announce_link(events, true);
    }
}

/// Tell whoever hosts this adapter about its venue link, as the `ExecLink` the lane defines for exactly
/// this ([`vike_exec::ExecLinkUpdate`]), stamped with the account's route key (the mount's `routed` lane
/// carries it; a bare lane means the venue's default account).
///
/// * `up: false` is the ONE transition of this adapter's death: it never reconnects (a rebuilt
///   connection is a NEW adapter). Without it the host learned of the death only by probing the port, up
///   to a probe period late, and the orders it sent in that window went into a socket nobody reads.
/// * `up: true` means more than "connected": it is sent when the connection has LEARNED the venue (the
///   open orders rebound, the day's executions replayed), never earlier. A host that holds orders back
///   from a rebuilt bridge lifts the hold on it.
///
/// ⚠ The host MUST consume `Ingest::ExecLink` itself and not hand it to a core, which reads it as a
/// trade link whose watchdog it must defer: a core latches its gate down on the `up: false` and nothing
/// brings it back. The only host that mounts this bridge is the gateway daemon, which consumes it
/// (`crates/vike-gatewayhub/src/trade/session.rs`'s forwarder, pinned by the gatewayhub's tests);
/// `vike-gatewayhub` is the only crate that depends on this one.
fn announce_link(events: &EventSender, up: bool) {
    let venue = Ustr::from("ibkr");
    let update = ExecLinkUpdate {
        route_key: events.route_key.unwrap_or(venue),
        venue,
        up,
        at_ts_ms: vike_model::now_ms(),
    };
    if events.exec_link(update).is_err() {
        tracing::debug!(
            up,
            "ibkr: the link transition was not disclosed: the event lane is closed"
        );
    }
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
        IbInbound::ExecDetails(mut e) => {
            // While the connection learns the venue, an execution of THIS client's order (by client id)
            // that no order of this connection explains is a fill the outage swallowed: recovered by its
            // orderRef as a partial (`EventMapper::on_exec_details`).
            e.recoverable = mapper.learning() == Learning::Executions
                && e.client_id != 0
                && transport.ownership().is_some_and(|own| own.client_id == e.client_id);
            mapper.on_exec_details(e)
        }
        IbInbound::Commission(c) => mapper.on_commission_report(c),
        IbInbound::Error { code, order_id, msg } => mapper.on_error(code, order_id, &msg),
        IbInbound::StreamDead { reason } => {
            // TERMINAL. Latch the venue closed: every later submit is refused with a synthetic
            // reject (`submit_refusal`), cancel/modify are refused loudly, and the stranded-fill
            // sweep stops writing into a socket nobody is reading.
            //
            // ⚠ Why this rather than the reconnect the old `Reconnected` name promised. Reconnecting
            // the socket backend IN PLACE is implementable — ibapi's blocking `Client` would have to be
            // rebuilt behind the `Arc` that `place_order`/`cancel_order` hold, a new
            // `order_update_stream` opened, and the coid⇄orderId map rebuilt from `all_open_orders`
            // through `rebind_open_order`, which exists for exactly that. What is missing is not the
            // code, it is the EVIDENCE: no induced-disconnect run has ever been driven against a
            // real Gateway, IB re-issues `nextValidId` on a fresh connection so the numbering
            // restarts, and an order placed into a transport that is mid-reconnect is precisely the
            // order this bug loses today. A half-proven reconnect fails in the same direction as the
            // bug and hides it again behind a name that sounds fine.
            //
            // So the venue STOPS, and says so — to its log and, through `deliver`, to its host
            // (`ExecLink{up: false}`), which rebuilds the whole connection (a NEW adapter that learns the
            // venue again). Reconnect stays available as a separately-provable change.
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
                "ibkr: ORDER-UPDATE STREAM DEAD — this connection will accept NO further orders. \
                 Nothing reconnects it: the socket backend has no reconnect and this is its terminal \
                 state (its host rebuilds the whole connection). The orders named above were in \
                 flight and their venue state is UNKNOWN — check them in TWS; a rebuilt connection \
                 learns the ones still resting, and a reconcile pass finds the rest."
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
        IbInbound::VenueOrder(row) => {
            // One row of the VENUE's open orders (`reqAllOpenOrders`, or an `openOrder` update). It
            // answers for EVERY client of the login: an order made by hand in TWS (client id 0, no
            // orderRef), another API client's, another account's. Only an order THIS client placed is
            // bound; anything else changes nothing (review of W14b, finding 1).
            let ours = match transport.ownership() {
                Some(own) => own.owns(&row),
                // A transport that cannot say whose orders are whose binds nothing from the venue.
                None => false,
            };
            mapper.note_row(ours, !row.order_ref.is_empty(), is_undecoded_tagged(&row));
            if !ours {
                tracing::debug!(
                    order_id = row.order_id,
                    client_id = row.client_id,
                    "ibkr: open-order row of another client, account or of TWS — not ours, ignored"
                );
                return vec![];
            }
            mapper.on_venue_order(&row)
        }
        IbInbound::Connected => {
            // A fresh connection: LEARN the venue. IB replays nothing on its own, so the open orders
            // are asked for now (this connection's id map is empty); the executions are asked for when
            // THEIR rows have all arrived (`SnapshotEnd`), because a replayed execution only resolves
            // to an order that is already bound.
            mapper.begin_learning();
            transport.request_open_orders();
            vec![]
        }
        IbInbound::SnapshotEnd { snapshot, complete } => {
            match mapper.snapshot_ended(snapshot, complete) {
                LearnStep::Ignore => {}
                LearnStep::RequestExecutions => {
                    // Held back while the host has not handed the client over (the loop asks for it then).
                    if mapper.executions_due() {
                        transport.request_executions();
                    }
                }
                LearnStep::Done => {
                    let stats = mapper.learn_stats();
                    tracing::info!(
                        rows = stats.rows,
                        ours = stats.ours,
                        foreign_with_ref = stats.foreign_with_ref,
                        undecoded_with_ref = stats.undecoded_with_ref,
                        bound = ?stats.bound,
                        "ibkr: the venue's open orders and the day's executions are learned; this \
                         connection can manage the orders it placed before (`bound`)"
                    );
                }
                LearnStep::Suspicious => {
                    let stats = mapper.learn_stats();
                    tracing::error!(
                        rows = stats.rows,
                        ours = stats.ours,
                        foreign_with_ref = stats.foreign_with_ref,
                        undecoded_with_ref = stats.undecoded_with_ref,
                        "ibkr: the venue returned open orders, NONE attributed to this client, and \
                         some carry an orderRef AND no decoded client id or account at all: that is \
                         what a decoder that did not attribute our own orders looks like. The \
                         connection is NOT learned and the link is NOT announced up (it is tried \
                         again a few times); orders this client placed before cannot be managed \
                         here, check them in TWS"
                    );
                }
                LearnStep::Failed => tracing::error!(
                    ?snapshot,
                    "ibkr: the venue's {} request FAILED or timed out — orders this client placed \
                     before this connection can be neither cancelled nor followed here; check them \
                     in TWS",
                    match snapshot {
                        Snapshot::OpenOrders => "open-order",
                        Snapshot::Executions => "execution",
                    }
                ),
            }
            vec![]
        }
    }
}

/// Production connect: build the socket transport and hand it to the exec loop. With the
/// `ibkr-socket` feature ON, this connects the blocking `ibapi` client to the Gateway/TWS
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

#[path = "exec_tests.rs"]
#[cfg(test)]
mod exec_tests;
