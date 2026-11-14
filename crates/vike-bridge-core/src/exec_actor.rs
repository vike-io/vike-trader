//! The exec-thread scaffold shared by the command-driven venue `ExecutionClient`s.
//!
//! [`ExecutionClientActor`] owns an mpsc channel, a dedicated OS thread draining [`ExecutionClientActorCommand`]s, the
//! [`ExecutionClient`] impl, and a `Drop`/`detach` that sends `Shutdown` and JOINS the thread —
//! deterministic teardown matters (a stranded feed thread is a 0xC0000409 on process exit). A venue
//! supplies only its command loop `run(Receiver<ExecutionClientActorCommand>)`; extra background threads (e.g.
//! OANDA's fills stream) attach via [`ExecutionClientActor::with_background`] and are flag-stopped and joined
//! on teardown too. For the REST-driven crypto venues the command loop itself is shared:
//! [`run_loop`] (command→REST→ingest plus the gap-sentinel, generic over [`VenueRest`]).
//!
//! ## Venue hooks (no wrapper)
//! What a venue's `ExecutionClient` adds to the actor's own doors is handed in as a hook, each named after
//! the method it serves ([`crate::execution_client::hooks`]): [`ExecutionClientActor::with_before_submit`],
//! [`ExecutionClientActor::with_adopt_restored`], [`ExecutionClientActor::with_wire_id_for`], and [`ExecutionClientActor::with_confirm`]. A
//! bridge then returns the actor itself, and no method can be left to the trait's no-op by a forward
//! nobody wrote. An actor given none of them answers each of those methods as the trait's default does.
//!
//! ## HALT kill-switch (submit boundary)
//! `submit` (and `modify`) consult the HALT file sentinel (`vike_execution::halt::sentinel_engaged`)
//! BEFORE touching the command channel: while the HALT file exists, a submit is refused with a
//! synthesized terminal
//! `OrderRejected` (never a silent vanish — the venue-adapter contract) and a modify is refused with
//! a NON-terminal `OrderModifyRejected` (the resting order keeps its terms). Cancel is deliberately
//! never gated — halt must let an operator reduce/exit, never trap a position. This works even when
//! the runtime is wedged: an operator `touch`es the file over ssh and the next submit stops.
//!
//! ## Bulk cancel (opt-in, per venue)
//! [`ExecutionClientActorCommand::CancelBatch`] carries a multi-order cancel INTACT, and is delivered ONLY to a
//! venue that declared it handles one ([`ExecutionClientActor::with_bulk_cancel`]); every other venue gets `n`
//! per-id [`ExecutionClientActorCommand::Cancel`]s. Opt-in because the trade is not free: Polymarket buys one round
//! trip instead of `n` on an emergency flatten, but its `cancel-all` debits `1 + n` rate tokens where
//! `n` singles debit `n` — a decision only the venue's own budget can make. Hyperliquid declares it
//! because its cancel action is batch-first (one signed action with N items).
//!
//! ## Bulk submit and a queued adopt (opt-in, per venue)
//! [`ExecutionClientActorCommand::SubmitBatch`] carries a multi-order submit INTACT to a venue whose order action is
//! batch-first ([`ExecutionClientActor::with_bulk_submit`]), and [`ExecutionClientActorCommand::Adopt`] teaches the venue's exec thread
//! an order a previous session left resting, on the lane `Cancel` rides ([`ExecutionClientActor::with_adopt_queued`]).
//! Both reach only a loop that declared them, the interlock [`ExecutionClientActorCommand::CancelBatch`] has, so every
//! other loop's arm is the shared, unreachable [`submit_batch_undeclared`] / [`adopt_undeclared`].
//!
//! ## Submit fence (a fill ahead of its order's own answer)
//! [`run_loop`] sends an order's `[OrderSubmitted, OrderAccepted]` only after the venue's submit
//! returns, while a venue's user-data pump sends a fill from its own thread the moment it arrives. A
//! bridge whose stream can deliver a fill with no accept of its own ahead of it wraps its REST client
//! in [`submit_fence::VenueRestOrderSubmitFence`] and routes the pump and the replay through [`submit_fence::SubmitFence`];
//! `submit_fence`'s module doc has the race and the roster.
//!
//! ## History-replay floor (the restart law)
//! [`run_loop`]'s gap-sentinel calls a venue `resync` closure that replays a WINDOW OF VENUE
//! HISTORY — `/v5/order/history` + `/v5/execution/list` on bybit, `allOrders` + `userTrades` on
//! binance/aster, `get_order_history_by_instrument` + `get_user_trades_by_instrument` on deribit,
//! `orders-history` + `fills-history` on okx — bounded by a ROW COUNT and by nothing else. The
//! core's `seen_trade_ids` dedup is EMPTY in a fresh process, so without a floor the first firing
//! after a restart folds the venue's retained execution history (fills for orders this process
//! never placed included) through the non-idempotent `Account::apply_fill`
//! (`balance -= commission`, `realized_pnl +=`). Those rows are ALREADY in the venue-reported
//! balance/position, so re-emitting one double-counts it.
//!
//! Hence [`run_loop`] samples its own spawn instant ([`vike_model::now_ms`], once, on entry) and
//! NEVER forwards a resync event stamped before it (`is_pre_spawn`). The floor lives in the ONE
//! shared loop, not in five venue closures, so it holds whatever a venue's `resync` returns. A
//! SPAWN-time floor is the right shape: the replay closes gaps WITHIN a process's life (the
//! first-connect subscribe race, a lost WS frame); adopting state that existed BEFORE the process
//! started is reconciliation's job (`vike_execution::recon::resolve`: policy, quarantine, operator
//! confirm).
//!
//! `bybit`'s `BybitFundingPoller` (`crates/bridges/bybit/src/execution/funding.rs`) guards the same hazard
//! the same way, with two differences: its floor is a PARAMETER (`0` = no floor, for tests/probes)
//! while this one is sampled INSIDE [`run_loop`] (no legitimate caller wants it off); and funding
//! can also narrow the request with `startTime`, while these history endpoints are row-count-bounded
//! — so here the client-side timestamp check IS the guarantee.
//!
//! **What the floor does NOT touch.** A mid-process WS reconnect still delivers every fill from the
//! gap (stamped AFTER spawn). An UNSTAMPED event (`ts == 0`) also rides through: a missing
//! timestamp is not evidence of being historical, and no timestamp is ever fabricated here.

use std::borrow::Cow;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, RecvTimeoutError, SendError, Sender, channel};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use vike_execution::{EventSender, ExecutionClient, RestoredOrder};
use vike_model::OrderCancelIntent;
use vike_model::OrderRequest;
use vike_model::events::{Event, OrderCancelRejected, OrderModifyRejected, OrderRejected};

use crate::execution_client::hooks::{AdoptRestoredFn, BeforeSubmitFn, WireIdForFn};
use crate::rest::{OrderCancelAck, VenueRest};

pub mod submit_fence;

/// A command processed by a venue exec thread.
pub enum ExecutionClientActorCommand {
    /// Place one order. The `Instant` is when it joined this queue, for [`too_old_to_send`]: a loop
    /// that can sit behind a blocking call asks it before anything else.
    Submit(Box<OrderRequest>, Instant),
    /// Cancel one resting order. Carries the core's [`OrderCancelIntent`] so a venue that meters its
    /// cancels decides (and debits) on ITS OWN thread, where the budget lives — deciding on the
    /// caller's thread would let a burst of routine cancels each pass against a balance none of them
    /// had spent yet. A venue with no budget ignores the field; `Unspecified` is the flatten-safe
    /// default.
    Cancel {
        order_client_id: String,
        intent: OrderCancelIntent,
    },
    /// Cancel MANY resting orders as ONE command, so a venue with a native bulk/mass-cancel
    /// endpoint sees the batch INTACT. Carries the core's [`OrderCancelIntent`] for the same reason
    /// [`ExecutionClientActorCommand::Cancel`] does, and carries it ONCE, because the core classifies a batch as a
    /// whole (`vike_execution::Engine::mass_cancel_with_intent`), never per id.
    ///
    /// ⚠ **Delivered ONLY to a venue that DECLARED it handles one** —
    /// [`ExecutionClientActor::with_bulk_cancel`].
    /// Without that declaration [`ExecutionClient::cancel_batch_with_intent`] fans the ids out into
    /// per-id [`ExecutionClientActorCommand::Cancel`]s before they reach the channel (`n` singles, in the caller's
    /// order). That opt-in IS the safety interlock: a venue cannot be handed a batch shape it never
    /// wrote code for, so the arm every loop must still write can be the one shared, unreachable
    /// fallback [`cancel_batch_undeclared`].
    CancelBatch {
        order_client_ids: Vec<String>,
        intent: OrderCancelIntent,
    },
    /// Reprice/resize a resting order in place (DOM drag-to-modify). Carries the whole resting order
    /// so a venue can read its side/current terms; venues without a native amend leave it a no-op
    /// (the order keeps its terms — nothing vanishes).
    Modify {
        order: Box<OrderRequest>,
        new_qty: Option<f64>,
        new_price: Option<f64>,
        /// A stop-limit's or a take-profit's new TRIGGER (`ExecutionClient::modify_trigger`). ⚠ `Some`
        /// ONLY on a loop that DECLARED it reads it ([`ExecutionClientActor::with_trigger_amend`]): every other
        /// actor refuses a trigger amend before the channel, so a loop that never heard of this field
        /// can match it away with `..` and never drop a trigger.
        new_trigger_price: Option<f64>,
    },
    /// Place MANY orders as ONE command, so a venue whose order action is batch-first (hyperliquid's
    /// `/exchange`) signs them as one request. The `Instant` is when the batch joined the queue: the loop asks
    /// [`too_old_to_send`] of each order.
    ///
    /// ⚠ **Delivered ONLY to a venue that DECLARED it handles one** ([`ExecutionClientActor::with_bulk_submit`]). For every
    /// other venue `ExecutionClient::submit_batch` fans the orders out into per-order [`ExecutionClientActorCommand::Submit`]s
    /// before the channel, so the arm every other loop must still write is the shared, unreachable
    /// [`submit_batch_undeclared`].
    SubmitBatch(Vec<OrderRequest>, Instant),
    /// Teach the venue's exec thread an order a previous session left resting (`ExecutionClient::adopt_restored`),
    /// on the lane [`ExecutionClientActorCommand::Cancel`] rides, so a cancel sent after it finds what it taught.
    ///
    /// ⚠ **Delivered ONLY to a venue that DECLARED it reads one** ([`ExecutionClientActor::with_adopt_queued`]); every other
    /// loop's arm is the shared, unreachable [`adopt_undeclared`].
    Adopt(RestoredOrder),
    Shutdown,
}

/// The outcome of a venue cancel attempt, mapped to a canonical event by [`cancel_event`].
pub enum ExecutionClientActorCancelOutcome {
    /// The venue confirmed the cancel — terminal `OrderCanceled`.
    Canceled,
    /// The cancel could not be delivered or confirmed (venue error, unknown/already-gone order,
    /// ambiguous ack). NON-terminal `OrderCancelRejected` — the order, if still live, stays live.
    Rejected(String),
}

/// Map a cancel attempt to its canonical event so a failed cancel is NEVER swallowed: success →
/// terminal `OrderCanceled`, any failure → non-terminal `OrderCancelRejected`. Shared by every
/// command-driven venue's `Cancel` branch.
pub fn cancel_event(order_client_id: &str, outcome: ExecutionClientActorCancelOutcome) -> Event {
    match outcome {
        ExecutionClientActorCancelOutcome::Canceled => {
            Event::OrderCanceled(vike_model::events::OrderCanceled {
                order_client_id: order_client_id.to_string(),
                reason: String::new().into(),
                timestamp: 0,
            })
        }
        ExecutionClientActorCancelOutcome::Rejected(reason) => {
            Event::OrderCancelRejected(OrderCancelRejected {
                order_client_id: order_client_id.to_string(),
                reason: reason.into(),
                timestamp: 0,
            })
        }
    }
}

/// The reason [`cancel_batch_undeclared`] puts on every id it refuses. A constant so the message a
/// mis-wired venue produces is greppable from the one place that can produce it.
pub const CANCEL_BATCH_UNDECLARED_REASON: &str =
    "venue declared no bulk-cancel path (ExecutionClientActor::with_bulk_cancel)";

/// The [`ExecutionClientActorCommand::CancelBatch`] arm for a venue command loop that has NO bulk-cancel path.
///
/// **Unreachable by construction**: [`ExecutionClientActor`] only puts a `CancelBatch` on the channel for a
/// venue that called [`ExecutionClientActor::with_bulk_cancel`], so an undeclared loop's batches were already
/// fanned out into per-id [`ExecutionClientActorCommand::Cancel`]s. The arm still has to be WRITTEN (the enum is
/// matched exhaustively), and the tempting empty arm (`=> {}`) would turn a future mis-wiring into
/// `n` orders vanishing with no event — the one thing the emitter-split contract forbids.
///
/// So it refuses every id NON-terminally: the orders are still resting at the venue, the caller
/// re-offers them, and the `error!` names the missing declaration.
pub fn cancel_batch_undeclared(events: &EventSender, order_client_ids: &[String]) {
    tracing::error!(
        target: "vike_bridge_core",
        count = order_client_ids.len(),
        "a CancelBatch reached a venue loop with no bulk-cancel path — every id refused \
         NON-terminally and every order left resting. Either wire the loop's CancelBatch arm or \
         drop its `with_bulk_cancel` declaration."
    );
    for order_client_id in order_client_ids {
        // `CoreGone` = the core has exited = shutting down; nothing to deliver to.
        let _ = events.blocking_send(Event::OrderCancelRejected(OrderCancelRejected {
            order_client_id: order_client_id.clone(),
            reason: CANCEL_BATCH_UNDECLARED_REASON.to_string().into(),
            timestamp: 0,
        }));
    }
}

/// The reason [`submit_batch_undeclared`] puts on every order it refuses, greppable like
/// [`CANCEL_BATCH_UNDECLARED_REASON`].
pub const SUBMIT_BATCH_UNDECLARED_REASON: &str = "this order was NOT sent: the venue declared no bulk-submit path (ExecutionClientActor::with_bulk_submit)";

/// The [`ExecutionClientActorCommand::SubmitBatch`] arm for a venue command loop that has NO bulk-submit path.
///
/// **Unreachable by construction**, as [`cancel_batch_undeclared`] is: [`ExecutionClientActor`] queues a `SubmitBatch` only
/// for a venue that called [`ExecutionClientActor::with_bulk_submit`]. A mis-wiring must not drop the orders without a word,
/// and nothing was sent, so each is refused TERMINALLY ([`crate::rest::refused`]) and the `error!` names the
/// missing declaration.
pub fn submit_batch_undeclared(events: &EventSender, requests: &[OrderRequest]) {
    tracing::error!(
        target: "vike_bridge_core",
        count = requests.len(),
        "a SubmitBatch reached a venue loop with no bulk-submit path — every order refused and none \
         sent. Either wire the loop's SubmitBatch arm or drop its `with_bulk_submit` declaration."
    );
    for request in requests {
        for event in crate::rest::refused(request, SUBMIT_BATCH_UNDECLARED_REASON.to_string()) {
            // `CoreGone` = the core has exited = shutting down; nothing to deliver to.
            let _ = events.blocking_send(event);
        }
    }
}

/// The [`ExecutionClientActorCommand::Adopt`] arm for a venue command loop that reads none. **Unreachable by construction**
/// ([`ExecutionClientActor::with_adopt_queued`] is the only way one is queued). An adopt places nothing, so there is no
/// order to answer; the `error!` names the missing declaration, because the teaching it carried is lost.
pub fn adopt_undeclared(order: &RestoredOrder) {
    tracing::error!(
        target: "vike_bridge_core",
        order_client_id = %order.order_client_id,
        "an Adopt reached a venue loop that reads none — what it taught is lost. Either wire the loop's \
         Adopt arm or drop its `with_adopt_queued` declaration."
    );
}

/// Send `event` on `events`; the FIRST time the lane is found closed (`CoreGone`: the core thread has
/// exited) raise `warned` and log ONE `warn!` naming the `actor`. Every later drop through the same
/// flag is silent: this runs on the exec thread and the submit/cancel boundary, and a core that is
/// gone stays gone, so one line per actor is the whole signal and a line per message would be a
/// flood. The hot (delivered) path costs nothing extra: the flag is touched only on failure.
fn send_or_warn_once(events: &EventSender, warned: &AtomicBool, actor: &str, event: Event) {
    if events.blocking_send(event).is_err() && !warned.swap(true, Ordering::Relaxed) {
        tracing::warn!(
            target: "vike_bridge_core",
            actor,
            "the core has exited: this actor's events are no longer delivered (logged once per actor, later \
             drops are silent)"
        );
    }
}

/// One background poll's answer onto the lane (a funding poll's, bybit and okx): its events forwarded in
/// order and `fail_streak` reset, or the failure counted. `Ok(Some(error))` only on the FIRST failure of
/// a streak, so the caller warns once per streak under its own `tracing` target; `Err` = the core is
/// gone and the thread should exit.
pub fn forward_poll<E>(
    polled: Result<Vec<Event>, E>,
    events: &EventSender,
    fail_streak: &mut u32,
) -> Result<Option<E>, vike_execution::CoreGone> {
    match polled {
        Err(e) => {
            *fail_streak += 1;
            Ok((*fail_streak == 1).then_some(e))
        }
        Ok(evs) => {
            *fail_streak = 0;
            for event in evs {
                events.blocking_send(event)?;
            }
            Ok(None)
        }
    }
}

/// Every [`Event`]'s wall-clock stamp in epoch milliseconds, by an EXHAUSTIVE match — a new
/// `Event` variant is a COMPILE error here rather than a silent hole in [`run_loop`]'s history
/// floor (the restart law; see the module doc). `0` means the emitter left it unstamped, which
/// `is_pre_spawn` treats as "not evidence of being historical".
pub fn event_ts(event: &Event) -> i64 {
    match event {
        Event::Fill(e) => e.timestamp,
        Event::OrderSubmitted(e) => e.timestamp,
        Event::OrderAccepted(e) => e.timestamp,
        Event::OrderRejected(e) => e.timestamp,
        Event::OrderDenied(e) => e.timestamp,
        Event::OrderTriggered(e) => e.timestamp,
        Event::OrderPartiallyFilled(e) => e.timestamp,
        Event::OrderFilled(e) => e.timestamp,
        Event::OrderCanceled(e) => e.timestamp,
        Event::OrderExpired(e) => e.timestamp,
        Event::OrderLiquidated(e) => e.timestamp,
        Event::OrderModified(e) => e.timestamp,
        Event::PositionOpened(e) => e.timestamp,
        Event::PositionChanged(e) => e.timestamp,
        Event::PositionClosed(e) => e.timestamp,
        Event::AccountState(e) => e.timestamp,
        Event::Funding(e) => e.timestamp,
        Event::PositionLiquidated(e) => e.timestamp,
        Event::OrderCancelRejected(e) => e.timestamp,
        Event::OrderModifyRejected(e) => e.timestamp,
        Event::FeeAdjustment(e) => e.timestamp,
    }
}

/// The restart law's one predicate: is this replayed event evidence of activity that happened
/// BEFORE `spawn_ms`, i.e. before this process could have caused it?
///
/// **The ONE predicate of all THREE history-replay consumers**: [`run_loop`]'s gap-sentinel,
/// `crate::websocket_session::user::run_user_resync` (the reconnect-gated replay of the SAME venue `resync`
/// closures), and — outside this crate, hence `pub` —
/// `vike_hyperliquid::execution::user_data::map_frame_to_events`'s first-`userFills`-snapshot floor (HL
/// reaches neither lane above, so its snapshot IS its history-replay path). A second spelling of
/// "is this row older than my process" would let the lanes disagree about the boundary invisibly.
///
/// A POSITIVE stamp strictly below the floor is the only thing that qualifies. `ts == 0` (an
/// emitter that left the stamp blank — `cancel_event`, the live submit lane) and any stamp at or
/// after the floor ride through: the floor may only ever REMOVE what predates the process, never
/// guess at what an unstamped event was.
///
/// The floor is a LOCAL clock reading and `ts` is the VENUE's, compared with no slack (as
/// `BybitFundingPoller` does). For the two lanes in THIS crate the skew is bounded by the venues'
/// own `recvWindow` (~5s; a larger offset fails every signed request), and an event inside that band
/// happened AT the start, before this process placed anything.
///
/// ⚠ **That `recvWindow` argument does NOT hold for the third consumer, and it must not be relied on
/// there.** Hyperliquid's nonce window is `(T − 2 days, T + 1 day)`
/// (`crates/bridges/hyperliquid/src/signing/hash.rs`'s `NonceManager`), so a signed HL request
/// survives skew that would drag a fresh fill's venue stamp below a local floor. The HL caller
/// therefore does not use this predicate ALONE: it drops a row only when this returns `true` **and**
/// `crates/bridges/hyperliquid/src/execution/registry.rs`'s `OrderClientIdVenueRegistry`'s `resolve` says the order was not
/// minted by this process. A clock-only floor on that lane would silently drop a fill that exists
/// nowhere else.
pub fn is_pre_spawn(event: &Event, spawn_ms: i64) -> bool {
    let ts = event_ts(event);
    ts > 0 && ts < spawn_ms
}

/// How long after the last command [`run_loop`]'s one-shot gap-sentinel fires its `resync`: the
/// `fill_sentinel` argument every command-driven venue passes today. A MECHANISM default and not a
/// fact about any venue (decision 0160), so it is a constant here and not a row of
/// `vike_venues::venue_timings`: binance, aster, bybit, okx and deribit each copied this
/// number into a private `FILL_SENTINEL`.
pub const DEFAULT_FILL_SENTINEL: Duration = Duration::from_secs(5);

/// How long a submit may wait on an exec thread's queue and still be sent.
///
/// ⚠ **One thread serves that queue and its calls block**, so a stalled call makes the orders behind it
/// wait. The core gives up on an unanswered order at its last-resort reject
/// (`crates/vike-core/src/runtime/watchdog/orders.rs`'s `sweep_stuck_orders`), and its `confirm` only
/// reads and stays silent for an order not dispatched yet, so nothing stops the POST. Sent after that,
/// the order rests at the venue under an id the core has rejected, its accept is dropped as an illegal
/// transition, and nothing tracks it.
///
/// Why 20 s: the core's reject comes first at the ack timeout plus two graces, 60 s at the shipped
/// defaults, and an order let through at this age still has its POST (30 s, `crate::http`'s
/// `DEFAULT_TIMEOUT`) and the re-query behind a lost answer (two 5 s reads on bybit and binance) inside
/// that: 20 + 30 + 10 = 60, which `crates/bridges/bybit/src/execution/execution_tests.rs`'s
/// `the_slowest_submit_is_answered_before_the_core_can_reject_it` pins. The 20 is what it may have spent by the POST: a
/// read in front of the POST asks again ([`too_old_to_post`]). A run profile that lowers those
/// guards shortens the core's side, and a bridge cannot read it.
///
/// ⚠ **The clock starts when the order joins this queue**, while the core counts from its own
/// registration (`crates/vike-execution/src/execution_engine/mod.rs`'s `gate_and_register`). The two starts
/// differ by the trade lane's transit, which the sum does not cover: milliseconds on one box. A lost
/// link restarts the core's count (`crates/vike-core/src/runtime/execution_link.rs`'s `execution_link_resumes`)
/// and re-sends no written order.
///
/// A cancel has no age: a late cancel is safe and a dropped one is not. An amend has none here either:
/// it acts on an order the core still tracks, so a late one cannot strand anything. A close has none
/// ([`closes_at_once`]).
pub const MAX_SUBMIT_WAIT: Duration = Duration::from_secs(20);

/// **Whether `request` is a close that goes at once, so no wait may refuse it**: a `reduce_only` MARKET order with
/// no trigger. The core marks every close it sends so, on every venue, whatever that venue's wire carries: a flatten
/// leg (`crates/vike-core/src/runtime/apply/exit.rs`'s `lower_flatten`), a margin call's and a latched mount's close
/// (`crates/vike-core/src/runtime/watchdog/risk.rs`), a fired core-held stop, and what the position covers of a plain
/// stop's or trailing arm's fire (`crates/vike-core/src/runtime/strategy_drive/conditionals.rs`'s `submit_fired`; the part
/// of a larger one past the position can open, so it is a plain order of its own and can be refused for its age).
///
/// A close refused for its age leaves the position OPEN: an operator's `close-all` and a plain stop get no second try from
/// the core, where only a margin call's next pass, a latched mount and a core-held stop ask again. Sent late, what it can do is
/// narrow: a market order does not rest under an id the core has rejected, and its bare `Fill` folds whatever the
/// order table reads (past the core's reject the position is right and the order reads REJECTED). A venue with the
/// flag on its wire trims or refuses it; ig's close route and fxcm's hedged one close by position and cannot open; a
/// spot sale and a Polymarket sale spend what the wallet holds. A venue that ignores the flag could open the other way
/// if the position went while the order waited: [`ExecutionClientActorLateCloses`] is what its loop asks.
pub fn closes_at_once(request: &OrderRequest) -> bool {
    request.reduce_only
        && request.order_type == vike_model::OrderType::Market
        && request.trigger_price.is_none()
}

/// How long after a close went out past [`MAX_SUBMIT_WAIT`] the closes behind it still ask the position first.
const LATE_CLOSE_MEMORY: Duration = Duration::from_secs(120);

/// **Which closes a loop whose venue ignores the flag sizes against the position it reads NOW** (oanda on a netting
/// account, which keeps the read though it says `positionFill: REDUCE_ONLY` since 2026-11-09, fxcm on a netted one, alpaca, vike-ibkr): one past [`MAX_SUBMIT_WAIT`], whose position may have gone while it waited,
/// and any behind it for [`LATE_CLOSE_MEMORY`]. Past the core's last-resort reject the core no longer counts the late
/// one as cover, so a margin call or a latched mount can queue another close behind it, young, for the same position
/// (`crates/vike-core/src/runtime/watchdog/closes.rs`), and two would flip it here. Every other close reads nothing.
#[derive(Default)]
pub struct ExecutionClientActorLateCloses(Option<Instant>);

impl ExecutionClientActorLateCloses {
    /// Whether the close `request`, which joined the queue at `queued_at`, must read the position first.
    pub fn must_read(&mut self, request: &OrderRequest, queued_at: Instant) -> bool {
        if !closes_at_once(request) {
            return false;
        }
        if queued_at.elapsed() > MAX_SUBMIT_WAIT {
            self.0 = Some(Instant::now());
            return true;
        }
        self.0.is_some_and(|at| at.elapsed() < LATE_CLOSE_MEMORY)
    }
}

/// What the close `request` may carry given the position `held` the loop just read (signed, long positive): `Ok(None)`
/// as it is, `Ok(Some(qty))` cut to what still faces it, and `Err(words)` when nothing does, which is the terminal
/// reject of an order that was never sent. A failed read is no evidence, and a close is never refused on none: it goes
/// (vike-ibkr refuses before it asks here: `crates/bridges/vike-ibkr/src/execution/mod.rs`'s `size_late_close` has why).
pub fn late_close_size(
    request: &OrderRequest,
    held: Result<f64, String>,
) -> Result<Option<f64>, String> {
    let held = match held {
        Ok(held) if held.is_finite() => held,
        Ok(_) | Err(_) => {
            tracing::warn!(
                target: "vike_bridge_core",
                order_client_id = %request.order_client_id,
                "a close that waited read no position, so it goes as it is"
            );
            return Ok(None);
        }
    };
    let facing = if request.side < 0 { held } else { -held };
    if facing <= 0.0 {
        return Err(format!(
            "this close was NOT sent: it waited behind earlier calls to the venue, and the venue now holds nothing \
             it could close ({} on `{}`), so sending it would open a position. Check the position",
            if held == 0.0 { "flat".to_string() } else { format!("{held}") },
            request.symbol
        ));
    }
    Ok((facing < request.qty).then_some(facing))
}

thread_local! {
    /// When the submit this exec thread is serving joined the queue. `VenueRest::submit_order` takes no stamp, and every
    /// layer of a client (a router, its mode check, the wire client) runs on this one thread.
    static SERVING_SINCE: std::cell::Cell<Option<Instant>> = const { std::cell::Cell::new(None) };
}

/// Run `serve`, the submit of an order that joined the queue at `queued_at`, so a layer of its client can ask the order's
/// age again ([`too_old_to_post`]).
pub fn serving_submit<T>(queued_at: Instant, serve: impl FnOnce() -> T) -> T {
    SERVING_SINCE.set(Some(queued_at));
    let answer = serve();
    SERVING_SINCE.set(None);
    answer
}

/// When the submit this exec thread is serving joined the queue: `Some` inside [`serving_submit`] only. A layer of a
/// client that needs the order's wait itself, not only [`too_old_to_post`]'s verdict, reads it here (alpaca asks
/// [`ExecutionClientActorLateCloses::must_read`] with it).
pub fn serving_since() -> Option<Instant> {
    SERVING_SINCE.get()
}

/// [`too_old_to_send`] asked again just before the POST, by a layer that spent time after the loop's own ask: a grid
/// fetched for a symbol, a mode read, a re-read universe. [`MAX_SUBMIT_WAIT`]'s sum counts from the POST, and the core's
/// clock does not wait for those reads. `None` outside [`serving_submit`].
pub fn too_old_to_post(request: &OrderRequest) -> Option<String> {
    serving_since().and_then(|queued_at| too_old_to_send(request, queued_at))
}

/// The words of the terminal reject of `request`, which joined the queue at `queued_at` and is too old to send now, or
/// `None` while it is not ([`MAX_SUBMIT_WAIT`]) and always for a close ([`closes_at_once`]). Asked before anything is
/// read or signed.
pub fn too_old_to_send(request: &OrderRequest, queued_at: Instant) -> Option<String> {
    if closes_at_once(request) {
        return None;
    }
    let waited = queued_at.elapsed();
    (waited > MAX_SUBMIT_WAIT).then(|| {
        format!(
            "this order was NOT sent. It waited {:.1} s behind earlier calls to the venue, past the {} s \
             an order may wait, so nothing was signed or sent. Send it again if it is still wanted.",
            waited.as_secs_f64(),
            MAX_SUBMIT_WAIT.as_secs()
        )
    })
}

/// What a cancel the venue answered "already gone" says at once: the order may have just filled, so
/// this is no statement about how it ended, and its final state follows ([`run_loop`]).
pub const ALREADY_GONE_REASON: &str =
    "the venue no longer holds this order (it may have just filled); its final state follows";

/// How many orders with no answer one lane remembers ([`SilentOrders`]): past it the oldest goes, and a fill on that one
/// is refused by the engine as it would be without this, its position still right.
const MAX_SILENT_ORDERS: usize = 64;

/// **The orders whose submit returned with no answer at all**: the POST got none and its re-query failed or found
/// nothing, so nothing is said and the core's backstop decides (decision 0171's amendment). A FILL of one is the venue
/// saying it holds the order after all, so it first accepts it ([`crate::event_map::accept_implied_by_fill`]): the wrap
/// alone is refused on a `Submitted` order and not kept, the order would read REJECTED at 60 to 75 s with its position
/// right, and a bracket's entry would never release its stop. Forgotten at that first fill; an accept that lands first
/// (a stream's own, a confirm's) makes this one a repeat the engine refuses. Bounded, and per lane: only an order that
/// got no answer is ever here, and nothing else pays more than one `is_empty`.
#[derive(Default)]
struct SilentOrders(Vec<String>);

impl SilentOrders {
    fn note(&mut self, order_client_id: &str) {
        if self.0.len() >= MAX_SILENT_ORDERS {
            self.0.remove(0);
        }
        self.0.push(order_client_id.to_string());
    }

    /// The accept `event` owes first, if it is a fill wrap of an order remembered here.
    fn accept_for(&mut self, event: &Event) -> Option<Event> {
        if self.0.is_empty() {
            return None;
        }
        let accept = crate::event_map::accept_implied_by_fill(event, false)?;
        let at = self.0.iter().position(|order_client_id| {
            Some(order_client_id.as_str()) == accepted_order_client_id(&accept)
        })?;
        self.0.remove(at);
        Some(accept)
    }
}

fn accepted_order_client_id(event: &Event) -> Option<&str> {
    match event {
        Event::OrderAccepted(a) => Some(&a.order_client_id),
        // every other event is no accept
        _ => None,
    }
}

/// Whether what a submit returned says nothing about the order: only the bridge's own `OrderSubmitted`.
fn no_answer(own: &[Event]) -> bool {
    own.iter().all(|event| matches!(event, Event::OrderSubmitted(_)))
}

/// The shared venue exec command pump: the command→REST→ingest mapping (generic over the
/// [`VenueRest`] surface so it is unit-tested with a mock), plus the gap-sentinel. Every
/// command-driven crypto venue (binance-family/deribit/bybit/okx) and alpaca drive their [`ExecutionClientActor`]
/// thread with this ONE loop; only the venue's `run`/`run_spot`/`run_perp` driver (REST types, hosts,
/// signers, `tracing` targets, the resync closure) stays per-venue.
///
/// `Submit` forwards `submit_order`'s `[Submitted, Accepted|Rejected]`, or `[Submitted]` alone when the venue gave no
/// answer, and then the first fill of that order goes behind the accept it implies ([`SilentOrders`]); `Cancel` maps
/// success→`OrderCanceled` / failure→`OrderCancelRejected` via [`cancel_event`], and "already gone"
/// ([`OrderCancelAck::AlreadyGone`]) to a non-terminal `OrderCancelRejected` that the gap-sentinel settles:
/// the replay's own terminal wins (a filled order ends FILLED), and with none after `fill_sentinel`
/// has passed the cancel ends `OrderCanceled`, as it used to at once, so no order waits for ever (this
/// loop's end does the same); `CancelBatch`
/// routes to [`VenueRest::cancel_batch`] and reports its one whole-batch `Result` per id; `Modify`
/// forwards `modify_order` (a native amend where the venue wires one; otherwise the `VenueRest`
/// default no-op — `[]`, the order keeps its terms).
/// `recv_timeout` arms a ONE-SHOT `resync` `fill_sentinel` after the last command, so a fill/cancel
/// the WS lost (esp. the first-connect subscribe race) is recovered; a normal WS delivery makes the
/// replay a dedup'd no-op. `Shutdown` returns so the caller tears down.
pub fn run_loop<R, F>(
    rest: &R,
    events: &EventSender,
    rx: Receiver<ExecutionClientActorCommand>,
    resync: F,
    fill_sentinel: Duration,
) where
    R: VenueRest,
    F: Fn() -> Vec<Event>,
{
    // The history-replay floor (the restart law — see the module doc). Sampled ONCE, here, because
    // this call IS the exec thread's spawn: nothing this process places can be filled before it.
    let spawn_ms = vike_model::now_ms();
    // Every `emit(..)` below discards a `CoreGone`: the core has exited, so the process is shutting
    // down and there is nothing left to deliver to (pre-start and lost state is reconcile's job on
    // the next boot). The FIRST such drop is announced, once for this loop ([`send_or_warn_once`]).
    let warned = AtomicBool::new(false);
    let actor = thread::current().name().unwrap_or("exec-loop").to_string();
    let emit = |event: Event| send_or_warn_once(events, &warned, &actor, event);
    // The cancels the venue answered "already gone", each with when, until the sentinel settles them.
    // A command does not push the sentinel back while one waits, or a busy account would starve it.
    // Those answered while this thread waited for its grid (`crate::venue_adapter::await_properties`) are
    // taken over here, and the sentinel is armed for them as it would have been after their cancel.
    let mut gone: Vec<(String, Instant)> = take_gone_while_waiting();
    // `Some(deadline)` = a resync is armed for `deadline`; `None` = idle (block on the next command).
    let mut resync_due: Option<Instant> =
        (!gone.is_empty()).then(|| Instant::now() + fill_sentinel);
    // The orders this loop saw no answer for. A fenced client sends its order's events itself and returns none, and
    // the fence keeps its own list (`VenueRest::accept_implied_by_replay`).
    let mut silent = SilentOrders::default();
    loop {
        if resync_due.is_some_and(|d| Instant::now() >= d) {
            // The restart law: a replayed row older than this thread's spawn is dropped,
            // whatever the venue closure returned. Counted, then reported ONCE per pass —
            // a background lane, never the core fold, so one aggregated line is bounded.
            let mut pre_spawn = 0usize;
            for event in resync() {
                if is_pre_spawn(&event, spawn_ms) {
                    pre_spawn += 1;
                    continue;
                }
                if let Some(order_client_id) = event.ended_order() {
                    gone.retain(|(c, _)| c != order_client_id);
                }
                // No venue's history mints an accept, so a silent order's fill brings its own.
                if let Some(accept) =
                    silent.accept_for(&event).or_else(|| rest.accept_implied_by_replay(&event))
                {
                    emit(accept);
                }
                emit(event);
            }
            if pre_spawn > 0 {
                tracing::info!(
                    target: "vike_bridge_core",
                    dropped = pre_spawn,
                    spawn_ms,
                    "history replay: dropped rows older than this process (restart law) — \
                     pre-start state is reconcile's job, not the gap-sentinel's"
                );
            }
            // An "already gone" the replay did not explain, past its time: what it always ended as.
            let now = Instant::now();
            gone.retain(|(order_client_id, at)| {
                if now.duration_since(*at) < fill_sentinel {
                    return true;
                }
                tracing::info!(
                    target: "vike_bridge_core",
                    order_client_id = %order_client_id,
                    "cancel: the venue said already gone and its history, read or not, names no end; reported cancelled"
                );
                emit(cancel_event(order_client_id, ExecutionClientActorCancelOutcome::Canceled));
                false
            });
            resync_due = (!gone.is_empty()).then(|| now + fill_sentinel);
        }
        let wait = match resync_due {
            Some(d) => d.saturating_duration_since(Instant::now()),
            None => Duration::from_secs(3600),
        };
        match rx.recv_timeout(wait) {
            Ok(ExecutionClientActorCommand::Submit(req, queued_at)) => {
                let answer = match too_old_to_send(&req, queued_at) {
                    Some(why) => rest.submit_refused(&req, why),
                    None => serving_submit(queued_at, || rest.submit_order(&req)),
                };
                if !answer.is_empty() && no_answer(&answer) {
                    silent.note(&req.order_client_id);
                }
                for event in answer {
                    emit(event);
                }
                resync_due = rearmed(resync_due, &gone, fill_sentinel);
            }
            // The REST venues meter nothing client-side, so the intent is read by nobody here.
            Ok(ExecutionClientActorCommand::Cancel { order_client_id, .. }) => {
                answer_cancel(rest, order_client_id, &mut gone, &emit);
                resync_due = rearmed(resync_due, &gone, fill_sentinel);
            }
            // The bulk lane, routed to [`VenueRest::cancel_batch`] — the SAME seam
            // `ExecutionClientRest::cancel_batch` uses, so a native batch endpoint is reached here too
            // and a venue without one gets that trait method's per-id fan-out. Its `Result` covers
            // the WHOLE batch, so a failure is reported per id exactly as `ExecutionClientRest` reports
            // it: over-reporting a non-terminal reject is safe (it changes no status),
            // under-reporting would be the silent vanish. Unreachable while no `run_loop` venue
            // calls `with_bulk_cancel`; written so a venue that declares one is right on arrival.
            Ok(ExecutionClientActorCommand::CancelBatch { order_client_ids, .. }) => {
                answer_cancel_batch(rest, &order_client_ids, &emit);
                resync_due = rearmed(resync_due, &gone, fill_sentinel);
            }
            Ok(ExecutionClientActorCommand::Modify {
                order,
                new_qty,
                new_price,
                new_trigger_price,
            }) => {
                // Native amend where the venue wires one (e.g. `[OrderModified]`); the `VenueRest`
                // default is a no-op (returns `[]`; the resting order keeps its terms). A trigger goes to
                // `modify_trigger`, whose default refuses it in words.
                let events = match new_trigger_price {
                    None => rest.modify_order(&order, new_qty, new_price),
                    Some(t) => rest.modify_trigger(&order, new_qty, new_price, t),
                };
                for event in events {
                    emit(event);
                }
                resync_due = rearmed(resync_due, &gone, fill_sentinel);
            }
            // No venue on this loop declares a bulk submit or a queued adopt, so both are mis-wirings.
            Ok(ExecutionClientActorCommand::SubmitBatch(requests, _)) => {
                submit_batch_undeclared(events, &requests)
            }
            Ok(ExecutionClientActorCommand::Adopt(order)) => adopt_undeclared(&order),
            Ok(ExecutionClientActorCommand::Shutdown) => break,
            // The top of the loop runs a sentinel that came due.
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => break,
        }
    }
    // Nothing more will settle these: what they always ended as, so no order is left waiting.
    for (order_client_id, _) in gone {
        emit(cancel_event(&order_client_id, ExecutionClientActorCancelOutcome::Canceled));
    }
}

/// **A `Cancel`'s answer**, the one mapping [`run_loop`] and `crate::venue_adapter::await_properties` (an exec
/// thread waiting for its grid, which sends a cancel all the same) share: success is a terminal `OrderCanceled`;
/// "already gone" ([`OrderCancelAck::AlreadyGone`]) the non-terminal `OrderCancelRejected`
/// ([`ALREADY_GONE_REASON`]) with the order client id kept in `gone` for the gap-sentinel; a failure the non-terminal
/// `OrderCancelRejected` in its words.
pub(crate) fn answer_cancel<R: VenueRest + ?Sized>(
    rest: &R,
    order_client_id: String,
    gone: &mut Vec<(String, Instant)>,
    emit: &impl Fn(Event),
) {
    match rest.cancel_order(&order_client_id) {
        Ok(OrderCancelAck::Canceled) => {
            emit(cancel_event(&order_client_id, ExecutionClientActorCancelOutcome::Canceled))
        }
        Ok(OrderCancelAck::AlreadyGone) => {
            let why = ExecutionClientActorCancelOutcome::Rejected(ALREADY_GONE_REASON.to_string());
            emit(cancel_event(&order_client_id, why));
            if !gone.iter().any(|(c, _)| *c == order_client_id) {
                gone.push((order_client_id, Instant::now()));
            }
        }
        Err(e) => {
            emit(cancel_event(&order_client_id, ExecutionClientActorCancelOutcome::Rejected(e.msg)))
        }
    }
}

/// A `CancelBatch`'s answer ([`answer_cancel`]'s twin): [`VenueRest::cancel_batch`]'s one whole-batch
/// `Result`, reported per id.
pub(crate) fn answer_cancel_batch<R: VenueRest + ?Sized>(
    rest: &R,
    order_client_ids: &[String],
    emit: &impl Fn(Event),
) {
    let result = rest.cancel_batch(order_client_ids);
    for order_client_id in order_client_ids {
        let outcome = match &result {
            Ok(()) => ExecutionClientActorCancelOutcome::Canceled,
            Err(e) => ExecutionClientActorCancelOutcome::Rejected(e.msg.clone()),
        };
        emit(cancel_event(order_client_id, outcome));
    }
}

thread_local! {
    /// The cancels answered "already gone" while this exec thread waited for its grid, handed to the
    /// [`run_loop`] it runs next ([`hand_over_gone_while_waiting`]).
    /// shortcut: per THREAD, as `SERVING_SINCE` is: right because the wait and the loop run on the one exec
    /// thread; an argument of `run_loop` is the upgrade if they ever do not.
    static GONE_WHILE_WAITING: std::cell::RefCell<Vec<(String, Instant)>> =
        const { std::cell::RefCell::new(Vec::new()) };
}

/// Hand the cancels answered "already gone" during the wait to this thread's next [`run_loop`].
pub(crate) fn hand_over_gone_while_waiting(gone: Vec<(String, Instant)>) {
    GONE_WHILE_WAITING.with_borrow_mut(|held| held.extend(gone));
}

fn take_gone_while_waiting() -> Vec<(String, Instant)> {
    GONE_WHILE_WAITING.with_borrow_mut(std::mem::take)
}

/// When the one-shot sentinel falls due after a command: `fill_sentinel` from now, unless a cancel
/// answered "already gone" is waiting for it, which keeps the time it already has.
fn rearmed(
    due: Option<Instant>,
    gone: &[(String, Instant)],
    fill_sentinel: Duration,
) -> Option<Instant> {
    due.filter(|_| !gone.is_empty()).or_else(|| Some(Instant::now() + fill_sentinel))
}

/// A venue's order-status re-query for [`ExecutionClientActor::with_confirm`]: maps an order-client-id to the
/// authoritative event(s) to emit, in the one form of [`crate::confirm`] (found is read by the order's state,
/// absent rejects only a dispatched order client id inside the venue's window, a failed query says nothing). Invoked
/// on the actor's OWN short-lived worker thread (blocking REST) so it NEVER runs on the
/// single-writer core fold. An empty `Vec` = nothing conclusive to report.
pub type ConfirmFn = Arc<dyn Fn(&str) -> Vec<Event> + Send + Sync>;

/// A venue exec client backed by a dedicated command thread (+ optional background threads). Owns
/// the channel, the thread handles, the [`ExecutionClient`] impl, and deterministic teardown. A venue
/// hands its own steps in as hooks (the module doc's "Venue hooks") and returns the actor itself; a
/// newtype that still wraps it forwards `ExecutionClient` to it by hand and needs no `Drop` (this one's
/// `Drop` joins everything).
pub struct ExecutionClientActor {
    tx: Sender<ExecutionClientActorCommand>,
    /// Clone of the venue's event lane — used to synthesize a terminal event when a command cannot be
    /// delivered (the `run` thread has exited) AND as the lane the [`ConfirmFn`] worker emits on. The
    /// happy submit/cancel path emits from inside `run`.
    events: EventSender,
    join: Option<JoinHandle<()>>,
    /// extra background threads: (stop flag, join handle) — flagged + joined on teardown.
    background: Vec<(Arc<AtomicBool>, JoinHandle<()>)>,
    /// Venue order-status re-query. `None` (default) = `confirm` is a clean no-op. `Some` =
    /// `confirm` runs it on a short-lived worker thread ([`Self::confirm_threads`]) and emits its
    /// result on `events`.
    confirm: Option<ConfirmFn>,
    /// In-flight one-shot confirm workers — pruned when finished + joined on teardown so a re-query
    /// never strands a thread at process exit (deterministic teardown, like `background`).
    confirm_threads: Vec<JoinHandle<()>>,
    /// The HALT sentinel this actor watches — handed in by the venue's start from
    /// `AdapterInputs::process.halt_path` ([`Self::with_halt_path`]), never resolved here.
    ///
    /// `None` (a test double, a tool) watches NO file — never the process-wide resolution
    /// (`vike_boot::halt::halt_path_from_env`), because a bridge reads no process-global state (decision
    /// 0099). An actor with no path says so once, at `error` (`vike_execution::halt::sentinel_engaged`), because a
    /// start that forgot to wire it is a dead kill switch.
    halt_path: Option<PathBuf>,
    /// The venue's step on every order before this actor reads it ([`Self::with_before_submit`]).
    /// `None` = the order goes as the caller gave it.
    before_submit: Option<BeforeSubmitFn>,
    /// What `adopt_restored` teaches the venue's tables ([`Self::with_adopt_restored`]). `None` = the
    /// trait's no-op.
    adopt_restored: Option<AdoptRestoredFn>,
    /// The venue's form of an order client id on its reports ([`Self::with_wire_id_for`]). `None` = the trait's
    /// answer, `None` for every order client id (the venue echoes it).
    wire_id_for: Option<WireIdForFn>,
    /// Does this venue's command loop handle [`ExecutionClientActorCommand::CancelBatch`]? `false` (the default)
    /// means `cancel_batch_with_intent` fans the ids out into per-id [`ExecutionClientActorCommand::Cancel`]s
    /// BEFORE the channel. Set by [`Self::with_bulk_cancel`].
    bulk_cancel: bool,
    /// Does this venue's command loop read [`ExecutionClientActorCommand::Modify`]'s `new_trigger_price`? `false` (the
    /// default) means `modify_trigger` refuses before the channel. Set by [`Self::with_trigger_amend`].
    trigger_amend: bool,
    /// Does this venue's command loop handle [`ExecutionClientActorCommand::SubmitBatch`]? `false` (the default) means
    /// `submit_batch` fans out into per-order [`ExecutionClientActorCommand::Submit`]s. Set by [`Self::with_bulk_submit`].
    bulk_submit: bool,
    /// Does this venue's command loop read [`ExecutionClientActorCommand::Adopt`]? `false` (the default) means `adopt_restored`
    /// queues nothing. Set by [`Self::with_adopt_queued`].
    adopt_queued: bool,
    /// The thread name this actor was spawned under -- the `actor` field of the one-time
    /// core-gone warning ([`Self::emit`]).
    name: String,
    /// Raised the first time an event this actor sent found the core gone, so the warning is ONE
    /// line per actor and never one per dropped message. `Arc` because the [`ConfirmFn`] worker
    /// thread shares it.
    core_gone_warned: Arc<AtomicBool>,
}

impl ExecutionClientActor {
    /// Spawn the command thread. `run` MUST return on `ExecutionClientActorCommand::Shutdown` (or when the channel
    /// closes) so teardown is deterministic. `events` is a clone of the lane `run` pushes into; the
    /// actor keeps it to synthesize a terminal event if a command lands on a dead (closed) channel.
    pub fn spawn<F>(name: &str, events: EventSender, run: F) -> Self
    where
        F: FnOnce(Receiver<ExecutionClientActorCommand>) + Send + 'static,
    {
        // ⚠ No HALT path is resolved here: a spawn is a bridge's act, and the process-wide resolver
        // (and its unarmable-switch report) is the composition layer's — `vike-tradehub-core` resolves it
        // before the first start (`ProcessPaths::halt_path`).
        let (tx, rx) = channel();
        let pin_label = name.to_string();
        let join = thread::Builder::new()
            .name(name.to_string())
            .spawn(move || {
                // Opt-in HFT pinning (VIKE_PIN_CORES=exec:N): keep the venue exec command thread on
                // a fixed core. No-op unless the env names the `exec` role — the default path.
                vike_execution::affinity::pin_current_thread(
                    vike_execution::affinity::Role::Execution,
                    &pin_label,
                );
                run(rx)
            })
            .expect("spawn exec thread");
        Self {
            tx,
            events,
            join: Some(join),
            background: Vec::new(),
            confirm: None,
            confirm_threads: Vec::new(),
            halt_path: None,
            before_submit: None,
            adopt_restored: None,
            wire_id_for: None,
            bulk_cancel: false,
            trigger_amend: false,
            bulk_submit: false,
            adopt_queued: false,
            name: name.to_string(),
            core_gone_warned: Arc::new(AtomicBool::new(false)),
        }
    }

    /// Attach a background thread (e.g. a fills stream) that is stopped via `flag` then joined on
    /// teardown. The thread MUST observe `flag` and return promptly.
    #[must_use]
    pub fn with_background(mut self, flag: Arc<AtomicBool>, join: JoinHandle<()>) -> Self {
        self.background.push((flag, join));
        self
    }

    /// Wire the venue's order-status re-query so [`ExecutionClient::confirm`] can ACTIVELY confirm a
    /// stuck order: the closure re-runs the venue's EXISTING re-query (the one behind
    /// `resolve_ambiguous_submit`) and returns the authoritative event(s) to emit. The actor runs it
    /// on its OWN short-lived worker thread (blocking REST) and pushes the result onto the venue
    /// event lane — so the core's watchdog can prod a wedged adapter with zero network on the fold.
    /// Without this call `confirm` is a no-op and only the watchdog's last-resort reject backstops
    /// the order.
    #[must_use]
    pub fn with_confirm(mut self, confirm: ConfirmFn) -> Self {
        self.confirm = Some(confirm);
        self
    }

    /// Run `hook` on every order before this actor reads it ([`BeforeSubmitFn`]): ahead of the HALT
    /// check and of the queue, so the order the actor rejects or queues is the one the hook left. A batch
    /// is the trait's per-order fan-out onto `submit`, so each of its orders passes it too.
    #[must_use]
    pub fn with_before_submit(mut self, hook: BeforeSubmitFn) -> Self {
        self.before_submit = Some(hook);
        self
    }

    /// Answer [`ExecutionClient::adopt_restored`] with `hook` ([`AdoptRestoredFn`]): the venue's tables
    /// learn the orders a previous session left resting. Not halt-gated: it places nothing.
    #[must_use]
    pub fn with_adopt_restored(mut self, hook: AdoptRestoredFn) -> Self {
        self.adopt_restored = Some(hook);
        self
    }

    /// Answer [`ExecutionClient::wire_id_for`] with `hook` ([`WireIdForFn`]), for a venue whose reports
    /// name an order by a pure function of its order client id.
    #[must_use]
    pub fn with_wire_id_for(mut self, hook: WireIdForFn) -> Self {
        self.wire_id_for = Some(hook);
        self
    }

    /// Declare that this venue's command loop handles [`ExecutionClientActorCommand::CancelBatch`], so a
    /// multi-order cancel reaches the venue as ONE command instead of `n` shredded singles.
    ///
    /// The payoff is LATENCY (one round trip instead of `n` on an emergency flatten), paid in rate
    /// allowance (Polymarket's `DELETE /cancel-all` debits `1 + n` where `n` cancels debit `n`).
    /// Deciding that trade is the VENUE's job: the batch arrives whole, with its [`OrderCancelIntent`],
    /// and the venue's own planner picks bulk or targeted. Not calling this leaves the fan-out: `n`
    /// per-id [`ExecutionClientActorCommand::Cancel`]s in the caller's order, each carrying the intent.
    ///
    /// ⚠ **Declaring it obliges the loop to write a real `CancelBatch` arm.** A declaration whose
    /// loop falls through to [`cancel_batch_undeclared`] refuses every id NON-terminally (nothing
    /// vanishes, but nothing cancels either) — which is a liveness bug wearing a loud `error!`, not
    /// a silent one.
    #[must_use]
    pub fn with_bulk_cancel(mut self) -> Self {
        self.bulk_cancel = true;
        self
    }

    /// Declare that this venue's command loop reads [`ExecutionClientActorCommand::Modify`]'s `new_trigger_price`, so
    /// [`ExecutionClient::modify_trigger`] queues the trigger amend instead of refusing it. The shared
    /// [`run_loop`] reads it (and hands it to `VenueRest::modify_trigger`); a loop of the venue's own
    /// reads it in its `Modify` arm. Not calling this keeps the trait's refusal in words
    /// (`vike_execution::execution_engine::TRIGGER_AMEND_UNSUPPORTED_REASON`), so a loop that matches the field
    /// away can never be handed one: the same interlock as [`Self::with_bulk_cancel`].
    #[must_use]
    pub fn with_trigger_amend(mut self) -> Self {
        self.trigger_amend = true;
        self
    }

    /// Declare that this venue's command loop handles [`ExecutionClientActorCommand::SubmitBatch`], so a multi-order submit reaches
    /// the venue as ONE command (hyperliquid: one signed `/exchange` order action with N items). Not calling this
    /// keeps the trait's fan-out: `n` [`ExecutionClientActorCommand::Submit`]s, each judged by the HALT boundary on its own.
    ///
    /// ⚠ A declared batch is judged by the HALT boundary as a UNIT: sent whole or refused whole, so a batch mixing
    /// reducing and opening orders is refused under a halt (one terminal `OrderRejected` per order) rather than split
    /// into a request the caller never built. Declaring it obliges the loop to write a real `SubmitBatch` arm that
    /// asks [`too_old_to_send`] of each order.
    #[must_use]
    pub fn with_bulk_submit(mut self) -> Self {
        self.bulk_submit = true;
        self
    }

    /// Declare that this venue's command loop reads [`ExecutionClientActorCommand::Adopt`], so `adopt_restored` queues one per
    /// restored order AFTER the [`Self::with_adopt_restored`] hook ran: the hook teaches, on the caller's thread,
    /// what must be known at once (hyperliquid's cloid map, which a stream frame reads); the command teaches the exec
    /// thread's own state (hyperliquid's asset per order), in order with the cancels behind it.
    #[must_use]
    pub fn with_adopt_queued(mut self) -> Self {
        self.adopt_queued = true;
        self
    }

    /// Hand this actor the HALT sentinel it watches: the venue's start passes
    /// `AdapterInputs::process.halt_path`, the one file `vike-tradehub-core` resolved for the whole process.
    /// Without it the actor watches nothing (see the field's doc); a test passes a path of its own,
    /// so it exercises the boundary without mutating the process environment.
    #[must_use]
    pub fn with_halt_path(mut self, path: PathBuf) -> Self {
        self.halt_path = Some(path);
        self
    }

    /// Deliver one synthesized event on the venue's lane. A closed lane is `CoreGone`: the core has
    /// exited, so the process is shutting down and there is nothing to deliver to -- announced ONCE
    /// per actor ([`send_or_warn_once`]), never per message.
    fn emit(&self, event: Event) {
        send_or_warn_once(&self.events, &self.core_gone_warned, &self.name, event);
    }

    /// True when the HALT sentinel file currently exists → new order placement must be refused.
    /// Consulted only at the submit/modify boundary (submits are infrequent) — never a hot path.
    fn halt_engaged(&self) -> bool {
        vike_execution::halt::sentinel_engaged(self.halt_path.as_deref())
    }

    /// Signal every thread this actor owns to wind down, and JOIN NOTHING.
    ///
    /// The first half of [`Self::stop`], so a caller holding SEVERAL actors can raise all their
    /// flags before it joins the first — it backs [`ExecutionClient::begin_detach`]. `stop` calls
    /// it too, so the signalling has one copy.
    ///
    /// **IDEMPOTENT by construction, which the seam requires**: the `Shutdown` send is `let _ =` (a
    /// command thread that has exited makes it an ignored error) and a `bool` flag store is a store
    /// either way. So `begin_detach` → `detach` re-signals harmlessly.
    ///
    /// ⚠ `Shutdown` rides the SAME command channel as submits and cancels, so anything already
    /// queued — the shutdown-policy cancel sweep above all — is still drained ahead of it. Raising
    /// the flag early does not cut a cancel that was already sent.
    fn signal_stop(&mut self) {
        // Command thread already exited = already stopping; nothing to deliver to.
        let _ = self.tx.send(ExecutionClientActorCommand::Shutdown);
        for (flag, _) in &self.background {
            flag.store(true, Ordering::Relaxed);
        }
    }

    fn stop(&mut self) {
        // signal everything first, THEN join, so the command + background threads wind down together.
        self.signal_stop();
        if let Some(j) = self.join.take() {
            // A `join` Err is a panic in an already-dead thread: nothing to recover at teardown.
            let _ = j.join();
        }
        for (_, j) in self.background.drain(..) {
            let _ = j.join();
        }
        // Join any in-flight confirm workers (one-shot; each is bounded by the short-timeout requery
        // transport) so a re-query in flight never strands a thread at process exit.
        for j in self.confirm_threads.drain(..) {
            let _ = j.join();
        }
    }
}

impl ExecutionClient for ExecutionClientActor {
    fn submit(&mut self, request: &OrderRequest) {
        // The venue's own step comes first, so the HALT reject below and the queued order both carry what it
        // wrote. Copied only when there is a hook; the queue takes its own copy either way.
        let mut request = Cow::Borrowed(request);
        if let Some(hook) = &self.before_submit {
            hook(request.to_mut());
        }
        // HALT kill-switch: while the sentinel file exists, refuse orders that OPEN risk. Synthesize
        // the terminal OrderRejected the contract requires (the order never reaches the venue, but
        // the intent must NOT silently vanish), then return without touching the command channel.
        // Checked here at the submit boundary only — never on a per-message hot path.
        //
        // ⚠ A REDUCING SUBMIT IS ADMITTED (`vike_execution::halt::halt_admits_submit`, which owns that
        // rule for this adapter AND for hyperliquid's bespoke copy). A cancel passing through is
        // not enough for "a kill switch must never trap you in a position": a cancel removes a
        // resting ORDER, while getting OUT of a position means SENDING one
        // (`OrderIntent::Flatten`'s `reduce_only` MARKET leg — refused here, `market-exit` under a
        // HALT file could close nothing).
        //
        // `halt_engaged()` is evaluated ONCE and reused: it is a filesystem `exists()`, and
        // rearranging these arms must not turn one syscall per submit into two.
        if self.halt_engaged() {
            if !vike_execution::halt::halt_admits_submit(&request) {
                // `CoreGone` = shutting down; nothing to deliver to.
                self.emit(Event::OrderRejected(OrderRejected {
                    cause: None,
                    order_client_id: request.order_client_id.clone(),
                    reason: vike_execution::halt::HALT_REJECT_REASON.into(),
                    timestamp: request.timestamp,
                }));
                return;
            }
            warn_halt_admit(&request);
        }
        let queued =
            ExecutionClientActorCommand::Submit(Box::new(request.into_owned()), Instant::now());
        if let Err(SendError(ExecutionClientActorCommand::Submit(request, _))) =
            self.tx.send(queued)
        {
            // Dead venue thread (login failure / panic): the order never reached the venue, so
            // synthesize the terminal OrderRejected the contract requires — never let it vanish.
            // (If even this send fails the core is gone = shutting down; nothing to deliver to.)
            self.emit(Event::OrderRejected(OrderRejected {
                cause: None,
                order_client_id: request.order_client_id.clone(),
                reason: "venue exec thread unavailable".to_string().into(),
                timestamp: request.timestamp,
            }));
        }
    }
    /// ONE `SubmitBatch` command for a venue that DECLARED a bulk lane ([`Self::with_bulk_submit`]); for every
    /// other venue the trait's per-order fan-out onto `submit`. Each order passes the venue's [`BeforeSubmitFn`]
    /// either way.
    fn submit_batch(&mut self, requests: &[OrderRequest]) {
        if !self.bulk_submit {
            for request in requests {
                self.submit(request);
            }
            return;
        }
        if requests.is_empty() {
            return;
        }
        let mut requests = requests.to_vec();
        if let Some(hook) = &self.before_submit {
            requests.iter_mut().for_each(|request| hook(request));
        }
        // ONE venue request, refused as a unit: every refusal carries the batch's first `ts`.
        let ts = requests[0].timestamp;
        // ALL-OR-NOTHING under a HALT, stricter than the per-order door on purpose: a batch mixing reducing and
        // opening legs cannot be "partly admitted" without sending a request the caller did not build, so EVERY
        // leg must reduce (`vike_execution::halt::halt_admits_submit`), or each order is refused terminally.
        if self.halt_engaged() {
            if !requests.iter().all(vike_execution::halt::halt_admits_submit) {
                for request in &requests {
                    self.emit(Event::OrderRejected(OrderRejected {
                        cause: None,
                        order_client_id: request.order_client_id.clone(),
                        reason: vike_execution::halt::HALT_REJECT_REASON.into(),
                        timestamp: ts,
                    }));
                }
                return;
            }
            requests.iter().for_each(warn_halt_admit);
        }
        let queued = ExecutionClientActorCommand::SubmitBatch(requests, Instant::now());
        if let Err(SendError(ExecutionClientActorCommand::SubmitBatch(requests, _))) =
            self.tx.send(queued)
        {
            // Dead venue thread: no order reached the venue, so each gets the terminal reject `submit` gives.
            for request in &requests {
                self.emit(Event::OrderRejected(OrderRejected {
                    cause: None,
                    order_client_id: request.order_client_id.clone(),
                    reason: "venue exec thread unavailable".to_string().into(),
                    timestamp: ts,
                }));
            }
        }
    }
    fn cancel(&mut self, order_client_id: &str) {
        // ONE cancel door, so the two can never disagree (the `cancel_with_intent` contract).
        self.cancel_with_intent(order_client_id, OrderCancelIntent::Unspecified);
    }
    fn cancel_with_intent(&mut self, order_client_id: &str, intent: OrderCancelIntent) {
        // Deliberately NOT halt-gated: a kill switch must let an operator reduce/exit, never trap a
        // position.
        if self
            .tx
            .send(ExecutionClientActorCommand::Cancel {
                order_client_id: order_client_id.to_string(),
                intent,
            })
            .is_err()
        {
            // Dead venue thread: the cancel could not be delivered. Non-terminal advisory — the
            // order (if live) stays live; surface the failure rather than swallow it.
            // (If even this send fails the core is gone = shutting down; nothing to deliver to.)
            self.emit(Event::OrderCancelRejected(OrderCancelRejected {
                order_client_id: order_client_id.to_string(),
                reason: "venue exec thread unavailable".to_string().into(),
                timestamp: 0,
            }));
        }
    }
    /// ONE `CancelBatch` command for a venue that DECLARED a bulk lane
    /// ([`Self::with_bulk_cancel`]); for every other venue the per-id fan-out — `n` queued
    /// `Cancel`s in the caller's order, each naming why it was issued.
    fn cancel_batch_with_intent(&mut self, order_client_ids: &[String], intent: OrderCancelIntent) {
        if !self.bulk_cancel {
            for c in order_client_ids {
                self.cancel_with_intent(c, intent);
            }
            return;
        }
        // Deliberately NOT halt-gated, like the single-cancel door.
        let cmd = ExecutionClientActorCommand::CancelBatch {
            order_client_ids: order_client_ids.to_vec(),
            intent,
        };
        if self.tx.send(cmd).is_err() {
            // Dead venue thread: the batch could not be delivered. Reported PER ID, exactly as the
            // fan-out would have — non-terminal, so every order (if live) stays live, and one
            // undeliverable batch never looks different downstream from `n` undeliverable singles.
            for c in order_client_ids {
                // `CoreGone` = shutting down; nothing to deliver to.
                self.emit(Event::OrderCancelRejected(OrderCancelRejected {
                    order_client_id: c.clone(),
                    reason: "venue exec thread unavailable".to_string().into(),
                    timestamp: 0,
                }));
            }
        }
    }
    fn modify(&mut self, order: &OrderRequest, new_qty: Option<f64>, new_price: Option<f64>) {
        // HALT kill-switch (conservative): a modify can add size / chase price and "reduce-only"
        // can't be reliably inferred here, so halt blocks it — the exit path under halt is cancel
        // (always allowed), never modify.
        //
        // ⚠ The block must be ANNOUNCED: a silent `return` makes a blocked modify indistinguishable
        // from a LOST one (no event, resting order at its old terms), so an operator could not tell
        // a working kill switch from a wedged adapter. The advisory is NON-TERMINAL
        // (`vike_model::OrderRecord`'s FSM maps `OrderModifyRejected` back to MODIFIABLE), so the resting
        // order keeps its terms. cTrader emits the same event with the same reason — the shared
        // wording lets one recogniser match every client.
        if self.halt_engaged() {
            // `CoreGone` = shutting down; nothing to deliver to.
            self.emit(Event::OrderModifyRejected(OrderModifyRejected {
                order_client_id: order.order_client_id.clone(),
                reason: vike_execution::halt::HALT_REJECT_REASON.into(),
                timestamp: crate::rest::amend_answer_time(None),
            }));
            return;
        }
        // Non-terminal: a dead channel just means the resting order keeps its current terms (the
        // ExecutionClient::modify default contract) — nothing to synthesize, unlike submit/cancel.
        let _ = self.tx.send(ExecutionClientActorCommand::Modify {
            order: Box::new(order.clone()),
            new_qty,
            new_price,
            new_trigger_price: None,
        });
    }
    /// Queued only on a loop that DECLARED it reads the trigger ([`Self::with_trigger_amend`]); every
    /// other actor refuses it in the trait's words. HALT-gated and announced exactly as `modify`.
    fn modify_trigger(
        &mut self,
        order: &OrderRequest,
        new_qty: Option<f64>,
        new_price: Option<f64>,
        new_trigger_price: f64,
    ) -> Result<(), &'static str> {
        if !self.trigger_amend {
            return Err(vike_execution::execution_engine::TRIGGER_AMEND_UNSUPPORTED_REASON);
        }
        if self.halt_engaged() {
            self.emit(Event::OrderModifyRejected(OrderModifyRejected {
                order_client_id: order.order_client_id.clone(),
                reason: vike_execution::halt::HALT_REJECT_REASON.into(),
                timestamp: crate::rest::amend_answer_time(None),
            }));
            return Ok(());
        }
        let _ = self.tx.send(ExecutionClientActorCommand::Modify {
            order: Box::new(order.clone()),
            new_qty,
            new_price,
            new_trigger_price: Some(new_trigger_price),
        });
        Ok(())
    }
    fn confirm(&mut self, order_client_id: &str) {
        // Prune confirm workers that already finished, so the tracking Vec stays bounded across
        // repeated confirms (each is one-shot: one bounded re-query + emit, then it returns).
        self.confirm_threads.retain(|h| !h.is_finished());
        // No re-query wired → clean no-op; the watchdog's stage-2 last-resort reject still
        // backstops the order. NOT gated on HALT: a read-only status confirm places no order, and
        // observing venue truth must never be blocked.
        let Some(confirm) = self.confirm.clone() else {
            return;
        };
        let events = self.events.clone();
        let warned = Arc::clone(&self.core_gone_warned);
        let actor = self.name.clone();
        let order_client_id = order_client_id.to_string();
        // Run the re-query on a SHORT-LIVED worker thread — NEVER on the caller (the single-writer
        // core fold): the closure does blocking REST. It emits the venue's authoritative terminal on
        // the same lossless ingest lane every other venue event rides, so the core just folds it. If
        // the spawn itself fails (OS thread exhaustion), the watchdog's stage-2 reject still covers it.
        if let Ok(handle) =
            thread::Builder::new().name("exec-confirm".to_string()).spawn(move || {
                for event in confirm(&order_client_id) {
                    // `CoreGone` = shutting down; nothing to deliver to.
                    send_or_warn_once(&events, &warned, &actor, event);
                }
            })
        {
            self.confirm_threads.push(handle);
        }
    }
    /// Raise the stop flags, join nothing — the trait's phase-one seam, wired here because this is
    /// the shared home most bridges reach `detach` through: their start hands out this actor itself.
    /// Which bridges override the seam (those, plus the ones that own their own
    /// threads) is answered by a command, not a count, because a count rots the day a bridge is
    /// added: the roster is `git grep -l 'fn begin_detach' -- crates/bridges`.
    ///
    /// ⚠ **A wrapper that does not ALSO delegate this inherits the trait's no-op** and silently
    /// keeps the serial cost — the shadowing hazard the `Box<dyn ExecutionClient + Send>` impl's doc
    /// in `crates/vike-execution/src/execution_engine/client.rs` describes. Nothing fails when a wrapper
    /// forgets; it just pays for it at shutdown.
    fn begin_detach(&mut self) {
        self.signal_stop();
    }
    fn detach(&mut self) {
        self.stop();
    }
    /// The venue's [`AdoptRestoredFn`] on the caller's thread, then, for a loop that declared it
    /// ([`Self::with_adopt_queued`]), one [`ExecutionClientActorCommand::Adopt`] per order; with neither, the trait's no-op.
    fn adopt_restored(&mut self, orders: &[RestoredOrder]) {
        if let Some(adopt) = &self.adopt_restored {
            adopt(orders);
        }
        if self.adopt_queued {
            for order in orders {
                // Dead venue thread: there is no state left to teach, and an adopt places nothing.
                let _ = self.tx.send(ExecutionClientActorCommand::Adopt(order.clone()));
            }
        }
    }
    /// The venue's [`WireIdForFn`]; with none, the trait's `None` (the venue echoes the order client id).
    fn wire_id_for(&self, order_client_id: &str) -> Option<String> {
        self.wire_id_for.as_ref().and_then(|wire_id_for| wire_id_for(order_client_id))
    }
}

/// **The HALT admit line** (owner ruling F-6): what the submit boundary writes when a HALT lets a `reduce_only`
/// order through, the one line `docs/ops/kill-switches.md` points an operator at. NEVER SILENT: this boundary can
/// only check the caller-asserted flag (it holds no position book — see `halt.rs`), so an order leaving while the
/// operator believes the switch is engaged must be findable in the log. Per order and halt-only: never a hot path.
fn warn_halt_admit(request: &OrderRequest) {
    tracing::warn!(
        target: "vike_bridge_core::halt",
        order_client_id = %request.order_client_id,
        symbol = %request.symbol,
        qty = request.qty,
        "HALT engaged: admitting a reduce_only submit so the operator can still get out. \
         This boundary trusts the flag — it cannot verify the order against a position."
    );
}

impl Drop for ExecutionClientActor {
    fn drop(&mut self) {
        self.stop();
    }
}

#[cfg(test)]
mod run_loop_tests;

#[cfg(test)]
mod cancel_event_tests;

#[cfg(test)]
mod hooks_tests;

#[cfg(test)]
mod bulk_submit_tests;
