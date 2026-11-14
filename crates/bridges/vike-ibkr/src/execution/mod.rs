//! The exec loop: drains `ExecutionClientActorCommand` (submit/cancel/modify/shutdown), maps to transport calls,
//! and pumps transport inbound through the `EventMapper` onto the `EventSender` lane. Reused by the
//! socket backend (Task 9) and the `IbkrTransportFake` double. `OrderSubmitted` is emitted
//! synchronously at submit (the vike venue-adapter contract); the venue-side
//! Accepted/Filled/Rejected/Canceled events fold in from the transport inbound.
//!
//! ## Learning the venue (a fresh socket connection)
//!
//! `IbkrInbound::Connected` makes the loop LEARN the venue: it asks for the open orders, then (when the
//! venue says that snapshot ended) for the day's executions, then (when those ended) announces the
//! link as UP (`ExecLink{up: true}`). The learned state is what lets this connection cancel and follow
//! an order a previous connection of this client placed. A request that fails or does not end leaves the
//! connection NOT learned and the link NOT announced up: the host decides what that means (the gateway
//! daemon holds the session). A failed learning is asked for again (`LEARN_RETRY_BACKOFF`: three attempts in all)
//! before it is given up. The executions replay is held back until the HOST has handed the client over (`Taught`:
//! its first `adopt_restored`), so the fills it brings carry the symbols the host taught. The loop never waits for
//! any of it; a submit waits a bounded time for the open orders (`LEARN_WAIT`).
//!
//! ## A close that waited
//!
//! IB's order has no reduce-only word, so a flagged market sent after the position went opens the other way. It has no age
//! (`vike_bridge_core::exec_actor`'s `closes_at_once`: refused, the position stays open), so a close past the age limit,
//! and any close behind one for a while (`ExecutionClientActorLateCloses`), reads the position first and is cut or refused
//! ([`size_late_close`]). That read is the one place this loop waits on the venue for an answer of its own. A close it
//! cannot size yet is kept and asked about again with the loop running ([`ParkedCloses`]), and refused only when that runs out.

use std::sync::mpsc::{Receiver, TryRecvError};
use std::time::{Duration, Instant};

use ustr::Ustr;
use vike_bridge_core::exec_actor::{
    ExecutionClientActorCommand, ExecutionClientActorLateCloses, MAX_SUBMIT_WAIT,
    cancel_batch_undeclared, closes_at_once, late_close_size, too_old_to_send,
};
use vike_execution::{EventSender, TradeLinkUpdate};
use vike_model::events::{
    Event, OrderCancelRejected, OrderModified, OrderModifyRejected, OrderRejected, OrderSubmitted,
};
use vike_model::{OrderRequest, OrderType};

use crate::execution::amend::{AMEND_CONFIRM_WAIT_MS, PendingAmend};
use crate::execution::event_mapper::{
    CANCEL_COMMISSION_WAIT_MS, EventMapper, LearnStep, Learning, MAX_PENDING_RECOVERY_ATTEMPTS,
    PENDING_COMMISSION_GRACE_MS,
};
use crate::execution::order::map_order_request;
use crate::execution::registry::OrderVenueIdAllocator;
use crate::instrument::contract::parse_simplified;
use crate::transport::{
    AmendOutcome, IbkrInbound, IbkrTransport, SnapshotRequest, is_undecoded_tagged,
};

pub(crate) mod amend;
pub(crate) mod event_mapper;

// No `///` here: the module's own `//!` block is its doc (an outer doc beside it makes rustdoc resolve that block's
// links from this scope).
pub(crate) mod delayed_order;
pub mod order;
pub(crate) mod registry;

/// How often the exec loop asks the mapper whether any buffered execution has outlived its
/// commissionReport (`event_mapper`'s module doc trap 5). One `HashMap` walk over a map that is
/// empty in steady state, on a loop that already wakes every 20ms — the cadence only needs to be
/// fine enough that the effective grace window is `PENDING_COMMISSION_GRACE_MS` to within one tick.
const PENDING_SWEEP_INTERVAL: Duration = Duration::from_secs(1);

/// How long a submit waits for the venue handshake (`NextValidId` + `AccountsReady`) before being
/// refused. The readiness gate exists because `OrderVenueIdAllocator::next_order_id` floors at 101 whether or
/// not IB has ever told us its next valid id, so a submit that beats the handshake picks a number
/// the server may already have used — but the seeds are pushed onto the inbound channel by
/// `connect` BEFORE it returns, so in practice the wait is microseconds and this bound only ever
/// engages on a genuinely half-open transport.
///
/// It is a bounded WAIT rather than an instant refusal precisely because the loop drains one
/// inbound per iteration: an instant gate would reject the first order of a session on a race with
/// the loop's own startup. It is bounded rather than unbounded because
/// `crates/bridges/vike-ibkr/tests/ibkr_smokes/ibkr_smoke.rs`'s item 4 asks a human to confirm this handshake
/// cannot hang or deadlock the start — a `while !ready {}` is exactly the hang it asks about.
const READINESS_WAIT: Duration = Duration::from_secs(5);

/// The most a fresh connection's LEARNING of the venue (the open orders, then the day's executions) may
/// take before the exec loop gives it up as failed. The transport bounds each request itself and says so
/// (`IbkrInbound::SnapshotEnd { complete: false }`); this is the backstop for a transport that never says
/// anything, so a silent request cannot leave the bridge "learning" for ever.
const LEARN_DEADLINE: Duration = Duration::from_secs(30);

/// The most a SUBMIT, or a cancel/modify of an order client id the connection does not know yet, waits for the learning
/// to finish (review 18, R5). A submit placed before the connection knows the orders it left would rest on
/// an id the venue may already use (an id sequence that restarted below a resting order's id would AMEND
/// it), and a cancel right after a restart must not be refused for a window it can simply wait out. Bounded
/// and small: past it a submit is refused terminally while the venue's open orders are still unknown (`EventMapper::orders_unknown`) and a
/// cancel of an unknown order client id is answered with the advisory and retried once when the learning ends, never held for ever.
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
/// `execution/execution_tests.rs`'s `the_unlearned_refusal_is_one_plain_line` pins it).
const UNLEARNED_REASON: &str = concat!(
    "ibkr: this connection has not learned the orders this client already has at the venue, so an order id ",
    "handed out now could be a resting order's and would amend it; the order was NOT sent. If the learning is ",
    "still running, retry when the connection reports itself up; if it FAILED (the journal has an ibkr error ",
    "line saying why) it stays closed to orders: clear foreign or stale orders in TWS, then restart the ",
    "gatewayhub to rebuild the session"
);

/// The reason on the non-terminal refusal of a cancel or amend of an order client id this connection
/// does not know. It says what is true: nothing was sent, and the order's state is not ours to claim.
const UNKNOWN_ORDER_REASON: &str = "ibkr: no venue order is known for this order client id on \
     this connection (it did not rest here, it finished, or the connection has not learned the \
     venue's open orders); the request was NOT sent";

/// How often a close that could not be sized yet is asked about again ([`ParkedCloses`]). Each round holds the exec thread
/// for at most one failed read, so the loop runs between two of them; the socket backend does not ask again while a read
/// is still out, so a gateway that stopped answering costs this loop its first bound and no more.
const CLOSE_RETRY: Duration = Duration::from_secs(2);

/// **How long after an execution reaches this loop a position row may still not show it.** A read holds one number and does
/// not say what it has seen of a close that executed moments ago, and taking it at its word either way is wrong: counted as
/// not shown, a real close is refused or cut; counted as shown, a row that lags flips the position. So inside this time an
/// execution counts as NOT in the row, the direction that cannot flip, and a close whose size depends on that waits for it
/// ([`size_late_close`]). A commission plays no part: its report can come 15 s later or never, and says nothing of the row.
///
/// shortcut: chosen, not measured (no gateway run before 2026-11-09), and a row lagging longer can still flip a second close;
/// raise it, or compare with the row read before the first close, if the attended run measures a longer lag.
const ROW_SETTLE: Duration = Duration::from_secs(5);

/// **How long an order IB has not answered at all waits before the loop reads the venue's open orders for it** (once per
/// order; `EventMapper::silent_submit_due`). IB answers a `placeOrder` in milliseconds, but on paper 2026-11-10 a
/// limit-on-close and a market-on-close placed inside the regular session drew NO frame for their order id until their
/// cancel, while `reqAllOpenOrders` listed each `PendingSubmit` from 5 s on (`tests/ibkr_smokes/ibkr_loc_rth_probe.rs`).
/// Unanswered, the core's watchdog rejects such an order at about 60 s and IB still keeps it for the close; listed
/// working, the rebind accepts it (`EventMapper::on_venue_order`). Well inside the watchdog's first rung (30 s).
const SILENT_SUBMIT_ASK: Duration = Duration::from_secs(5);

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
    /// How often a close that could not be sized yet is asked about again.
    close_retry: Duration,
    /// How long such a close is kept before it is refused.
    close_retry_for: Duration,
    /// How long an execution may be missing from the position row.
    row_settle: Duration,
}

impl LoopTimes {
    const PRODUCTION: LoopTimes = LoopTimes {
        learn_deadline: LEARN_DEADLINE,
        learn_wait: LEARN_WAIT,
        teach_wait: TEACH_WAIT,
        retry: LEARN_RETRY_BACKOFF,
        close_retry: CLOSE_RETRY,
        // What an order may wait: the core's reject comes at 60 s, so a close that first read just past this age is
        // answered inside that. An older one has already met the core's reject, and sent late it folds as `closes_at_once` says.
        close_retry_for: MAX_SUBMIT_WAIT,
        row_settle: ROW_SETTLE,
    };
}

/// Run the exec loop until Shutdown (or the command channel closes). `transport` is already
/// connected. Emits venue events on `events`. Blocking, single-threaded — designed to own the
/// `ExecutionClientActor` command thread.
pub(crate) fn run_execution(
    rx: Receiver<ExecutionClientActorCommand>,
    events: EventSender,
    transport: Box<dyn IbkrTransport>,
    taught: Taught,
) {
    run_loop_with(rx, events, transport, taught, LoopTimes::PRODUCTION);
}

/// [`run_execution`] with the learning backstop and the commands' wait for it as parameters (a test shortens them),
/// no retry of a failed learning and the production wait for the host's handover.
#[cfg(test)]
fn run_loop(
    rx: Receiver<ExecutionClientActorCommand>,
    events: EventSender,
    transport: Box<dyn IbkrTransport>,
    taught: Taught,
    learn_deadline: Duration,
    learn_wait: Duration,
) {
    let times = LoopTimes { learn_deadline, learn_wait, retry: &[], ..LoopTimes::PRODUCTION };
    run_loop_with(rx, events, transport, taught, times);
}

fn run_loop_with(
    rx: Receiver<ExecutionClientActorCommand>,
    events: EventSender,
    mut transport: Box<dyn IbkrTransport>,
    taught: Taught,
    times: LoopTimes,
) {
    let LoopTimes { learn_deadline, learn_wait, .. } = times;
    let mut mapper = EventMapper::new(OrderVenueIdAllocator::default());
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
    // Cancels and amends of an order client id the connection did not know while it was still learning the venue: retried
    // ONCE when the learning ends.
    let mut deferred: Vec<Deferred> = Vec::new();
    let mut late = ExecutionClientActorLateCloses::default();
    let mut closes_out = ClosesOut::default();
    let mut parked = ParkedCloses::default();
    loop {
        // 0) What the host taught about restored orders (the actor's `adopt_restored` hook). Whether the
        //    host has handed the client over is read BEFORE the pairs are taken: a handover seen here has its
        //    pairs parked already.
        let handed_over = taught.handed_over();
        absorb_taught(&taught, &mut mapper, transport.as_mut(), &events);
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
                     orders the host knew, so the fills of those orders, finished or still open, are NOT \
                     emitted by the bridge (an order nobody taught emits no fill); reconcile finds them",
                    times.teach_wait.as_secs_f32()
                );
                taught.release();
            }
        } else {
            owed_since = None;
        }
        // The mapper's clock, and the ends IB made right after an accept whose hold for IB's words ran out.
        for event in mapper.tick(started.elapsed().as_millis() as i64) {
            let _ = events.blocking_send(event);
        }
        // An order IB said nothing about is looked for in the venue's open orders, once ([`SILENT_SUBMIT_ASK`]). Never while
        // the open orders are being learned: this request's end would close the learning.
        if mapper.learning() != Learning::OpenOrders
            && !mapper.ids().stream_dead()
            && mapper.silent_submit_due(
                started.elapsed().as_millis() as i64,
                SILENT_SUBMIT_ASK.as_millis() as i64,
            )
        {
            transport.request_open_orders();
        }
        // 1) Drain one ready inbound (short blocking poll so we also service the command channel).
        if let Some(inbound) = transport.next_recv(Duration::from_millis(20)) {
            deliver(&mut mapper, transport.as_mut(), &events, inbound);
        }
        closes_out.observe(&mut mapper, times.row_settle);
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
        retry_parked(
            &mut parked,
            &mut mapper,
            transport.as_mut(),
            &events,
            &mut closes_out,
            &times,
        );
        // 2) Poll one command (non-blocking).
        match rx.try_recv() {
            Ok(ExecutionClientActorCommand::Submit(req, queued_at)) => {
                // vike contract: OrderSubmitted synchronously at submit.
                let _ = events.blocking_send(Event::OrderSubmitted(OrderSubmitted {
                    order_client_id: req.order_client_id.clone(),
                    timestamp: req.timestamp,
                }));
                // An opening order priced off DELAYED data is refused here, first and without touching the
                // transport (E07: a market order, or a price outside the band around the delayed mark;
                // `crate::execution::delayed_order`). A venue event, so the journal carries it and a replay folds it.
                // A lifetime this bridge cannot send is refused FIRST, by name (post-only: IB has no such
                // order) - the capability row's `Unsupported`, here too so an order that skipped the core's
                // preflight is not sent as a plain GTC limit.
                let refusal = too_old_to_send(&req, queued_at)
                    .or_else(|| {
                        match vike_venues::venue_time_in_force::deny_unsupported_time_in_force(
                            "ibkr", &req,
                        ) {
                            Some(Event::OrderRejected(rejected)) => {
                                Some(rejected.reason.to_string())
                            }
                            _ => None,
                        }
                    })
                    // The doors' order-terms rules, here too for a caller with no door in front: a trigger
                    // kind with no trigger (a stop-limit would go out with no `auxPrice`), a stop-limit with no
                    // limit, a price that is no number; an attribute or an adjustment its order cannot carry.
                    .or_else(|| {
                        vike_model::order_terms_refusal(
                            &req.order_type,
                            req.price,
                            req.trigger_price,
                            req.trail,
                            req.limit_offset,
                            req.peg.as_ref(),
                            req.trail_percent,
                        )
                        .or_else(|| {
                            vike_model::attributes_refusal(
                                &req.order_type,
                                req.time_in_force,
                                req.attributes.as_ref(),
                            )
                        })
                        .or_else(|| {
                            vike_model::stop_adjust_refusal(
                                &req.order_type,
                                req.side,
                                req.trigger_price,
                                req.adjust.as_ref(),
                            )
                        })
                        .map(|why| format!("ibkr: {why}; nothing was sent"))
                    })
                    .or_else(|| {
                        crate::execution::delayed_order::refusal(
                            &req,
                            crate::execution::delayed_order::recent_mark,
                        )
                    })
                    // A price off the venue's grid at that price is refused by name, never sent (N08,
                    // `crate::instrument::tick_grid`).
                    .or_else(|| crate::instrument::tick_grid::refusal(&req))
                    .or_else(|| submit_refusal(&mut mapper, transport.as_mut(), &events));
                if let Some(reason) = refusal {
                    // The venue cannot take this order — synth the terminal HERE rather than place
                    // it and hope. Never touches the transport: on the dead-stream arm the socket is
                    // gone, and a write into it would return `Ok` from the kernel buffer while the
                    // order became unreachable in both directions (`IbkrInbound::StreamDead`).
                    tracing::error!(
                        order_client_id = %req.order_client_id,
                        symbol = %req.symbol,
                        %reason,
                        "ibkr: REFUSING submit — rejecting the intent instead of placing it"
                    );
                    let _ = events.blocking_send(Event::OrderRejected(OrderRejected {
                        cause: None,
                        order_client_id: req.order_client_id.clone(),
                        reason: reason.into(),
                        timestamp: req.timestamp,
                    }));
                } else {
                    // The order's distances go out on the venue's grid: a peg offset toward the passive side, a
                    // discretionary amount and a trail amount down (`crate::instrument::tick_grid::put_distances_on_grid`; one
                    // that rounds to nothing was refused above).
                    let mut req = req;
                    crate::instrument::tick_grid::put_distances_on_grid(&mut req);
                    settle_learning(
                        &mut mapper,
                        transport.as_mut(),
                        &events,
                        learn_wait,
                        &mut deferred,
                    );
                    // Asked again: the wait above can spend longer than the order had left.
                    if let Some(why) = too_old_to_send(&req, queued_at) {
                        let _ = events.blocking_send(Event::OrderRejected(OrderRejected {
                            cause: None,
                            order_client_id: req.order_client_id.clone(),
                            reason: why.into(),
                            timestamp: req.timestamp,
                        }));
                        continue;
                    }
                    if mapper.orders_unknown() {
                        // The wait is over and the connection still does not know which orders of this
                        // client rest at the venue (learning is running, or failed on its first request).
                        // An id handed out now could be a resting order's, and in IB's model that is an
                        // AMEND of it. Refuse the intent, terminally and with words, instead.
                        tracing::error!(
                            order_client_id = %req.order_client_id,
                            learning = ?mapper.learning(),
                            "ibkr: REFUSING submit - this connection has not learned the venue's open orders"
                        );
                        let _ = events.blocking_send(Event::OrderRejected(OrderRejected {
                            cause: None,
                            order_client_id: req.order_client_id.clone(),
                            reason: UNLEARNED_REASON.into(),
                            timestamp: req.timestamp,
                        }));
                        continue;
                    }
                    match parse_simplified(&req.symbol) {
                        Ok(contract) => {
                            // Asked last: every wait above can make a close late.
                            if late.must_read(&req, queued_at) {
                                // Behind a close that is waiting it waits too: first come, first read, and a gateway
                                // that just did not answer is not asked out of turn, once per close.
                                let sized = if parked.list.is_empty() {
                                    size_late_close(
                                        &req,
                                        &contract,
                                        &mut mapper,
                                        transport.as_mut(),
                                        &mut closes_out,
                                        times.row_settle,
                                        false,
                                    )
                                } else {
                                    CloseSize::Wait(
                                        "the closes ahead of it are not sized yet".to_string(),
                                    )
                                };
                                match sized {
                                    CloseSize::Go(cut) => write_order(
                                        &req,
                                        &contract,
                                        cut,
                                        &mut mapper,
                                        transport.as_mut(),
                                        &events,
                                        &mut closes_out,
                                    ),
                                    CloseSize::Refuse(why) => refuse_close(&events, &req, why),
                                    CloseSize::Wait(why) | CloseSize::NoRead(why) => {
                                        parked.park(req, contract, why, &times)
                                    }
                                }
                            } else {
                                write_order(
                                    &req,
                                    &contract,
                                    None,
                                    &mut mapper,
                                    transport.as_mut(),
                                    &events,
                                    &mut closes_out,
                                );
                            }
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
                                order_client_id = %req.order_client_id,
                                symbol = %req.symbol,
                                reason = %why,
                                "ibkr: REFUSING submit — the symbol does not name one contract"
                            );
                            let _ = events.blocking_send(Event::OrderRejected(OrderRejected {
                                cause: None,
                                order_client_id: req.order_client_id.clone(),
                                reason: why.into(),
                                timestamp: req.timestamp,
                            }));
                        }
                    }
                }
            }
            Ok(ExecutionClientActorCommand::Cancel { order_client_id, .. }) => {
                // Not sent yet: the cancel ends it here, or it would go out behind the caller's back.
                if let Some(close) = parked.take(&order_client_id) {
                    let why = never_sized(&close.req, "a cancel ended it while it waited");
                    refuse_close(&events, &close.req, why);
                    continue;
                }
                if mapper.ids().order_id_of(&order_client_id).is_none() {
                    // Not known (yet): a teaching that came after this turn's head, then the learning if it runs.
                    absorb_taught(&taught, &mut mapper, transport.as_mut(), &events);
                    settle_learning(
                        &mut mapper,
                        transport.as_mut(),
                        &events,
                        learn_wait,
                        &mut deferred,
                    );
                }
                if mapper.ids().stream_dead() {
                    // Deliberately NOT synthesized as `OrderCanceled`: the stream died, so the order
                    // may well still be RESTING at IB. Claiming a cancel we never delivered would
                    // tell the platform it is flat while the venue holds live exposure — the exact
                    // inversion of the vanishing-order bug, and the more dangerous direction.
                    tracing::error!(
                        order_client_id = %order_client_id,
                        "ibkr: CANNOT CANCEL — the order-update stream is dead, so this cancel \
                         reached no venue. The order's true state is UNKNOWN; cancel it in TWS."
                    );
                } else if let Some(order_id) = mapper.ids_mut().order_id_of(&order_client_id) {
                    send_cancel(
                        &mut mapper,
                        transport.as_mut(),
                        &events,
                        &order_client_id,
                        order_id,
                    );
                } else {
                    // Unknown order client id: NOTHING is sent now, and the caller is told so. It used to be a silent
                    // no-op (the ExecutionClientActor surfaces only a dead-LANE reject), which left the caller
                    // believing an order was being cancelled while it may rest at IB. The advisory is
                    // NON-terminal: the order's state is not ours to claim.
                    if mapper.recently_finished(&order_client_id) {
                        // A race the cancel lost: the order reached its terminal on this connection moments
                        // ago. Not the alarm an unknown order is.
                        tracing::info!(
                            order_client_id = %order_client_id,
                            "ibkr: cancel of an order that has just finished - nothing to cancel"
                        );
                    } else {
                        tracing::error!(
                            order_client_id = %order_client_id,
                            learning = ?mapper.learning(),
                            "ibkr: CANNOT CANCEL — no venue order is known for this order client id; \
                             nothing was sent"
                        );
                    }
                    let _ = events.blocking_send(Event::OrderCancelRejected(OrderCancelRejected {
                        order_client_id: order_client_id.clone(),
                        reason: UNKNOWN_ORDER_REASON.into(),
                        timestamp: 0,
                    }));
                    // The connection is still learning (the wait ran out): ONE retry when it ends, which is
                    // when an order the venue still holds becomes known.
                    if still_learning(&mapper) {
                        deferred.push(Deferred::Cancel(order_client_id));
                    }
                }
            }
            // No bulk-cancel path here, and this venue declares none — so `ExecutionClientActor` fanned the
            // batch out into the per-id `Cancel`s above before it ever reached this channel, and
            // this arm is unreachable. It exists because a new command variant is exhaustive; the
            // shared helper refuses every id NON-terminally rather than letting a future mis-wiring
            // drop them silently.
            Ok(ExecutionClientActorCommand::CancelBatch { order_client_ids, .. }) => {
                cancel_batch_undeclared(&events, &order_client_ids)
            }
            // Unreachable the same way: this venue declares no bulk submit and no queued adopt.
            Ok(ExecutionClientActorCommand::SubmitBatch(requests, _)) => {
                vike_bridge_core::exec_actor::submit_batch_undeclared(&events, &requests)
            }
            Ok(ExecutionClientActorCommand::Adopt(order)) => {
                vike_bridge_core::exec_actor::adopt_undeclared(&order)
            }
            Ok(ExecutionClientActorCommand::Modify {
                order,
                new_qty,
                new_price,
                new_trigger_price,
            }) => {
                // Resolve the resting order's numeric id from its order client id; unknown ⇒ nothing to amend. Every
                // outcome of a known order ends in an event (`amend`). The trigger reaches here because
                // `lib.rs` declares `with_trigger_amend`.
                if mapper.ids().order_id_of(&order.order_client_id).is_none() {
                    absorb_taught(&taught, &mut mapper, transport.as_mut(), &events);
                    settle_learning(
                        &mut mapper,
                        transport.as_mut(),
                        &events,
                        learn_wait,
                        &mut deferred,
                    );
                }
                let asked = (new_qty, new_price, new_trigger_price);
                match amend(&mut mapper, transport.as_mut(), &events, &order, asked) {
                    Amended::Unknown => {
                        tracing::error!(
                            order_client_id = %order.order_client_id,
                            learning = ?mapper.learning(),
                            "ibkr: CANNOT MODIFY — no venue order is known for this order client id; \
                             nothing was sent"
                        );
                        let _ =
                            events.blocking_send(Event::OrderModifyRejected(OrderModifyRejected {
                                order_client_id: order.order_client_id.clone(),
                                reason: UNKNOWN_ORDER_REASON.into(),
                                timestamp: vike_bridge_core::rest::amend_answer_time(None),
                            }));
                        if still_learning(&mapper) {
                            deferred.push(Deferred::Modify { order, asked });
                        }
                    }
                    Amended::Handled => {}
                }
            }
            Ok(ExecutionClientActorCommand::Shutdown) | Err(TryRecvError::Disconnected) => {
                if let Some(key) = history_key(&events) {
                    vike_bridge_core::link_history::closed_by_us(key);
                }
                break;
            }
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
    // An end still held for IB's words goes out as it is: it must not die with the loop.
    for event in mapper.tick(i64::MAX) {
        let _ = events.blocking_send(event);
    }
    parked.refuse_all(&events, "this connection is shutting down");
}

/// What the HOST taught this client about orders it restored (`ExecutionClient::adopt_restored`: the pairs of
/// order client id and the unified symbol the engine registered the order under). The call comes from another
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
    /// The client's [`KEPT_TAUGHT`] key; `None` keeps nothing.
    identity: Option<String>,
}

/// The most pairs [`KEPT_TAUGHT`] holds for one client identity, past which a new one is said and not kept.
const MAX_KEPT_TAUGHT: usize = 4096;

/// **What the host taught a client AFTER its handover, kept for the next client of that identity** (one exec
/// client id on one gateway). The gatewayhub rebuilds this adapter after IB's daily logoff, with no epoch change,
/// so the tradehub says nothing, and hands the new client only what ITS ledger holds
/// (`crates/vike-gatewayhub/src/trade/session.rs`'s `start_adapter_locked`). An order the engine adopted from a
/// PREVIOUS daemon is in no ledger: without this it comes back untaught ([`EventMapper`]'s `is_untaught`), and
/// from the first logoff on its fills reach no engine while the engine holds it live. The handover's own pairs are
/// the ledger's, which teaches them again, and are not kept.
///
/// shortcut: an entry leaves only with the process, because a client cannot tell a finished order from one it is
/// about to learn; a stale one only names a symbol for a replayed fill the engine dedups. Bounded per identity.
static KEPT_TAUGHT: std::sync::LazyLock<
    std::sync::Mutex<std::collections::HashMap<String, std::collections::HashMap<String, String>>>,
> = std::sync::LazyLock::new(Default::default);

impl Default for Taught {
    fn default() -> Self {
        Taught::new(true, None)
    }
}

impl Taught {
    fn new(handed_over: bool, identity: Option<String>) -> Self {
        Taught(std::sync::Arc::new(TaughtShared {
            orders: std::sync::Mutex::new(Vec::new()),
            handed_over: std::sync::atomic::AtomicBool::new(handed_over),
            awaiting: std::sync::atomic::AtomicBool::new(false),
            identity,
        }))
    }

    /// A handover still to come: the executions replay of the learning waits for the host's first teaching.
    #[cfg(any(test, feature = "test-support"))]
    pub(crate) fn gated() -> Self {
        Taught::new(false, None)
    }

    /// [`Self::gated`] for a client of `identity`: it starts knowing what [`KEPT_TAUGHT`] holds for it, and adds
    /// to it what it is taught after its handover.
    pub(crate) fn gated_as(identity: String) -> Self {
        let kept = KEPT_TAUGHT.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        let pairs = kept.get(&identity).map(|pairs| {
            pairs
                .iter()
                .map(|(order_client_id, symbol)| (order_client_id.clone(), symbol.clone()))
                .collect()
        });
        drop(kept);
        let taught = Taught::new(false, Some(identity));
        *taught.0.orders.lock().unwrap_or_else(std::sync::PoisonError::into_inner) =
            pairs.unwrap_or_default();
        taught
    }

    /// Keep what the host taught after the handover ([`KEPT_TAUGHT`]).
    fn keep(&self, orders: &[(String, String)]) {
        let Some(identity) = self.0.identity.as_ref().filter(|_| self.handed_over()) else {
            return;
        };
        let mut kept = KEPT_TAUGHT.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        let table = kept.entry(identity.clone()).or_default();
        for (order_client_id, symbol) in orders {
            if table.len() >= MAX_KEPT_TAUGHT && !table.contains_key(order_client_id) {
                tracing::warn!(
                    order_client_id,
                    cap = MAX_KEPT_TAUGHT,
                    "ibkr: the table of restored orders is full: this order's fills stop at the next rebuild \
                     of the bridge, and the reconcile reports them"
                );
                continue;
            }
            table.insert(order_client_id.clone(), symbol.clone());
        }
    }

    /// Park what the host taught and mark the client handed over. Returns at once (the host's contract: no
    /// network, no waiting).
    pub(crate) fn teach(&self, orders: &[(String, String)]) {
        self.keep(orders);
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

/// One close this loop has written.
struct CloseOut {
    order_id: i32,
    symbol: String,
    side: i32,
    /// What had executed of it when this loop last looked, and when that last moved.
    executed: f64,
    executed_at: Option<Instant>,
}

/// **The closes this loop has written whose effect on the position read is not settled**: not ended on this connection, or
/// ended with an execution younger than [`ROW_SETTLE`]. A write to IB returns before the venue has seen the order, so the
/// position read just behind it still shows what it will close, and two closes side by side would each read the whole
/// position and flip it. Only a close counts: an order the core marked [`closes_at_once`].
#[derive(Default)]
struct ClosesOut(Vec<CloseOut>);

impl ClosesOut {
    /// Keep the order just written when it is a close.
    fn note(&mut self, order_id: i32, req: &OrderRequest, mapper: &mut EventMapper) {
        if closes_at_once(req) {
            mapper.watch_executions(order_id);
            self.0.push(CloseOut {
                order_id,
                symbol: req.symbol.clone(),
                side: req.side,
                executed: 0.0,
                executed_at: None,
            });
        }
    }

    /// Stamp what has executed since the last look (the mapper has no clock), and forget what is settled. Every pass.
    fn observe(&mut self, mapper: &mut EventMapper, row_settle: Duration) {
        if self.0.is_empty() {
            return;
        }
        let now = Instant::now();
        self.0.retain_mut(|close| {
            let executed = mapper.executed(close.order_id);
            if executed > close.executed {
                close.executed = executed;
                close.executed_at = Some(now);
            }
            let settled = close.executed_at.is_none_or(|at| at + row_settle <= now);
            let keep = !settled || mapper.unexecuted(close.order_id).is_some();
            if !keep {
                mapper.unwatch_executions(close.order_id);
            }
            keep
        });
    }

    /// What may still come off a position read that was asked at `asked`, of the closes on `req`'s symbol and side: what has
    /// not executed, and what executed too recently to be sure the row shows it ([`ROW_SETTLE`]).
    fn still_out(
        &self,
        req: &OrderRequest,
        mapper: &EventMapper,
        asked: Instant,
        row_settle: Duration,
    ) -> f64 {
        self.0
            .iter()
            .filter(|close| close.symbol == req.symbol && close.side == req.side)
            .map(|close| {
                let in_row = close.executed_at.is_none_or(|at| at + row_settle <= asked);
                mapper.unexecuted(close.order_id).unwrap_or(0.0)
                    + if in_row { 0.0 } else { close.executed }
            })
            .sum()
    }
}

/// What [`size_late_close`] says of a close.
enum CloseSize {
    /// Send it: as it is, or cut to this.
    Go(Option<f64>),
    /// A refusal in these words, with nothing sent.
    Refuse(String),
    /// Its size depends on how a close still out ends, or on what the row does not show yet: ask again.
    Wait(String),
    /// The position could not be read, in these words: ask again, and read nothing more this round.
    NoRead(String),
}

/// **What the close `req`, which must read the position ([`ExecutionClientActorLateCloses`]), may carry.** What is still out ([`ClosesOut`]) is taken
/// off what the venue says it holds, the figure that cannot flip the position while a row lags less than [`ROW_SETTLE`]. When
/// that gives a smaller answer than the row alone the close waits ([`CloseSize::Wait`]) to see how those closes end, unless this
/// is its `last_try`: then the smaller answer stands. So a pending commission neither refuses nor cuts it, and it goes at once
/// when it fits either way.
///
/// ⚠ **A failed read never sends** ([`CloseSize::NoRead`]), where `late_close_size` lets a close go on the other venues. Their POST
/// answers: a venue that cannot answer a read mostly fails the POST as well, in words. IB's write is a bare write, which
/// returns `Ok` from the kernel buffer on the very socket that just failed to answer, so the order would go out when the
/// gateway wakes up, the closes behind it too, and nothing on the wire stops them opening the other way. It is asked again
/// ([`ParkedCloses`]), and past that refused, loudly, with the position still open. One loop serves both backends, so cpapi,
/// whose POST does answer, refuses too.
fn size_late_close(
    req: &OrderRequest,
    contract: &crate::instrument::contract::IbkrContract,
    mapper: &mut EventMapper,
    transport: &mut dyn IbkrTransport,
    closes_out: &mut ClosesOut,
    row_settle: Duration,
    last_try: bool,
) -> CloseSize {
    closes_out.observe(mapper, row_settle);
    // Before the read: the row is at best as young as the question.
    let asked = Instant::now();
    let held = match transport.held_position(contract) {
        Ok(held) if held.is_finite() => held,
        Ok(held) => return CloseSize::NoRead(format!("the position read as {held}")),
        Err(why) => return CloseSize::NoRead(why),
    };
    let out = closes_out.still_out(req, mapper, asked, row_settle);
    let sized = late_close_size(req, Ok(held + f64::from(req.side) * out)).map_err(|why| {
        if out > 0.0 { format!("{why} (held {held}, {out} already on its way)") } else { why }
    });
    if !last_try && out > 0.0 && sized != Ok(None) && late_close_size(req, Ok(held)).is_ok() {
        return CloseSize::Wait(format!("held {held}, and {out} on its way decides its size"));
    }
    match sized {
        Ok(cut) => CloseSize::Go(cut),
        Err(why) => CloseSize::Refuse(why),
    }
}

/// Give `req` an order id and write it, at `cut` when [`size_late_close`] cut it.
fn write_order(
    req: &OrderRequest,
    contract: &crate::instrument::contract::IbkrContract,
    cut: Option<f64>,
    mapper: &mut EventMapper,
    transport: &mut dyn IbkrTransport,
    events: &EventSender,
    closes_out: &mut ClosesOut,
) {
    // price_magnifier 1: bonds resolve theirs via contractDetails (Task
    // 9+). No magnifier scaling on the equity/forex/derivative paths.
    let spec = match cut {
        Some(qty) => map_order_request(&OrderRequest { qty, ..req.clone() }, contract, 1),
        None => map_order_request(req, contract, 1),
    };
    // A kind this bridge builds no IB order for is refused before an id is
    // spent or bound: it used to go out as MARKET.
    let spec = match spec {
        Ok(spec) => spec,
        Err(why) => {
            tracing::error!(
                order_client_id = %req.order_client_id,
                reason = %why,
                "ibkr: REFUSING submit — an order kind this bridge does not build"
            );
            let _ = events.blocking_send(Event::OrderRejected(OrderRejected {
                cause: None,
                order_client_id: req.order_client_id.clone(),
                reason: why.into(),
                timestamp: req.timestamp,
            }));
            return;
        }
    };
    let Some(order_id) = mapper.ids_mut().next_order_id() else {
        // The id space is spent: placing under a reused id would AMEND a live
        // order. Refuse the intent instead, terminally.
        tracing::error!(
            order_client_id = %req.order_client_id,
            "ibkr: REFUSING submit — this connection has no order id left"
        );
        let _ = events.blocking_send(Event::OrderRejected(OrderRejected {
            cause: None,
            order_client_id: req.order_client_id.clone(),
            reason: "ibkr: this connection has used every order id it can allocate; restart it"
                .into(),
            timestamp: req.timestamp,
        }));
        return;
    };
    mapper.ids_mut().bind(order_id, &req.order_client_id);
    // Track as unacked: a refusal before the accept is this order's reject (an
    // accepted one ends as a cancel), and an id-less one can still be attributed here.
    // A cut close completes at its cut size, so its last fill is final.
    mapper.on_submit(order_id, cut.unwrap_or(req.qty));
    // The fills of this order carry THIS symbol, not the instrument IB echoes on
    // the execution (see `EventMapper::fill_symbol`).
    mapper.register_symbol(&req.order_client_id, &req.symbol);
    closes_out.note(order_id, req, mapper);
    transport.place_order(order_id, &spec, contract);
}

/// Refuse the close `req`, terminally, with nothing sent.
fn refuse_close(events: &EventSender, req: &OrderRequest, why: String) {
    tracing::error!(
        order_client_id = %req.order_client_id,
        symbol = %req.symbol,
        reason = %why,
        "ibkr: REFUSING a close that waited"
    );
    let _ = events.blocking_send(Event::OrderRejected(OrderRejected {
        cause: None,
        order_client_id: req.order_client_id.clone(),
        reason: why.into(),
        timestamp: req.timestamp,
    }));
}

/// The words of a close that ends without ever being sized.
fn never_sized(req: &OrderRequest, why: &str) -> String {
    format!(
        "ibkr: this close was NOT sent and what it was closing is STILL OPEN: it waited behind earlier calls to the \
         gateway and could not be sized against the position ({why}), and IB's order has no reduce-only word, so sent \
         blind it could open a position the other way. Check {} in TWS and close it there, or send the close again",
        req.symbol
    )
}

/// A close [`size_late_close`] could not size yet.
struct ParkedClose {
    req: Box<OrderRequest>,
    contract: crate::instrument::contract::IbkrContract,
    /// The last thing that stopped it.
    why: String,
    give_up_at: Instant,
}

/// **The closes waiting to be sized**, oldest first, asked about again every [`LoopTimes::close_retry`] by [`retry_parked`] for
/// [`LoopTimes::close_retry_for`] each. Kept here rather than waited for, so the loop keeps folding what the venue sends and
/// serving the commands behind them. Each ends in an event: written, refused, or refused at the shutdown.
#[derive(Default)]
struct ParkedCloses {
    list: Vec<ParkedClose>,
    next_try: Option<Instant>,
}

impl ParkedCloses {
    fn park(
        &mut self,
        req: Box<OrderRequest>,
        contract: crate::instrument::contract::IbkrContract,
        why: String,
        times: &LoopTimes,
    ) {
        tracing::warn!(
            order_client_id = %req.order_client_id,
            symbol = %req.symbol,
            reason = %why,
            waits_s = times.close_retry_for.as_secs_f32(),
            "ibkr: a close that waited cannot be sized yet: it is NOT sent, and is asked about again"
        );
        let now = Instant::now();
        self.next_try.get_or_insert(now + times.close_retry);
        self.list.push(ParkedClose { req, contract, why, give_up_at: now + times.close_retry_for });
    }

    fn take(&mut self, order_client_id: &str) -> Option<ParkedClose> {
        let at = self.list.iter().position(|close| close.req.order_client_id == order_client_id)?;
        Some(self.list.remove(at))
    }

    fn refuse_all(&mut self, events: &EventSender, why: &str) {
        self.next_try = None;
        for close in std::mem::take(&mut self.list) {
            let why = never_sized(&close.req, &format!("{why}; before that: {}", close.why));
            refuse_close(events, &close.req, why);
        }
    }
}

/// One round of [`ParkedCloses`], when one is due; a dead stream ends them at once, as nothing can answer any more. A gateway
/// that says it has no link, and a connection learning the venue again, could not take the order the answer sizes (a fresh
/// submit is refused for both), so they read nothing and the closes wait on.
fn retry_parked(
    parked: &mut ParkedCloses,
    mapper: &mut EventMapper,
    transport: &mut dyn IbkrTransport,
    events: &EventSender,
    closes_out: &mut ClosesOut,
    times: &LoopTimes,
) {
    let Some(due) = parked.next_try else { return };
    if let Some(reason) = mapper.ids().stream_death_reason() {
        let why = format!("the order-update stream is dead: {reason}");
        parked.refuse_all(events, &why);
        return;
    }
    if Instant::now() < due {
        return;
    }
    let mut no_read = if transport.server_link_broken() {
        Some("the gateway says it has no link to IBKR's servers".to_string())
    } else if mapper.orders_unknown() {
        Some("this connection is learning the venue's open orders again".to_string())
    } else {
        None
    };
    let mut waiting = Vec::new();
    for mut close in std::mem::take(&mut parked.list) {
        let last_try = Instant::now() >= close.give_up_at;
        let sized = match &no_read {
            Some(why) => CloseSize::Wait(why.clone()),
            None => size_late_close(
                &close.req,
                &close.contract,
                mapper,
                transport,
                closes_out,
                times.row_settle,
                last_try,
            ),
        };
        close.why = match sized {
            CloseSize::Go(cut) => {
                write_order(
                    &close.req,
                    &close.contract,
                    cut,
                    mapper,
                    transport,
                    events,
                    closes_out,
                );
                continue;
            }
            CloseSize::Refuse(why) => {
                refuse_close(events, &close.req, why);
                continue;
            }
            CloseSize::NoRead(why) => {
                no_read = Some(why.clone());
                why
            }
            CloseSize::Wait(why) => why,
        };
        if last_try {
            refuse_close(events, &close.req, never_sized(&close.req, &close.why));
        } else {
            waiting.push(close);
        }
    }
    parked.next_try = (!waiting.is_empty()).then(|| Instant::now() + times.close_retry);
    parked.list = waiting;
}

/// A cancel or amend of an order client id the connection did not know while it was still learning the venue, kept for
/// ONE retry when the learning ends (the caller has already been told the request was not sent).
enum Deferred {
    Cancel(String),
    /// `asked` is `(new_qty, new_price, new_trigger_price)`, as `ExecutionClientActorCommand::Modify` carries them.
    Modify {
        order: Box<OrderRequest>,
        asked: (Option<f64>, Option<f64>, Option<f64>),
    },
}

/// Whether the connection is in the middle of learning the venue.
fn still_learning(mapper: &EventMapper) -> bool {
    matches!(mapper.learning(), Learning::OpenOrders | Learning::Executions)
}

/// What [`amend`] did.
enum Amended {
    /// No venue order is known for the order client id: nothing was sent, and the caller owes the advisory.
    Unknown,
    /// Sent (its answer is tracked by `PendingAmends`) or refused with an event: nothing more is owed.
    Handled,
}

/// Amend the resting order of `order` in place, and say what became of the request: every outcome but
/// "sent, now waiting for TWS's echo" ends in an event here, and the one that waits is closed by
/// [`crate::execution::amend::PendingAmends`] (confirmed, refused or unconfirmed). Nothing is accepted in silence.
///
/// `asked` is `(new_qty, new_price, new_trigger_price)`. A TRIGGER amend (`ExecutionClient::modify_trigger`, a
/// stop-limit's or a take-profit's trigger) re-places the order with the new `auxPrice` beside the price it moves (or
/// keeps), and its echo is matched on the trigger too; a stop's trigger is its price and rides `new_price`, so a
/// trigger on any other kind is refused with nothing sent.
fn amend(
    mapper: &mut EventMapper,
    transport: &mut dyn IbkrTransport,
    events: &EventSender,
    order: &OrderRequest,
    asked: (Option<f64>, Option<f64>, Option<f64>),
) -> Amended {
    let (new_qty, new_price, new_trigger) = asked;
    let order_client_id = &order.order_client_id;
    let refuse = |reason: String| {
        let _ = events.blocking_send(Event::OrderModifyRejected(OrderModifyRejected {
            order_client_id: order_client_id.clone(),
            reason: reason.into(),
            timestamp: vike_bridge_core::rest::amend_answer_time(None),
        }));
    };
    if mapper.ids().stream_dead() {
        tracing::error!(
            order_client_id = %order_client_id,
            "ibkr: CANNOT MODIFY — the order-update stream is dead; amend it in TWS."
        );
        // Non-terminal and true: the request was NOT sent. (A cancel stays log-only in this state because
        // it cannot say whether the order still rests; an advisory that nothing was sent can.)
        refuse(
            "ibkr: the order-update stream is dead, so this amend was NOT sent; the order's venue state \
             is unknown, check it in TWS"
                .to_string(),
        );
        return Amended::Handled;
    }
    let Some(order_id) = mapper.ids_mut().order_id_of(order_client_id) else {
        return Amended::Unknown;
    };
    match parse_simplified(&order.symbol) {
        Ok(contract) => {
            // A stop's price is its trigger (`aux_price` at TWS); every other kind, a stop-limit included, moves its
            // limit price (`vike_model::modified_price_is_trigger`, the field the order's FSM moves too).
            let is_stop = vike_model::modified_price_is_trigger(&order.order_type);
            if matches!(order.order_type, OrderType::Market) {
                refuse("ibkr: a market order has no resting terms to amend".to_string());
                return Amended::Handled;
            }
            // The re-place would restate the adjustment as it was asked, and IB may already have made it.
            if order.adjust.is_some() {
                refuse(
                    "ibkr: an adjustable stop cannot be amended: an amend re-sends the order whole with its \
                     adjustment, which IB may already have made; cancel it and place a new one. Nothing was sent"
                        .to_string(),
                );
                return Amended::Handled;
            }
            // An amend RE-PLACES the whole order, so every term must be known and expressible. Only a limit, a
            // stop, a stop-limit and a take-profit are (`map_order_request` builds a market-to-limit too, but its
            // limit is the venue's, set by its first fill), and an order the engine rebuilt from a venue status
            // report (`OrderRecord::from_status_report`: restored or adopted) holds only the terms the venue's row
            // stated (`crate::reconcile`'s order-report parsers; a Client Portal row states a limit's price alone), so
            // a term the row did not state would go out missing (a stop-limit's or a take-profit's trigger is
            // re-sent as it was). A take-profit with no limit (IB's `MIT`) has its trigger alone. Refused, in words,
            // with nothing sent.
            // Its real time in force is the engine's to guard before it gets here
            // (`vike_execution::Engine`'s `modify_order`).
            let before_price = if is_stop { order.trigger_price } else { order.price };
            let known = |p: Option<f64>| p.is_some_and(f64::is_finite);
            let market_if_touched =
                order.order_type == OrderType::TakeProfit && order.price.is_none();
            let terms_known = match order.order_type {
                OrderType::Limit | OrderType::Stop => known(before_price),
                OrderType::StopLimit => known(before_price) && known(order.trigger_price),
                OrderType::TakeProfit => {
                    (market_if_touched || known(before_price)) && known(order.trigger_price)
                }
                _ => false,
            };
            if !terms_known {
                tracing::error!(
                    order_client_id = %order_client_id,
                    order_type = ?order.order_type,
                    "ibkr: CANNOT MODIFY — this order's terms are not fully known to the platform"
                );
                // The words of every kind but a stop-limit and a take-profit are the ones operators have read since
                // before the stop-limit was amendable, byte for byte (`tests/ibkr_amend.rs` pins them).
                refuse(if order.order_type == OrderType::StopLimit {
                    "ibkr: this stop-limit order cannot be amended in place: an amend re-sends it whole, and the \
                     platform does not hold both its limit and its trigger price (restored from the venue \
                     without them); amend it in TWS. Nothing was sent"
                        .to_string()
                } else if order.order_type == OrderType::MarketToLimit {
                    "ibkr: a market-to-limit order cannot be amended here: the limit its remainder rests at is \
                     the price of its first fill, which the venue set and the platform does not hold; amend it in \
                     TWS. Nothing was sent"
                        .to_string()
                } else if order.order_type == OrderType::TakeProfit {
                    "ibkr: this take-profit (if-touched) order cannot be amended in place: an amend re-sends it \
                     whole, and the platform does not hold its trigger price, or its limit (restored from the \
                     venue without them); amend it in TWS. Nothing was sent"
                        .to_string()
                } else {
                    format!(
                        "ibkr: this order cannot be amended in place: only a limit or stop order whose price and \
                         time in force the platform holds can be (this one is {:?}{}); amend it in TWS. \
                         Nothing was sent",
                        order.order_type,
                        if before_price.is_none() {
                            ", restored from the venue without its price"
                        } else {
                            ""
                        }
                    )
                });
                return Amended::Handled;
            }
            if new_trigger.is_some()
                && !matches!(order.order_type, OrderType::StopLimit | OrderType::TakeProfit)
            {
                refuse(format!(
                    "ibkr: a {} order has no trigger to amend apart from its price; nothing was sent",
                    order.order_type
                ));
                return Amended::Handled;
            }
            // A market-if-touched has no limit for `new_price` to move (`vike_model::modified_price_is_trigger` says a
            // take-profit's price is its limit): setting one would send it as a limit-if-touched. Its trigger moves
            // with a trigger amend.
            if market_if_touched && new_price.is_some() {
                refuse(
                    "ibkr: this market-if-touched order (a take-profit with no limit) has no limit price to amend; \
                     its size and its trigger can be amended. Nothing was sent"
                        .to_string(),
                );
                return Amended::Handled;
            }
            // Rebuild the spec from the (possibly resized/repriced) order: `new_price` lands where `is_stop` says,
            // an explicit trigger on the trigger (`vike_model::modified_order`).
            let mut amended = vike_model::modified_order(order, new_qty, new_price, new_trigger);
            // A release IB holds whose time has passed is live already: the re-place goes WITHOUT its `goodAfterTime`.
            if amended.release.is_some_and(|r| r.at <= vike_model::now_ms()) {
                amended.release = None;
            }
            let requested_price = if is_stop { amended.trigger_price } else { amended.price };
            let spec = match map_order_request(&amended, &contract, 1) {
                Ok(spec) => spec,
                Err(why) => {
                    refuse(why);
                    return Amended::Handled;
                }
            };
            // Kept BEFORE the write: TWS's echo can beat the line after it.
            let mut pending = PendingAmend::new(
                order_id,
                (order.qty, before_price),
                (amended.qty, requested_price),
                is_stop,
            );
            if let Some(t) = new_trigger {
                pending = pending.with_trigger(order.trigger_price, t);
            }
            // An earlier amend of this order still waiting for TWS: this one is refused, NOT written.
            if let Some(event) = mapper.note_amend(order_client_id, pending) {
                let _ = events.blocking_send(event);
                return Amended::Handled;
            }
            match transport.modify_order(order_id, &spec, &contract) {
                // The venue answers by re-sending the order; `PendingAmends` closes the request.
                AmendOutcome::Sent => {}
                AmendOutcome::Acknowledged => {
                    mapper.take_amend(order_client_id);
                    mapper.amend_acknowledged(
                        order_client_id,
                        order_id,
                        amended.qty,
                        requested_price,
                    );
                    let _ = events.blocking_send(Event::OrderModified(OrderModified {
                        order_client_id: order_client_id.clone(),
                        venue_order_id: Some(order_id.to_string().into()),
                        new_qty: Some(amended.qty),
                        new_price: requested_price,
                        new_trigger_price: new_trigger,
                        timestamp: vike_bridge_core::rest::amend_answer_time(None),
                    }));
                }
                AmendOutcome::Refused(reason) => {
                    mapper.take_amend(order_client_id);
                    tracing::error!(order_client_id = %order_client_id, %reason, "ibkr: CANNOT MODIFY — the amend was refused");
                    refuse(reason);
                }
            }
        }
        // An amend whose symbol does not name one contract keeps the RESTING order exactly as it is, and
        // says so: the caller believes the order was repriced. Non-terminal: the order is still live at IB.
        Err(why) => {
            tracing::error!(
                order_client_id = %order_client_id,
                symbol = %order.symbol,
                reason = %why,
                "ibkr: CANNOT MODIFY — the symbol does not name one contract; the \
                 resting order is UNCHANGED"
            );
            refuse(format!(
                "ibkr: the symbol does not name one contract ({why}); the resting order is UNCHANGED"
            ));
        }
    }
    Amended::Handled
}

/// What the host taught ([`Taught`]) reaches the mapper: each order's symbol, and a restored order this connection does
/// not hold is bound when the venue still lists it ([`IbkrTransport::locate_restored`]: the cpapi backend; the socket
/// learns its orders at the connect). The loop's head calls it, and so does a cancel or amend of an order client id not known yet,
/// which may have been sent right behind a teaching this turn's head did not see.
fn absorb_taught(
    taught: &Taught,
    mapper: &mut EventMapper,
    transport: &mut dyn IbkrTransport,
    events: &EventSender,
) {
    let mut unbound = Vec::new();
    for (order_client_id, symbol) in taught.take() {
        mapper.register_symbol(&order_client_id, &symbol);
        if mapper.ids().order_id_of(&order_client_id).is_none() {
            unbound.push(order_client_id);
        }
    }
    if unbound.is_empty() {
        return;
    }
    for (order_id, order_client_id) in transport.locate_restored(&unbound) {
        for event in mapper.rebind_open_order(order_id, &order_client_id) {
            let _ = events.blocking_send(event);
        }
    }
}

/// Send the cancel of `order_client_id` (`order_id` on this connection), or say that it did not take: a backend that cannot
/// address the order, or a venue that refused the cancel, answers in words ([`IbkrTransport::cancel_order`]), a
/// non-terminal `OrderCancelRejected` (the order still rests at the venue). The end that answers it is not held for
/// IB's words (`EventMapper::note_cancel_sent`).
fn send_cancel(
    mapper: &mut EventMapper,
    transport: &mut dyn IbkrTransport,
    events: &EventSender,
    order_client_id: &str,
    order_id: i32,
) {
    mapper.note_cancel_sent(order_id);
    if let Err(reason) = transport.cancel_order(order_id) {
        tracing::error!(order_client_id, order_id, %reason, "ibkr: CANNOT CANCEL — the cancel did not take");
        let _ = events.blocking_send(Event::OrderCancelRejected(OrderCancelRejected {
            order_client_id: order_client_id.to_string(),
            reason: reason.into(),
            timestamp: 0,
        }));
    }
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
            Deferred::Cancel(order_client_id) => {
                if !learned {
                    tracing::error!(
                        order_client_id = %order_client_id,
                        "ibkr: the cancel of this order was not retried: the connection never learned the \
                         venue's open orders; cancel it in TWS"
                    );
                } else if mapper.ids().stream_dead() {
                    tracing::error!(order_client_id = %order_client_id, "ibkr: the deferred cancel was not sent: the stream is dead");
                } else if let Some(order_id) = mapper.ids().order_id_of(&order_client_id) {
                    tracing::info!(order_client_id = %order_client_id, order_id, "ibkr: retrying a cancel deferred during the learning");
                    send_cancel(mapper, transport, events, &order_client_id, order_id);
                } else {
                    tracing::info!(
                        order_client_id = %order_client_id,
                        "ibkr: a cancel deferred during the learning: the venue does not hold the order \
                         (it finished); nothing to cancel"
                    );
                }
            }
            Deferred::Modify { order, asked } => {
                if !learned {
                    tracing::error!(
                        order_client_id = %order.order_client_id,
                        "ibkr: the amend of this order was not retried: the connection never learned the \
                         venue's open orders; amend it in TWS"
                    );
                } else if let Amended::Unknown = amend(mapper, transport, events, &order, asked) {
                    tracing::info!(
                        order_client_id = %order.order_client_id,
                        "ibkr: an amend deferred during the learning: the venue does not hold the order"
                    );
                }
            }
        }
    }
}

/// One pass of the loop's periodic upkeep: a cancel held back for a commission asks the venue for it once its bound runs
/// out, the cancels whose commission is not coming go out (also with the stream dead: no commission can arrive then),
/// then the stranded-fill sweep. Takes the clock
/// and both bounds as parameters so a test drives it without waiting.
fn sweep_tick(
    mapper: &mut EventMapper,
    transport: &mut dyn IbkrTransport,
    events: &EventSender,
    now_ms: i64,
    grace_ms: i64,
    cancel_wait_ms: i64,
) {
    // A cancel whose bound ran out with a fill still unjoined asks the venue for the commission ONCE before the fee is
    // booked unknown: the stranded-fill ladder's first ask falls at `PENDING_COMMISSION_GRACE_MS`, after this release.
    if mapper.ask_for_held_commissions(now_ms, cancel_wait_ms) {
        transport.request_executions();
    }
    for event in mapper.release_held_cancels(now_ms, cancel_wait_ms) {
        let _ = events.blocking_send(event);
    }
    // IB's words for an accepted order whose status never said it ended: logged, the order left live.
    mapper.expire_refusals(now_ms, cancel_wait_ms);
    // ...and the amends TWS never echoed (`execution::amend`): reported, not left in flight for ever.
    for event in mapper.sweep_amends(now_ms, AMEND_CONFIRM_WAIT_MS) {
        let _ = events.blocking_send(event);
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
            order_client_id = %s.order_client_id,
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

/// What a submit is refused in while the gateway says it has no link to IBKR's servers.
const NO_SERVER_LINK: &str = "ibkr: IB Gateway says it has no link to IBKR's servers right now (1100/2110), so this order was NOT sent; send it again when the link is back";

/// Why a submit must be refused right now, or `None` when the venue can take it.
///
/// Three refusals, and they are NOT the same condition — which is the whole point of splitting
/// `IbkrInbound::StreamResync` into [`crate::transport::IbkrInbound::StreamResync`] and
/// [`crate::transport::IbkrInbound::StreamDead`]:
///
/// * **the stream is DEAD** — terminal, immediate, no wait. Nothing will re-establish it, so there
///   is nothing to wait for.
/// * **the gateway has no link to IBKR's servers** ([`IbkrTransport::server_link_broken`]) — refused at once, for as
///   long as it says so. Below the watchdog's `LOST_AFTER` (5 minutes; a nightly window measured 94 s) nothing else stops
///   an order, and written to such a gateway it waits there: the stuck-order ladder ends it at 60 s, and if the gateway
///   sends it at the `1102` the venue holds an order the engine calls dead, its accept dropped. A refused
///   order has not reached anything, so this is safe on a `live` account too.
/// * **the handshake has not landed yet** — transient, so this drains inbound for up to
///   [`READINESS_WAIT`] (folding normally, so nothing is dropped on the floor) before deciding.
///   Folding is what makes the wait productive: the very inbounds it is waiting for are the ones it
///   is draining. They go through [`deliver`] like every other inbound, so a stream that dies INSIDE
///   the wait is disclosed to the host like one that dies in the loop.
///
/// This is the caller `crates/bridges/vike-ibkr/src/execution/registry.rs`'s `is_connected` was written
/// for and did not have. It shipped `#[allow(dead_code)]` under a comment promising a "Task 10"
/// caller, while `crates/bridges/vike-ibkr/tests/ibkr_smokes/ibkr_smoke.rs`'s module doc asked a human to
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
    if transport.server_link_broken() {
        return Some(NO_SERVER_LINK.to_string());
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
/// goes through [`deliver`]. Waits for nothing once the learning is over (or was never started). Once it is over,
/// what was [`Deferred`] during it is retried HERE, before the command that waited: those arrived first.
fn settle_learning(
    mapper: &mut EventMapper,
    transport: &mut dyn IbkrTransport,
    events: &EventSender,
    within: Duration,
    deferred: &mut Vec<Deferred>,
) {
    let learning =
        |m: &EventMapper| matches!(m.learning(), Learning::OpenOrders | Learning::Executions);
    let deadline = Instant::now() + within;
    while learning(mapper) && !mapper.ids().stream_dead() && Instant::now() < deadline {
        if let Some(inbound) = transport.next_recv(Duration::from_millis(20)) {
            deliver(mapper, transport, events, inbound);
        }
    }
    retry_deferred(mapper, transport, events, deferred);
}

/// Fold one inbound and send what it produced. The ONE door every inbound takes (the loop's own drain
/// and the submit gate's readiness wait both use it), so the link transitions are disclosed from one
/// place: the fold that turns the stream dead tells the lane (`up: false`), once, and the fold that
/// finishes LEARNING the venue tells it the link is up (`up: true`), once.
fn deliver(
    mapper: &mut EventMapper,
    transport: &mut dyn IbkrTransport,
    events: &EventSender,
    inbound: IbkrInbound,
) {
    let was_dead = mapper.ids().stream_dead();
    let was_done = mapper.learning() == Learning::Done;
    for event in fold_inbound(mapper, transport, inbound) {
        let _ = events.blocking_send(event);
    }
    if !was_dead && mapper.ids().stream_dead() {
        announce_link(events, false);
        if let Some(key) = history_key(events) {
            let why = mapper.ids().stream_death_reason().map(ToString::to_string);
            vike_bridge_core::link_history::closed(
                key,
                why.as_deref().unwrap_or("the stream died"),
            );
        }
    }
    if !was_done && mapper.learning() == Learning::Done && !mapper.ids().stream_dead() {
        announce_link(events, true);
        if let Some(key) = history_key(events) {
            vike_bridge_core::link_history::opened(key);
        }
    }
}

/// This account's row in the session history. Stateless on purpose, as [`announce_link`] is: a repeat of a word changes no row.
fn history_key(events: &EventSender) -> Option<vike_secrets::link_sessions::LinkKey> {
    vike_bridge_core::link_history::trade_key(events.route_key.map(|key| key.as_str()), "exec")
}

/// Tell whoever hosts this adapter about its venue link, as the `ExecLink` the lane defines for exactly
/// this ([`vike_execution::TradeLinkUpdate`]), stamped with the account's route key (the mount's `routed` lane
/// carries it; a bare lane means the venue's default account).
///
/// * `up: false` is the ONE transition of this adapter's death: it never reconnects (a rebuilt
///   connection is a NEW adapter). Without it the host learned of the death only by probing the port, up
///   to a probe period late, and the orders it sent in that window went into a socket nobody reads.
/// * `up: true` means more than "connected": it is sent when the connection has LEARNED the venue (the
///   open orders rebound, the day's executions replayed), never earlier. A host that holds orders back
///   from a rebuilt bridge lifts the hold on it.
///
/// ⚠ The host MUST consume `Ingest::TradeLink` itself and not hand it to a core, which reads it as a
/// trade link whose watchdog it must defer: a core latches its gate down on the `up: false` and nothing
/// brings it back. The only host that mounts this bridge is the gateway daemon, which consumes it
/// (`crates/vike-gatewayhub/src/trade/session.rs`'s forwarder, pinned by the gatewayhub's tests);
/// `vike-gatewayhub` is the only crate that depends on this one.
fn announce_link(events: &EventSender, up: bool) {
    let venue = Ustr::from("ibkr");
    let update = TradeLinkUpdate {
        route_key: events.route_key.unwrap_or(venue),
        venue,
        up,
        at_timestamp_ms: vike_model::now_ms(),
    };
    if events.execution_link(update).is_err() {
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
    inbound: IbkrInbound,
) -> Vec<Event> {
    match inbound {
        IbkrInbound::NextValidId(id) => {
            mapper.ids_mut().on_next_valid_id(id);
            vec![]
        }
        IbkrInbound::AccountsReady => {
            mapper.ids_mut().set_accounts_ready(true);
            vec![]
        }
        IbkrInbound::OrderStatus(s) => mapper.on_order_status(s),
        IbkrInbound::ExecutionDetails(mut e) => {
            // While the connection learns the venue, an execution of THIS client's order (by client id)
            // that no order of this connection explains is a fill the outage swallowed: recovered by its
            // orderRef as a partial when the host taught the order's symbol, else left to the reconcile
            // (`EventMapper::on_execution_details`).
            e.recoverable = mapper.learning() == Learning::Executions
                && e.client_id != 0
                && transport.ownership().is_some_and(|own| own.client_id == e.client_id);
            mapper.on_execution_details(e)
        }
        IbkrInbound::Commission(c) => mapper.on_commission_report(c),
        IbkrInbound::Error { code, order_id, msg } => mapper.on_error(code, order_id, &msg),
        IbkrInbound::StreamDead { reason } => {
            // TERMINAL. Latch the venue closed: every later submit is refused with a synthetic
            // reject (`submit_refusal`), cancel/modify are refused loudly, and the stranded-fill
            // sweep stops writing into a socket nobody is reading.
            //
            // ⚠ Why this rather than the reconnect the old `Reconnected` name promised. Reconnecting
            // the socket backend IN PLACE is implementable — ibapi's blocking `Client` would have to be
            // rebuilt behind the `Arc` that `place_order`/`cancel_order` hold, a new
            // `order_update_stream` opened, and the order client id⇄orderId map rebuilt from `all_open_orders`
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
            // hold live exposure. They are NAMED instead, so an operator has the order client id list to take
            // to TWS, and the platform-level answer to an unknown venue state is the reconcile pass.
            let live: Vec<&str> = mapper.live_order_client_ids();
            tracing::error!(
                %reason,
                live_orders = live.len(),
                order_client_ids = ?live,
                "ibkr: ORDER-UPDATE STREAM DEAD — this connection will accept NO further orders. \
                 Nothing reconnects it: the socket backend has no reconnect and this is its terminal \
                 state (its host rebuilds the whole connection). The orders named above were in \
                 flight and their venue state is UNKNOWN — check them in TWS; a rebuilt connection \
                 learns the ones still resting, and a reconcile pass finds the rest."
            );
            mapper.ids_mut().mark_stream_dead(&reason);
            vec![]
        }
        IbkrInbound::StreamResync => {
            // Reconnect resync (Task 10): IB does not replay open orders/executions on its own, so
            // re-request the open-order snapshot; each row lands back here as `IbkrInbound::OpenOrder`
            // and rebuilds the order client id⇄orderId map via `rebind_open_order`.
            transport.request_open_orders();
            // …and the day's EXECUTIONS, which the open-order snapshot structurally cannot cover: a
            // filled order is not an open order. A socket blip between an execDetails and its
            // commissionReport is the dominant way a fill strands unemitted (`event_mapper` module
            // doc trap 5), and a blip that swallows an execDetails outright loses the fill even
            // more completely — both are recovered here, at the exact moment the loss happened,
            // with the REAL commission rather than a fabricated one. Unconditional: the replay is
            // idempotent (the mapper's `emitted` index drops re-delivered halves, and
            // `vike_execution::Engine`'s `seen_trade_ids` dedups downstream), and the bridge
            // cannot know what the blip swallowed — `pending` being empty is not evidence of
            // nothing missing, since a lost execDetails leaves no trace at all.
            transport.request_executions();
            vec![]
        }
        IbkrInbound::OpenOrder { order_id, order_ref } => {
            // Forward any synthesized OrderAccepted: a mid-flight order whose accept was lost in the
            // socket blip is unstuck here (no-order-vanishes). External orders return empty.
            mapper.rebind_open_order(order_id, &order_ref)
        }
        IbkrInbound::VenueOrder(row) => {
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
        IbkrInbound::Connected => {
            // A fresh connection: LEARN the venue. IB replays nothing on its own, so the open orders
            // are asked for now (this connection's id map is empty); the executions are asked for when
            // THEIR rows have all arrived (`SnapshotEnd`), because a replayed execution only resolves
            // to an order that is already bound.
            mapper.begin_learning();
            transport.request_open_orders();
            vec![]
        }
        IbkrInbound::SnapshotEnd { snapshot, complete } => {
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
                        SnapshotRequest::OpenOrders => "open-order",
                        SnapshotRequest::Executions => "execution",
                    }
                ),
            }
            vec![]
        }
    }
}

/// Production connect: build the socket transport and hand it to the exec loop. With the
/// `ibkr-socket` feature ON, this connects the blocking `ibapi` client to the Gateway/TWS
/// named by `config`; with it OFF, it degrades to `Unavailable` — the app root then keeps the venue
/// paper, exactly like the dukascopy missing-sidecar gate. `events` names the account whose
/// one link cell (`vike_bridge_core::venue_link::venue_link`, by the route key) the transport's link watch writes; the
/// transport delivers all venue events through its
/// own pump. Beside it, the account the session trades (`IbkrTransportSocket::account`).
#[cfg(feature = "ibkr-socket")]
pub(crate) fn connect_socket(
    config: &crate::config::IbkrConfig,
    events: &EventSender,
) -> Result<(Box<dyn IbkrTransport>, String), crate::transport::error_codes::IbkrError> {
    let link = events.route_key.map(|key| vike_bridge_core::venue_link::venue_link(&key));
    let transport = crate::transport::IbkrTransportSocket::connect(config, link)?;
    let account = transport.account().to_string();
    Ok((Box::new(transport), account))
}

/// Feature-off variant: no socket backend compiled → always `Unavailable` (the account does not run).
#[cfg(not(feature = "ibkr-socket"))]
pub(crate) fn connect_socket(
    _config: &crate::config::IbkrConfig,
    _events: &EventSender,
) -> Result<(Box<dyn IbkrTransport>, String), crate::transport::error_codes::IbkrError> {
    Err(crate::transport::error_codes::IbkrError::Unavailable)
}

/// Production connect for the cpapi backend: build the `IbkrTransportCpapi` (REST + WS pump to a
/// running, browser-authenticated Client Portal Gateway) and hand it to the exec loop. With the
/// `ibkr-cpapi` feature OFF, or an unreachable/unauthenticated gateway, it degrades to
/// `Unavailable` → the account is NOT RUNNING (same gate as the socket path).
#[cfg(feature = "ibkr-cpapi")]
pub(crate) fn connect_cpapi(
    config: &crate::config::IbkrConfig,
    _events: &EventSender,
) -> Result<Box<dyn IbkrTransport>, crate::transport::error_codes::IbkrError> {
    let transport = crate::transport::IbkrTransportCpapi::connect(config)?;
    Ok(Box::new(transport))
}

/// Feature-off variant: no cpapi backend compiled → always `Unavailable` (the account does not run).
#[cfg(not(feature = "ibkr-cpapi"))]
pub(crate) fn connect_cpapi(
    _config: &crate::config::IbkrConfig,
    _events: &EventSender,
) -> Result<Box<dyn IbkrTransport>, crate::transport::error_codes::IbkrError> {
    Err(crate::transport::error_codes::IbkrError::Unavailable)
}

#[path = "execution_late_close_tests.rs"]
#[cfg(test)]
mod execution_late_close_tests;
#[path = "execution_tests.rs"]
#[cfg(test)]
mod execution_tests;
