//! The exec-thread scaffold shared by the command-driven venue `ExecutionClient`s.
//!
//! [`ExecActor`] owns an mpsc channel, a dedicated OS thread draining [`ExecCommand`]s, the
//! [`ExecutionClient`] impl, and a `Drop`/`detach` that sends `Shutdown` and JOINS the thread —
//! deterministic teardown matters (a stranded feed thread is a 0xC0000409 on process exit). A venue
//! supplies only its command loop `run(Receiver<ExecCommand>)`; extra background threads (e.g.
//! OANDA's fills stream) attach via [`ExecActor::with_background`] and are flag-stopped and joined
//! on teardown too. For the REST-driven crypto venues the command loop itself is shared:
//! [`run_loop`] (command→REST→ingest plus the gap-sentinel, generic over [`VenueRest`]).
//!
//! ## HALT kill-switch (submit boundary)
//! `submit` (and `modify`) consult the [`crate::halt`] file sentinel BEFORE touching the command
//! channel: while the HALT file exists, a submit is refused with a synthesized terminal
//! `OrderRejected` (never a silent vanish — the venue-adapter contract) and a modify is refused with
//! a NON-terminal `OrderModifyRejected` (the resting order keeps its terms). Cancel is deliberately
//! never gated — halt must let an operator reduce/exit, never trap a position. This works even when
//! the runtime is wedged: an operator `touch`es the file over ssh and the next submit stops.
//!
//! ## Bulk cancel (opt-in, per venue)
//! [`ExecCommand::CancelBatch`] carries a multi-order cancel INTACT, and is delivered ONLY to a
//! venue that declared it handles one ([`ExecActor::with_bulk_cancel`]); every other venue gets `n`
//! per-id [`ExecCommand::Cancel`]s. Opt-in because the trade is not free: Polymarket (the only
//! declaration) buys one round trip instead of `n` on an emergency flatten, but its `cancel-all`
//! debits `1 + n` rate tokens where `n` singles debit `n` — a decision only the venue's own budget
//! can make.
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
//! started is reconciliation's job (`vike_exec::recon::resolve`: policy, quarantine, operator
//! confirm).
//!
//! `bybit`'s `BybitFundingPoller` (`crates/bridges/bybit/src/funding.rs`) guards the same hazard
//! the same way, with two differences: its floor is a PARAMETER (`0` = no floor, for tests/probes)
//! while this one is sampled INSIDE [`run_loop`] (no legitimate caller wants it off); and funding
//! can also narrow the request with `startTime`, while these history endpoints are row-count-bounded
//! — so here the client-side timestamp check IS the guarantee.
//!
//! **What the floor does NOT touch.** A mid-process WS reconnect still delivers every fill from the
//! gap (stamped AFTER spawn). An UNSTAMPED event (`ts == 0`) also rides through: a missing
//! timestamp is not evidence of being historical, and no timestamp is ever fabricated here.

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, RecvTimeoutError, Sender, channel};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use vike_exec::{CancelIntent, EventSender, ExecutionClient};
use vike_model::OrderRequest;
use vike_model::events::{Event, OrderCancelRejected, OrderModifyRejected, OrderRejected};

use crate::error_kind::{
    ExecErrorPolicy, SubmitAttempt, SubmitDisposition, SubmitRetryBudget, VenueTaxonomy,
    classify_venue, reject_reason, retry_exhausted_reason, submit_disposition,
};
use crate::halt;
use crate::rest::VenueRest;
use crate::transport::VenueApiError;

/// A command processed by a venue exec thread.
pub enum ExecCommand {
    Submit(Box<OrderRequest>),
    /// Cancel one resting order. Carries the core's [`CancelIntent`] so a venue that meters its
    /// cancels decides (and debits) on ITS OWN thread, where the budget lives — deciding on the
    /// caller's thread would let a burst of routine cancels each pass against a balance none of them
    /// had spent yet. A venue with no budget ignores the field; `Unspecified` is the flatten-safe
    /// default.
    Cancel {
        client_order_id: String,
        intent: CancelIntent,
    },
    /// Cancel MANY resting orders as ONE command, so a venue with a native bulk/mass-cancel
    /// endpoint sees the batch INTACT. Carries the core's [`CancelIntent`] for the same reason
    /// [`ExecCommand::Cancel`] does, and carries it ONCE, because the core classifies a batch as a
    /// whole (`vike_exec::ExecutionEngine::mass_cancel_with_intent`), never per id.
    ///
    /// ⚠ **Delivered ONLY to a venue that DECLARED it handles one** —
    /// [`ExecActor::with_bulk_cancel`].
    /// Without that declaration [`ExecutionClient::cancel_batch_with_intent`] fans the ids out into
    /// per-id [`ExecCommand::Cancel`]s before they reach the channel (`n` singles, in the caller's
    /// order). That opt-in IS the safety interlock: a venue cannot be handed a batch shape it never
    /// wrote code for, so the arm every loop must still write can be the one shared, unreachable
    /// fallback [`cancel_batch_undeclared`].
    CancelBatch {
        client_order_ids: Vec<String>,
        intent: CancelIntent,
    },
    /// Reprice/resize a resting order in place (DOM drag-to-modify). Carries the whole resting order
    /// so a venue can read its side/current terms; venues without a native amend leave it a no-op
    /// (the order keeps its terms — nothing vanishes).
    Modify {
        order: Box<OrderRequest>,
        new_qty: Option<f64>,
        new_price: Option<f64>,
    },
    Shutdown,
}

/// The outcome of a venue cancel attempt, mapped to a canonical event by [`cancel_event`].
pub enum CancelOutcome {
    /// The venue confirmed the cancel — terminal `OrderCanceled`.
    Canceled,
    /// The cancel could not be delivered or confirmed (venue error, unknown/already-gone order,
    /// ambiguous ack). NON-terminal `OrderCancelRejected` — the order, if still live, stays live.
    Rejected(String),
}

/// Map a cancel attempt to its canonical event so a failed cancel is NEVER swallowed: success →
/// terminal `OrderCanceled`, any failure → non-terminal `OrderCancelRejected`. Shared by every
/// command-driven venue's `Cancel` branch.
pub fn cancel_event(client_order_id: &str, outcome: CancelOutcome) -> Event {
    match outcome {
        CancelOutcome::Canceled => Event::OrderCanceled(vike_model::events::OrderCanceled {
            client_order_id: client_order_id.to_string(),
            reason: String::new().into(),
            ts: 0,
        }),
        CancelOutcome::Rejected(reason) => Event::OrderCancelRejected(OrderCancelRejected {
            client_order_id: client_order_id.to_string(),
            reason: reason.into(),
            ts: 0,
        }),
    }
}

/// The reason [`cancel_batch_undeclared`] puts on every id it refuses. A constant so the message a
/// mis-wired venue produces is greppable from the one place that can produce it.
pub const CANCEL_BATCH_UNDECLARED_REASON: &str =
    "venue declared no bulk-cancel path (ExecActor::with_bulk_cancel)";

/// The [`ExecCommand::CancelBatch`] arm for a venue command loop that has NO bulk-cancel path.
///
/// **Unreachable by construction**: [`ExecActor`] only puts a `CancelBatch` on the channel for a
/// venue that called [`ExecActor::with_bulk_cancel`], so an undeclared loop's batches were already
/// fanned out into per-id [`ExecCommand::Cancel`]s. The arm still has to be WRITTEN (the enum is
/// matched exhaustively), and the tempting empty arm (`=> {}`) would turn a future mis-wiring into
/// `n` orders vanishing with no event — the one thing the emitter-split contract forbids.
///
/// So it refuses every id NON-terminally: the orders are still resting at the venue, the caller
/// re-offers them, and the `error!` names the missing declaration.
pub fn cancel_batch_undeclared(events: &EventSender, client_order_ids: &[String]) {
    tracing::error!(
        target: "vike_bridge_core",
        count = client_order_ids.len(),
        "a CancelBatch reached a venue loop with no bulk-cancel path — every id refused \
         NON-terminally and every order left resting. Either wire the loop's CancelBatch arm or \
         drop its `with_bulk_cancel` declaration."
    );
    for coid in client_order_ids {
        // `CoreGone` = the core has exited = shutting down; nothing to deliver to.
        let _ = events.blocking_send(Event::OrderCancelRejected(OrderCancelRejected {
            client_order_id: coid.clone(),
            reason: CANCEL_BATCH_UNDECLARED_REASON.to_string().into(),
            ts: 0,
        }));
    }
}

/// Send `ev` on `events`; the FIRST time the lane is found closed (`CoreGone`: the core thread has
/// exited) raise `warned` and log ONE `warn!` naming the `actor`. Every later drop through the same
/// flag is silent: this runs on the exec thread and the submit/cancel boundary, and a core that is
/// gone stays gone, so one line per actor is the whole signal and a line per message would be a
/// flood. The hot (delivered) path costs nothing extra: the flag is touched only on failure.
fn send_or_warn_once(events: &EventSender, warned: &AtomicBool, actor: &str, ev: Event) {
    if events.blocking_send(ev).is_err() && !warned.swap(true, Ordering::Relaxed) {
        tracing::warn!(
            target: "vike_bridge_core",
            actor,
            "the core has exited: this actor's events are no longer delivered (logged once per actor, later \
             drops are silent)"
        );
    }
}

/// Every [`Event`]'s wall-clock stamp in epoch milliseconds, by an EXHAUSTIVE match — a new
/// `Event` variant is a COMPILE error here rather than a silent hole in [`run_loop`]'s history
/// floor (the restart law; see the module doc). `0` means the emitter left it unstamped, which
/// `is_pre_spawn` treats as "not evidence of being historical".
pub fn event_ts(ev: &Event) -> i64 {
    match ev {
        Event::Fill(e) => e.ts,
        Event::OrderSubmitted(e) => e.ts,
        Event::OrderAccepted(e) => e.ts,
        Event::OrderRejected(e) => e.ts,
        Event::OrderDenied(e) => e.ts,
        Event::OrderTriggered(e) => e.ts,
        Event::OrderPartiallyFilled(e) => e.ts,
        Event::OrderFilled(e) => e.ts,
        Event::OrderCanceled(e) => e.ts,
        Event::OrderExpired(e) => e.ts,
        Event::OrderLiquidated(e) => e.ts,
        Event::OrderModified(e) => e.ts,
        Event::PositionOpened(e) => e.ts,
        Event::PositionChanged(e) => e.ts,
        Event::PositionClosed(e) => e.ts,
        Event::AccountState(e) => e.ts,
        Event::Funding(e) => e.ts,
        Event::PositionLiquidated(e) => e.ts,
        Event::OrderCancelRejected(e) => e.ts,
        Event::OrderModifyRejected(e) => e.ts,
    }
}

/// The restart law's one predicate: is this replayed event evidence of activity that happened
/// BEFORE `spawn_ms`, i.e. before this process could have caused it?
///
/// **The ONE predicate of all THREE history-replay consumers**: [`run_loop`]'s gap-sentinel,
/// `crate::user_data::run_resync_supervisor` (the reconnect-gated replay of the SAME venue `resync`
/// closures), and — outside this crate, hence `pub` —
/// `vike_hyperliquid::user_data::map_frame_to_events`'s first-`userFills`-snapshot floor (HL
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
/// happened AT the mount, before this process placed anything.
///
/// ⚠ **That `recvWindow` argument does NOT hold for the third consumer, and it must not be relied on
/// there.** Hyperliquid's nonce window is `(T − 2 days, T + 1 day)`
/// (`crates/bridges/hyperliquid/src/signing/hash.rs`'s `NonceManager`), so a signed HL request
/// survives skew that would drag a fresh fill's venue stamp below a local floor. The HL caller
/// therefore does not use this predicate ALONE: it drops a row only when this returns `true` **and**
/// `crates/bridges/hyperliquid/src/exec.rs`'s `CloidRegistry`'s `resolve` says the order was not
/// minted by this process. A clock-only floor on that lane would silently drop a fill that exists
/// nowhere else.
pub fn is_pre_spawn(ev: &Event, spawn_ms: i64) -> bool {
    let ts = event_ts(ev);
    ts > 0 && ts < spawn_ms
}

/// The shared venue exec command pump: the command→REST→ingest mapping (generic over the
/// [`VenueRest`] surface so it is unit-tested with a mock), plus the gap-sentinel. Every
/// command-driven crypto venue (binance-family/deribit/bybit/okx) drives its [`ExecActor`] thread
/// with this ONE loop; only the venue's `run`/`run_spot`/`run_perp` driver (REST types, hosts,
/// signers, `tracing` targets, the resync closure) stays per-venue.
///
/// `Submit` forwards `submit_order`'s `[Submitted, Accepted|Rejected]`; `Cancel` maps
/// success→`OrderCanceled` / failure→`OrderCancelRejected` via [`cancel_event`]; `CancelBatch`
/// routes to [`VenueRest::cancel_batch`] and reports its one whole-batch `Result` per id; `Modify`
/// forwards `modify_order` (a native amend where the venue wires one; otherwise the `VenueRest`
/// default no-op — `[]`, the order keeps its terms).
/// `recv_timeout` arms a ONE-SHOT `resync` `fill_sentinel` after the last command, so a fill/cancel
/// the WS lost (esp. the first-connect subscribe race) is recovered; a normal WS delivery makes the
/// replay a dedup'd no-op. `Shutdown` returns so the caller tears down.
pub fn run_loop<R, F>(
    rest: &R,
    events: &EventSender,
    rx: Receiver<ExecCommand>,
    resync: F,
    fill_sentinel: Duration,
) where
    R: VenueRest,
    F: Fn() -> Vec<Event>,
{
    // The history-replay floor (the restart law — see the module doc). Sampled ONCE, here, because
    // this call IS the exec thread's spawn: nothing this process places can be filled before it.
    let spawn_ms = vike_model::now_ms();
    // `Some(deadline)` = a resync is armed for `deadline`; `None` = idle (block on the next command).
    let mut resync_due: Option<Instant> = None;
    // Every `emit(..)` below discards a `CoreGone`: the core has exited, so the process is shutting
    // down and there is nothing left to deliver to (pre-start and lost state is reconcile's job on
    // the next boot). The FIRST such drop is announced, once for this loop ([`send_or_warn_once`]).
    let warned = AtomicBool::new(false);
    let actor = thread::current().name().unwrap_or("exec-loop").to_string();
    let emit = |ev: Event| send_or_warn_once(events, &warned, &actor, ev);
    loop {
        let wait = match resync_due {
            Some(d) => d.saturating_duration_since(Instant::now()),
            None => Duration::from_secs(3600),
        };
        match rx.recv_timeout(wait) {
            Ok(ExecCommand::Submit(req)) => {
                for ev in rest.submit_order(&req) {
                    emit(ev);
                }
                resync_due = Some(Instant::now() + fill_sentinel);
            }
            // The REST venues meter nothing client-side, so the intent is read by nobody here.
            Ok(ExecCommand::Cancel { client_order_id: coid, .. }) => {
                let outcome = match rest.cancel_order(&coid) {
                    Ok(()) => CancelOutcome::Canceled,
                    Err(e) => CancelOutcome::Rejected(e.msg),
                };
                emit(cancel_event(&coid, outcome));
                resync_due = Some(Instant::now() + fill_sentinel);
            }
            // The bulk lane, routed to [`VenueRest::cancel_batch`] — the SAME seam
            // `LiveRestClient::cancel_batch` uses, so a native batch endpoint is reached here too
            // and a venue without one gets that trait method's per-id fan-out. Its `Result` covers
            // the WHOLE batch, so a failure is reported per id exactly as `LiveRestClient` reports
            // it: over-reporting a non-terminal reject is safe (it changes no status),
            // under-reporting would be the silent vanish. Unreachable while no `run_loop` venue
            // calls `with_bulk_cancel`; written so a venue that declares one is right on arrival.
            Ok(ExecCommand::CancelBatch { client_order_ids, .. }) => {
                let result = rest.cancel_batch(&client_order_ids);
                for coid in &client_order_ids {
                    let outcome = match &result {
                        Ok(()) => CancelOutcome::Canceled,
                        Err(e) => CancelOutcome::Rejected(e.msg.clone()),
                    };
                    emit(cancel_event(coid, outcome));
                }
                resync_due = Some(Instant::now() + fill_sentinel);
            }
            Ok(ExecCommand::Modify { order, new_qty, new_price }) => {
                // Native amend where the venue wires one (e.g. `[OrderModified]`); the `VenueRest`
                // default is a no-op (returns `[]`; the resting order keeps its terms).
                for ev in rest.modify_order(&order, new_qty, new_price) {
                    emit(ev);
                }
                resync_due = Some(Instant::now() + fill_sentinel);
            }
            Ok(ExecCommand::Shutdown) => break,
            Err(RecvTimeoutError::Timeout) => {
                if resync_due.is_some_and(|d| Instant::now() >= d) {
                    // The restart law: a replayed row older than this thread's spawn is dropped,
                    // whatever the venue closure returned. Counted, then reported ONCE per pass —
                    // a background lane, never the core fold, so one aggregated line is bounded.
                    let mut pre_spawn = 0usize;
                    for ev in resync() {
                        if is_pre_spawn(&ev, spawn_ms) {
                            pre_spawn += 1;
                            continue;
                        }
                        emit(ev);
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
                    resync_due = None;
                }
            }
            Err(RecvTimeoutError::Disconnected) => break,
        }
    }
}

/// A venue's order-status re-query for [`ExecActor::with_confirm`]: maps a client-order-id to the
/// authoritative event(s) to emit — typically a single `vike_bridge_core::resolve_ambiguous_submit`
/// result (Accepted when the venue HAS the order, Rejected only when it confirms absent). Invoked
/// on the actor's OWN short-lived worker thread (blocking REST) so it NEVER runs on the
/// single-writer core fold. An empty `Vec` = nothing conclusive to report.
pub type ConfirmFn = Arc<dyn Fn(&str) -> Vec<Event> + Send + Sync>;

/// A venue exec client backed by a dedicated command thread (+ optional background threads). Owns
/// the channel, the thread handles, the [`ExecutionClient`] impl, and deterministic teardown. Venues
/// wrap it in a newtype and forward `ExecutionClient` to `.0`; the newtype needs no `Drop` (this one's
/// `Drop` joins everything).
pub struct ExecActor {
    tx: Sender<ExecCommand>,
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
    /// The HALT sentinel this actor watches — handed in by the venue's mount from
    /// `MountInputs::process.halt_path` ([`Self::with_halt_path`]), never resolved here.
    ///
    /// `None` (a test double, a tool) watches NO file — never the process-wide resolution
    /// (`halt::halt_path_from_env`), because a bridge reads no process-global state (decision
    /// 0099). An actor with no path says so once, at `error` (`halt::sentinel_engaged`), because a
    /// mount that forgot to wire it is a dead kill switch.
    halt_path: Option<PathBuf>,
    /// Which classification path [`Self::on_submit_error`] takes; [`ExecErrorPolicy::Legacy`] is the
    /// default.
    error_policy: ExecErrorPolicy,
    /// This venue's error taxonomy, consulted only on the `Classified` path. `None` = use the
    /// central baseline alone.
    taxonomy: Option<VenueTaxonomy>,
    /// Bound on the classified retry loop: once a caller's attempt exceeds it, `on_submit_error`
    /// converts `Retry` into a terminal `Reject` and emits it, so a persistently-retryable failure
    /// can never leave an order with no terminal event.
    retry_budget: SubmitRetryBudget,
    /// Does this venue's command loop handle [`ExecCommand::CancelBatch`]? `false` (the default)
    /// means `cancel_batch_with_intent` fans the ids out into per-id [`ExecCommand::Cancel`]s
    /// BEFORE the channel. Set by [`Self::with_bulk_cancel`].
    bulk_cancel: bool,
    /// The thread name this actor was spawned under -- the `actor` field of the one-time
    /// core-gone warning ([`Self::emit`]).
    name: String,
    /// Raised the first time an event this actor sent found the core gone, so the warning is ONE
    /// line per actor and never one per dropped message. `Arc` because the [`ConfirmFn`] worker
    /// thread shares it.
    core_gone_warned: Arc<AtomicBool>,
}

impl ExecActor {
    /// Spawn the command thread. `run` MUST return on `ExecCommand::Shutdown` (or when the channel
    /// closes) so teardown is deterministic. `events` is a clone of the lane `run` pushes into; the
    /// actor keeps it to synthesize a terminal event if a command lands on a dead (closed) channel.
    pub fn spawn<F>(name: &str, events: EventSender, run: F) -> Self
    where
        F: FnOnce(Receiver<ExecCommand>) + Send + 'static,
    {
        // ⚠ No HALT path is resolved here: a spawn is a bridge's act, and the process-wide resolver
        // (and its unarmable-switch report) is the composition layer's — `vike-mount` resolves it
        // before the first mount (`ProcessFacts::halt_path`).
        let (tx, rx) = channel();
        let pin_label = name.to_string();
        let join = thread::Builder::new()
            .name(name.to_string())
            .spawn(move || {
                // Opt-in HFT pinning (VIKE_PIN_CORES=exec:N): keep the venue exec command thread on
                // a fixed core. No-op unless the env names the `exec` role — the default path.
                vike_exec::affinity::pin_current_thread(
                    vike_exec::affinity::Role::Exec,
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
            error_policy: ExecErrorPolicy::Legacy,
            taxonomy: None,
            retry_budget: SubmitRetryBudget::default(),
            bulk_cancel: false,
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

    /// Declare that this venue's command loop handles [`ExecCommand::CancelBatch`], so a
    /// multi-order cancel reaches the venue as ONE command instead of `n` shredded singles.
    ///
    /// The payoff is LATENCY (one round trip instead of `n` on an emergency flatten), paid in rate
    /// allowance (Polymarket's `DELETE /cancel-all` debits `1 + n` where `n` cancels debit `n`).
    /// Deciding that trade is the VENUE's job: the batch arrives whole, with its [`CancelIntent`],
    /// and the venue's own planner picks bulk or targeted. Not calling this leaves the fan-out: `n`
    /// per-id [`ExecCommand::Cancel`]s in the caller's order, each carrying the intent.
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

    /// Hand this actor the HALT sentinel it watches: the venue's mount passes
    /// `MountInputs::process.halt_path`, the one file `vike-mount` resolved for the whole process.
    /// Without it the actor watches nothing (see the field's doc); a test passes a path of its own,
    /// so it exercises the boundary without mutating the process environment.
    #[must_use]
    pub fn with_halt_path(mut self, path: PathBuf) -> Self {
        self.halt_path = Some(path);
        self
    }

    /// Opt into the classified submit-error path, optionally with this venue's own error table
    /// (`vike_binance::error_codes::TAXONOMY` and its bybit/okx siblings).
    ///
    /// **Not calling this leaves [`ExecErrorPolicy::Legacy`]**, which reproduces BOTH of the venues'
    /// hand-written arms: the ambiguous-timeout re-query first, the raw-message terminal reject
    /// otherwise. The flip is deliberately per-venue and explicit.
    ///
    /// ### The intended flip
    /// A venue's command loop answers a REST submit failure with two hand-written arms:
    /// `exc.code == E_TIMEOUT_AMBIGUOUS` → `resolve_ambiguous_submit`, else a terminal
    /// `OrderRejected` built from the raw venue message. To adopt the taxonomy it deletes both and
    /// routes the failure through [`Self::on_submit_error`], honoring the returned
    /// [`SubmitDisposition`]:
    ///
    /// - `Retry` — re-issue the SAME request after a backoff scaled by
    ///   [`ErrorKind::backoff_scale`](crate::transport::ErrorKind::backoff_scale), passing the
    ///   incremented [`SubmitAttempt`] back in. The seam bounds the loop for you.
    /// - `Requery` — run the venue's existing `resolve_ambiguous_submit` re-query. NEVER re-issue.
    /// - `Reject` — do NOTHING further: `on_submit_error` has ALREADY emitted the terminal
    ///   `OrderRejected`. Emitting your own would be a duplicate (the `ManagedOrder` FSM drops it,
    ///   but the adapter should not rely on that).
    #[must_use]
    pub fn with_error_policy(
        mut self,
        policy: ExecErrorPolicy,
        taxonomy: Option<VenueTaxonomy>,
    ) -> Self {
        self.error_policy = policy;
        self.taxonomy = taxonomy;
        self
    }

    /// Override the classified path's retry bound (default [`SubmitRetryBudget::default`]: 5
    /// attempts / 30s). Inert under [`ExecErrorPolicy::Legacy`], which never returns `Retry`.
    #[must_use]
    pub fn with_retry_budget(mut self, budget: SubmitRetryBudget) -> Self {
        self.retry_budget = budget;
        self
    }

    /// Decide what to do about a venue submit failure, emitting the terminal `OrderRejected` when
    /// (and only when) the failure is terminal.
    ///
    /// - [`ExecErrorPolicy::Legacy`] (default): the venues' two arms — the ambiguous-timeout
    ///   sentinel returns [`SubmitDisposition::Requery`] emitting nothing, and every other failure
    ///   emits a reject carrying the RAW venue message and returns [`SubmitDisposition::Reject`]
    ///   carrying the real central-baseline [`ErrorKind`](crate::transport::ErrorKind)
    ///   (informational — the EVENT carries no kind).
    /// - [`ExecErrorPolicy::Classified`]: classifies via [`classify_venue`] and returns the
    ///   disposition. It emits the reject ONLY for terminal kinds; `Retry`/`Requery` emit nothing,
    ///   because the order's fate is not yet decided and a premature terminal event would either
    ///   strand a phantom position (`Requery`) or lose an order a backoff would have placed
    ///   (`Retry`).
    ///
    /// ## The retry bound (why `attempt` is a parameter)
    /// `Retry` emitting nothing is only safe if the loop is bounded. `attempt` is the caller's
    /// position in its re-issue sequence ([`SubmitAttempt::first`] on the original submit, then
    /// [`SubmitAttempt::next`]); once [`SubmitRetryBudget::exhausted`] says the budget is spent, a
    /// would-be `Retry` becomes a `Reject` and the terminal `OrderRejected` is emitted HERE. So an
    /// adapter cannot regress the "exactly one terminal event" guarantee by forgetting to bound its
    /// own loop — the worst it can do is retry fewer times than the budget allows.
    ///
    /// [`SubmitDisposition::Requery`] is NEVER budget-converted: the venue may hold the order, and
    /// synthesizing a reject over a possibly-filled order strands a phantom position. A stuck
    /// re-query is closed by the venue's status confirm
    /// ([`Self::with_confirm`]) and, last-resort, by the core's stuck-order watchdog — not by
    /// guessing here.
    pub fn on_submit_error(
        &self,
        client_order_id: &str,
        ts: i64,
        err: &VenueApiError,
        attempt: SubmitAttempt,
    ) -> SubmitDisposition {
        let disposition = match self.error_policy {
            ExecErrorPolicy::Legacy => {
                // Arm 1 (the venues' FIRST arm): the ambiguous timeout re-queries — never rejects.
                if err.code == crate::transport::E_TIMEOUT_AMBIGUOUS {
                    return SubmitDisposition::Requery;
                }
                // Arm 2: terminal, reason = the venue's own text, byte-identical to the venue sites.
                // (`CoreGone` = shutting down; nothing to deliver to — likewise the sends below.)
                self.emit(Event::OrderRejected(OrderRejected {
                    client_order_id: client_order_id.to_string(),
                    reason: err.msg.clone().into(),
                    ts,
                }));
                return SubmitDisposition::Reject(err.kind());
            }
            ExecErrorPolicy::Classified => {
                submit_disposition(classify_venue(self.taxonomy.as_ref(), err))
            }
        };
        match disposition {
            SubmitDisposition::Reject(kind) => {
                // Terminal: discharge the contract obligation now.
                self.emit(Event::OrderRejected(OrderRejected {
                    client_order_id: client_order_id.to_string(),
                    reason: reject_reason(kind, &err.msg).into(),
                    ts,
                }));
                disposition
            }
            SubmitDisposition::Retry(kind) if self.retry_budget.exhausted(attempt) => {
                // The bound: a retryable failure that has outlasted the budget still owes the order
                // exactly one terminal event. Emit it here rather than trust an unwritten loop.
                self.emit(Event::OrderRejected(OrderRejected {
                    client_order_id: client_order_id.to_string(),
                    reason: retry_exhausted_reason(kind, attempt, &err.msg).into(),
                    ts,
                }));
                SubmitDisposition::Reject(kind)
            }
            // Retry (budget left) / Requery: the order's fate is still open — emit nothing.
            other => other,
        }
    }

    /// Deliver one synthesized event on the venue's lane. A closed lane is `CoreGone`: the core has
    /// exited, so the process is shutting down and there is nothing to deliver to -- announced ONCE
    /// per actor ([`send_or_warn_once`]), never per message.
    fn emit(&self, ev: Event) {
        send_or_warn_once(&self.events, &self.core_gone_warned, &self.name, ev);
    }

    /// True when the HALT sentinel file currently exists → new order placement must be refused.
    /// Consulted only at the submit/modify boundary (submits are infrequent) — never a hot path.
    fn halt_engaged(&self) -> bool {
        halt::sentinel_engaged(self.halt_path.as_deref())
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
        let _ = self.tx.send(ExecCommand::Shutdown);
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

impl ExecutionClient for ExecActor {
    fn submit(&mut self, request: &OrderRequest) {
        // HALT kill-switch: while the sentinel file exists, refuse orders that OPEN risk. Synthesize
        // the terminal OrderRejected the contract requires (the order never reaches the venue, but
        // the intent must NOT silently vanish), then return without touching the command channel.
        // Checked here at the submit boundary only — never on a per-message hot path.
        //
        // ⚠ A REDUCING SUBMIT IS ADMITTED (`halt::halt_admits_submit`, which owns that rule for this
        // adapter AND for hyperliquid's bespoke copy). A cancel passing through is not enough for
        // "a kill switch must never trap you in a position": a cancel removes a resting ORDER, while
        // getting OUT of a position means SENDING one (`OrderIntent::Flatten`'s `reduce_only`
        // MARKET leg — refused here, `market-exit` under a HALT file could close nothing).
        //
        // `halt_engaged()` is evaluated ONCE and reused: it is a filesystem `exists()`, and
        // rearranging these arms must not turn one syscall per submit into two.
        if self.halt_engaged() {
            if !halt::halt_admits_submit(request) {
                // `CoreGone` = shutting down; nothing to deliver to.
                self.emit(Event::OrderRejected(OrderRejected {
                    client_order_id: request.client_order_id.clone(),
                    reason: halt::HALT_REJECT_REASON.into(),
                    ts: request.ts,
                }));
                return;
            }
            // NEVER SILENT. This boundary can only check the caller-asserted flag (it holds no
            // position book — see `halt.rs`), so an order leaving while the operator believes the
            // switch is engaged must be findable in the log. Per-order and halt-only: nowhere near
            // a hot path.
            tracing::warn!(
                target: "vike_bridge_core::halt",
                coid = %request.client_order_id,
                symbol = %request.symbol,
                qty = request.qty,
                "HALT engaged: admitting a reduce_only submit so the operator can still get out. \
                 This boundary trusts the flag — it cannot verify the order against a position."
            );
        }
        if self.tx.send(ExecCommand::Submit(Box::new(request.clone()))).is_err() {
            // Dead venue thread (login failure / panic): the order never reached the venue, so
            // synthesize the terminal OrderRejected the contract requires — never let it vanish.
            // (If even this send fails the core is gone = shutting down; nothing to deliver to.)
            self.emit(Event::OrderRejected(OrderRejected {
                client_order_id: request.client_order_id.clone(),
                reason: "venue exec thread unavailable".to_string().into(),
                ts: request.ts,
            }));
        }
    }
    fn cancel(&mut self, client_order_id: &str) {
        // ONE cancel door, so the two can never disagree (the `cancel_with_intent` contract).
        self.cancel_with_intent(client_order_id, CancelIntent::Unspecified);
    }
    fn cancel_with_intent(&mut self, client_order_id: &str, intent: CancelIntent) {
        // Deliberately NOT halt-gated: a kill switch must let an operator reduce/exit, never trap a
        // position.
        if self
            .tx
            .send(ExecCommand::Cancel { client_order_id: client_order_id.to_string(), intent })
            .is_err()
        {
            // Dead venue thread: the cancel could not be delivered. Non-terminal advisory — the
            // order (if live) stays live; surface the failure rather than swallow it.
            // (If even this send fails the core is gone = shutting down; nothing to deliver to.)
            self.emit(Event::OrderCancelRejected(OrderCancelRejected {
                client_order_id: client_order_id.to_string(),
                reason: "venue exec thread unavailable".to_string().into(),
                ts: 0,
            }));
        }
    }
    /// ONE `CancelBatch` command for a venue that DECLARED a bulk lane
    /// ([`Self::with_bulk_cancel`]); for every other venue the per-id fan-out — `n` queued
    /// `Cancel`s in the caller's order, each naming why it was issued.
    fn cancel_batch_with_intent(&mut self, client_order_ids: &[String], intent: CancelIntent) {
        if !self.bulk_cancel {
            for c in client_order_ids {
                self.cancel_with_intent(c, intent);
            }
            return;
        }
        // Deliberately NOT halt-gated, like the single-cancel door.
        let cmd = ExecCommand::CancelBatch { client_order_ids: client_order_ids.to_vec(), intent };
        if self.tx.send(cmd).is_err() {
            // Dead venue thread: the batch could not be delivered. Reported PER ID, exactly as the
            // fan-out would have — non-terminal, so every order (if live) stays live, and one
            // undeliverable batch never looks different downstream from `n` undeliverable singles.
            for c in client_order_ids {
                // `CoreGone` = shutting down; nothing to deliver to.
                self.emit(Event::OrderCancelRejected(OrderCancelRejected {
                    client_order_id: c.clone(),
                    reason: "venue exec thread unavailable".to_string().into(),
                    ts: 0,
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
        // (`vike_exec::order`'s FSM maps `OrderModifyRejected` back to MODIFIABLE), so the resting
        // order keeps its terms. cTrader emits the same event with the same reason — the shared
        // wording lets one recogniser match every client.
        if self.halt_engaged() {
            // `CoreGone` = shutting down; nothing to deliver to.
            self.emit(Event::OrderModifyRejected(OrderModifyRejected {
                client_order_id: order.client_order_id.clone(),
                reason: halt::HALT_REJECT_REASON.into(),
                ts: order.ts,
            }));
            return;
        }
        // Non-terminal: a dead channel just means the resting order keeps its current terms (the
        // ExecutionClient::modify default contract) — nothing to synthesize, unlike submit/cancel.
        let _ = self.tx.send(ExecCommand::Modify {
            order: Box::new(order.clone()),
            new_qty,
            new_price,
        });
    }
    fn confirm(&mut self, client_order_id: &str) {
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
        let coid = client_order_id.to_string();
        // Run the re-query on a SHORT-LIVED worker thread — NEVER on the caller (the single-writer
        // core fold): the closure does blocking REST. It emits the venue's authoritative terminal on
        // the same lossless ingest lane every other venue event rides, so the core just folds it. If
        // the spawn itself fails (OS thread exhaustion), the watchdog's stage-2 reject still covers it.
        if let Ok(handle) =
            thread::Builder::new().name("exec-confirm".to_string()).spawn(move || {
                for ev in confirm(&coid) {
                    // `CoreGone` = shutting down; nothing to deliver to.
                    send_or_warn_once(&events, &warned, &actor, ev);
                }
            })
        {
            self.confirm_threads.push(handle);
        }
    }
    /// Raise the stop flags, join nothing — the trait's phase-one seam, wired here because this is
    /// the shared home most bridges reach `detach` through: the `.0` newtypes, plus
    /// `crates/bridges/vike-ibkr/src/lib.rs`'s `IbkrExecutionClient`, which holds its actor as a
    /// named field. Which bridges override the seam (those, plus the ones that own their own
    /// threads) is answered by a command, not a count, because a count rots the day a bridge is
    /// added: the roster is `git grep -l 'fn begin_detach' -- crates/bridges`.
    ///
    /// ⚠ **A wrapper that does not ALSO delegate this inherits the trait's no-op** and silently
    /// keeps the serial cost — the shadowing hazard the `Box<dyn ExecutionClient + Send>` impl's doc
    /// in `crates/vike-exec/src/execution_engine/client.rs` describes. Nothing fails when a wrapper
    /// forgets; it just pays for it at shutdown.
    fn begin_detach(&mut self) {
        self.signal_stop();
    }
    fn detach(&mut self) {
        self.stop();
    }
}

impl Drop for ExecActor {
    fn drop(&mut self) {
        self.stop();
    }
}

#[cfg(test)]
mod submit_error_policy_tests;

#[cfg(test)]
mod run_loop_tests;

#[cfg(test)]
mod cancel_event_tests;
