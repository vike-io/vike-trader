//! Generic exec-thread scaffold shared by the command-driven venue `ExecutionClient`s.
//!
//! Every command-driven venue (FXCM, IG, OANDA, Polymarket) ran the SAME wrapper: an mpsc channel +
//! a dedicated OS thread draining Submit/Cancel/Shutdown, the [`ExecutionClient`] impl, and a
//! `Drop`/`detach` that sends Shutdown and JOINS the thread — deterministic teardown matters (a
//! stranded feed thread is a 0xC0000409 on process exit). This collapses that ~35-line-per-venue
//! wrapper into one seam; each venue supplies only its command loop `run(Receiver<ExecCommand>)`.
//! Extra background threads (e.g. OANDA's fills stream) attach via [`ExecActor::with_background`] and
//! are flag-stopped + joined on teardown too. For the REST-driven crypto venues that command loop is
//! itself shared: [`run_loop`] (the command→REST→ingest pump + gap-sentinel, generic over
//! [`VenueRest`]) — hoisted here from four byte-identical copies in binance-family/deribit/bybit/okx.
//!
//! ## HALT kill-switch (submit boundary)
//! `submit` (and `modify`) consult the [`crate::halt`] file sentinel BEFORE touching the command
//! channel: while the HALT file exists, a submit is refused with a synthesized terminal
//! `OrderRejected` (never a silent vanish — the venue-adapter contract) and a modify is refused with
//! a NON-terminal `OrderModifyRejected` (the resting order keeps its terms). Cancel is deliberately
//! never gated — halt must let an operator reduce/exit, never trap a position. This works even when
//! the runtime is wedged: an operator `touch`es the file over ssh and the next submit stops.
//!
//! ⚠ The modify advisory is NEW, and the old behaviour is worth knowing because it is what a stale
//! binary still does: this arm returned with no event at all, so a halted modify looked exactly like
//! a modify that had been lost. `docs/ops/kill-switches.md` carried it as gap 5.
//!
//! ## Bulk cancel (opt-in, per venue)
//! A multi-order cancel used to be shredded HERE: `cancel_batch_with_intent` fanned out into `n`
//! per-id [`ExecCommand::Cancel`]s, so a venue's own `cancel_batch` was unreachable through this
//! actor no matter what that venue implemented. [`ExecCommand::CancelBatch`] is the intact lane,
//! and it is delivered ONLY to a venue that declared it handles one
//! ([`ExecActor::with_bulk_cancel`]) — an undeclared venue still gets `n` singles, byte-identical
//! to before. Polymarket is the only declaration in the tree; the reason it wants one is latency
//! on an emergency flatten (one round trip instead of `n`), and the reason the lane is opt-in is
//! that the trade is not free — its `cancel-all` debits `1 + n` rate tokens where `n` singles
//! debit `n`, which is a decision only the venue's own budget can make.
//!
//! ## History-replay floor (the restart law)
//! [`run_loop`]'s gap-sentinel calls a venue `resync` closure that replays a WINDOW OF VENUE
//! HISTORY — `/v5/order/history` + `/v5/execution/list` on bybit, `allOrders` + `userTrades` on
//! binance/aster, `get_order_history_by_instrument` + `get_user_trades_by_instrument` on deribit,
//! `orders-history` + `fills-history` on okx. Every one of those requests is bounded by a ROW
//! COUNT and by nothing else: no venue closure passes a start time. The only thing that stopped a
//! replayed row from folding twice was the core's in-memory `seen_trade_ids`, which is EMPTY in a
//! fresh process — so the first sentinel firing after a restart folded the venue's whole retained
//! execution history through the non-idempotent `Account::apply_fill` (`balance -= commission`,
//! `realized_pnl +=`), including fills for orders this process never placed. Measured on the CI box:
//! each unexplained equity step equalled the sum of PRIOR sessions' costs exactly.
//!
//! Hence [`run_loop`] samples its own spawn instant ([`vike_model::time::clock::now_ms`], once, on entry)
//! and NEVER forwards a resync event stamped before it (`is_pre_spawn`). The floor is here, in
//! the ONE shared loop, rather than in five venue closures: it is one venue-agnostic guarantee that
//! holds whatever a venue's `resync` returns, and this repo's recurring failure mode is a law
//! spelled five times.
//!
//! **Why a SPAWN-time floor is the right shape.** The replay's job is closing a gap WITHIN a
//! process's life (the first-connect subscribe race, a lost WS frame). Adopting state that existed
//! BEFORE the process started is RECONCILIATION's job, and reconcile has the machinery this lane
//! lacks — `VIKE_RECONCILE_POLICY`, quarantine, operator confirm (`vike_exec::recon::resolve`).
//! The two jobs were conflated and the more dangerous one was done silently.
//!
//! **Precedent: `bybit`'s `BybitFundingPoller`** (`crates/bridges/bybit/src/funding.rs`) guards the
//! same hazard the same way — a spawner-supplied `floor_ms`, a client-side timestamp filter, and
//! an in-memory id-dedup kept as the second guard within a process lifetime. The shapes are
//! analogous in the part that matters and differ in two:
//!   1. The funding floor is a PARAMETER (`BybitFundingPoller::new(.., now_ms())`, `0` = no floor
//!      for tests/probes); this one is sampled INSIDE [`run_loop`], because the loop already is the
//!      spawn — that keeps all five venue call sites byte-identical, and there is no legitimate
//!      caller that wants the floor off.
//!   2. Funding can ALSO shrink the response with a `startTime` request param; the venue history
//!      endpoints here are row-count-bounded, so the filter is purely client-side. That costs
//!      bandwidth, never correctness — in both cases the client-side timestamp check IS the
//!      guarantee.
//!
//! The justification is identical: those pre-start rows are ALREADY embedded in the venue-reported
//! balance/position the live account runs on, so re-emitting one double-counts it.
//!
//! **What the floor deliberately does NOT touch.** A mid-process WS reconnect still delivers every
//! fill that happened during the gap — those are stamped AFTER spawn and ride through untouched.
//! An UNSTAMPED event (`ts == 0`) also rides through: a missing timestamp is not evidence of being
//! historical, and no timestamp is ever fabricated here.

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
    /// cancels can make the shedding decision (and its debit) on ITS OWN thread, where the budget
    /// already lives — deciding on the caller's thread would separate the decision from the debit
    /// and let a burst of routine cancels each be allowed against a balance none of them had spent
    /// yet. A venue with no budget ignores the field; `Unspecified` is the flatten-safe default.
    Cancel {
        client_order_id: String,
        intent: CancelIntent,
    },
    /// Cancel MANY resting orders as ONE command, so a venue with a native bulk/mass-cancel
    /// endpoint sees the batch INTACT instead of the `n` shredded singles the fan-out below used to
    /// deliver. Carries the core's [`CancelIntent`] for exactly the reason [`ExecCommand::Cancel`]
    /// does — the venue decides and debits on ITS OWN thread — and carries it ONCE, because the
    /// core classifies a batch as a whole (`vike_exec::ExecutionEngine::mass_cancel_with_intent`),
    /// never per id.
    ///
    /// ⚠ **Delivered ONLY to a venue that DECLARED it handles one** —
    /// [`ExecActor::with_bulk_cancel`].
    /// Without that declaration [`ExecutionClient::cancel_batch_with_intent`] fans the ids out into
    /// per-id [`ExecCommand::Cancel`]s before they reach the channel, so a loop with no bulk path
    /// keeps receiving exactly what it received before — `n` singles, in the caller's order. That
    /// opt-in IS the safety interlock: a venue cannot be handed a batch shape it never wrote code
    /// for, and the arm it must write anyway ([`cancel_batch_undeclared`]) can therefore be the one
    /// shared, unreachable fallback rather than `n` per-venue re-implementations of a fan-out.
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

/// Map a cancel attempt to its canonical event so a failed cancel is NEVER swallowed (audit A2):
/// success → terminal `OrderCanceled`, any failure → non-terminal `OrderCancelRejected`. Shared by
/// every command-driven venue's `Cancel` branch.
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
/// **Unreachable by construction**, and that is the point: [`ExecActor`] only puts a `CancelBatch`
/// on the channel for a venue that called [`ExecActor::with_bulk_cancel`], and a loop reaching here
/// declared nothing — so its batches were already fanned out into per-id [`ExecCommand::Cancel`]s
/// before the channel. The arm still has to be WRITTEN, because a new enum variant is exhaustive
/// everywhere, and the tempting empty arm (`=> {}`) would turn a future mis-wiring into `n` orders
/// vanishing with no event at all — the one thing the emitter-split contract forbids.
///
/// So it refuses every id NON-terminally instead: the orders are still resting at the venue, the
/// caller re-offers them, and the `error!` names the missing declaration. ONE shared function
/// rather than a copy per venue — the loops that call it differ in nothing here, and this repo's
/// recurring failure mode is a law spelled five times.
pub fn cancel_batch_undeclared(events: &EventSender, client_order_ids: &[String]) {
    tracing::error!(
        target: "vike_bridge_core",
        count = client_order_ids.len(),
        "a CancelBatch reached a venue loop with no bulk-cancel path — every id refused \
         NON-terminally and every order left resting. Either wire the loop's CancelBatch arm or \
         drop its `with_bulk_cancel` declaration."
    );
    for coid in client_order_ids {
        let _ = events.blocking_send(Event::OrderCancelRejected(OrderCancelRejected {
            client_order_id: coid.clone(),
            reason: CANCEL_BATCH_UNDECLARED_REASON.to_string().into(),
            ts: 0,
        }));
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
/// **There are THREE history-replay consumers and this is the ONE predicate all of them use.**
/// [`run_loop`]'s gap-sentinel is the first; `crate::user_data::run_resync_supervisor` — the
/// reconnect-gated replay of the SAME venue `resync` closures — is the second; the third is OUTSIDE
/// this crate, `vike_hyperliquid::user_data::map_frame_to_events`'s first-`userFills`-snapshot floor,
/// which is why this is `pub` rather than `pub(crate)`. HL reaches neither of the two lanes above
/// (`spawn_hyperliquid_user_data` drives `run_user_data_forever_with_idle` directly, and its venue
/// re-sends `isSnapshot` on every subscribe), so its snapshot IS its history-replay path. A second
/// spelling of "is this row older than my process" is this repo's recurring defect, and three lanes
/// disagreeing about the boundary would be invisible until an equity step showed up on the CI box.
///
/// A POSITIVE stamp strictly below the floor is the only thing that qualifies. `ts == 0` (an
/// emitter that left the stamp blank — `cancel_event`, the live submit lane) and any stamp at or
/// after the floor ride through: the floor may only ever REMOVE what predates the process, never
/// guess at what an unstamped event was.
///
/// The floor is a LOCAL clock reading and `ts` is the VENUE's, so the comparison carries whatever
/// skew exists between them — deliberately with no slack, exactly as `BybitFundingPoller` does it.
/// For the two lanes in THIS crate that skew is bounded by the venues' own `recvWindow` (~5s: a
/// larger offset would make every signed request fail outright), and an event inside that band
/// happened AT the mount, so it belongs to an order this process had not yet placed — the very
/// thing being excluded.
///
/// ⚠ **That `recvWindow` argument does NOT hold for the third consumer, and it must not be relied on
/// there.** Hyperliquid's nonce window is `(T − 2 days, T + 1 day)`
/// (`crates/bridges/hyperliquid/src/signing/hash.rs`'s `NonceManager`), so a signed HL request
/// survives skew that would drag a fresh fill's venue stamp below a local floor. The HL caller
/// therefore does not use this predicate ALONE: it drops a row only when this returns `true` **and**
/// `crates/bridges/hyperliquid/src/exec.rs`'s `CloidRegistry`'s `resolve` says the order was not
/// minted by this process. A clock-only floor on that lane would silently drop a fill that exists
/// nowhere else, which is the defect its own reconnect gate exists to prevent.
pub fn is_pre_spawn(ev: &Event, spawn_ms: i64) -> bool {
    let ts = event_ts(ev);
    ts > 0 && ts < spawn_ms
}

/// The shared venue exec command pump: the command→REST→ingest mapping (generic over the
/// [`VenueRest`] surface so it is unit-tested with a mock), plus the gap-sentinel. Hoisted here from
/// four byte-identical venue copies (binance-family/deribit/bybit/okx `exec.rs`) — every
/// command-driven crypto venue drives its [`ExecActor`] thread with this ONE loop; only the venue's
/// `run`/`run_spot`/`run_perp` driver (REST types, hosts, signers, `tracing` targets, the resync
/// closure) stays per-venue.
///
/// `Submit` forwards `submit_order`'s `[Submitted, Accepted|Rejected]`; `Cancel` maps
/// success→`OrderCanceled` / failure→`OrderCancelRejected` via [`cancel_event`] (audit A2: a failed
/// cancel is never swallowed); `CancelBatch` routes to [`VenueRest::cancel_batch`] and reports its
/// one whole-batch `Result` per id (unreachable today — no venue here declares the lane); `Modify`
/// forwards `modify_order` (a native amend where the venue
/// wires one; the `VenueRest` default no-op — `[]`, the order keeps its terms — otherwise).
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
    // this call IS the exec thread's spawn: nothing this process places can be filled before it, so
    // a resync row stamped earlier belongs to a PREVIOUS session and folding it double-counts a fee
    // and a realized PnL that are already inside the venue-reported balance.
    let spawn_ms = vike_model::time::clock::now_ms();
    // `Some(deadline)` = a resync is armed for `deadline`; `None` = idle (block on the next command).
    let mut resync_due: Option<Instant> = None;
    loop {
        let wait = match resync_due {
            Some(d) => d.saturating_duration_since(Instant::now()),
            None => Duration::from_secs(3600),
        };
        match rx.recv_timeout(wait) {
            Ok(ExecCommand::Submit(req)) => {
                for ev in rest.submit_order(&req) {
                    let _ = events.blocking_send(ev);
                }
                resync_due = Some(Instant::now() + fill_sentinel);
            }
            // The REST venues meter nothing client-side, so the intent is read by nobody here and
            // every cancel goes to the wire exactly as before.
            Ok(ExecCommand::Cancel { client_order_id: coid, .. }) => {
                let outcome = match rest.cancel_order(&coid) {
                    Ok(()) => CancelOutcome::Canceled,
                    Err(e) => CancelOutcome::Rejected(e.msg),
                };
                let _ = events.blocking_send(cancel_event(&coid, outcome));
                resync_due = Some(Instant::now() + fill_sentinel);
            }
            // The bulk lane, routed to [`VenueRest::cancel_batch`] — the SAME seam
            // `LiveRestClient::cancel_batch` uses, so a venue that wired a native batch endpoint
            // (binance/bybit/okx/aster `perp.rs`) reaches it here too, and one that did not gets
            // that trait method's own per-id fan-out. Its `Result` covers the WHOLE batch and
            // cannot attribute a failure to one id, so a failure is reported per id exactly as
            // `LiveRestClient` reports it: over-reporting a non-terminal reject is safe (it changes
            // no status), under-reporting would be the silent vanish.
            //
            // Unreachable in the tree today — no `run_loop` venue calls `with_bulk_cancel`, so
            // every batch is still fanned out into the `Cancel` arm above. Written correctly
            // anyway, so a venue that later declares one is right on arrival.
            Ok(ExecCommand::CancelBatch { client_order_ids, .. }) => {
                let result = rest.cancel_batch(&client_order_ids);
                for coid in &client_order_ids {
                    let outcome = match &result {
                        Ok(()) => CancelOutcome::Canceled,
                        Err(e) => CancelOutcome::Rejected(e.msg.clone()),
                    };
                    let _ = events.blocking_send(cancel_event(coid, outcome));
                }
                resync_due = Some(Instant::now() + fill_sentinel);
            }
            Ok(ExecCommand::Modify { order, new_qty, new_price }) => {
                // Native amend where the venue wires one (e.g. `[OrderModified]`); the `VenueRest`
                // default is a no-op (returns `[]`; the resting order keeps its terms).
                for ev in rest.modify_order(&order, new_qty, new_price) {
                    let _ = events.blocking_send(ev);
                }
                resync_due = Some(Instant::now() + fill_sentinel);
            }
            Ok(ExecCommand::Shutdown) => break,
            Err(RecvTimeoutError::Timeout) => {
                if resync_due.is_some_and(|d| Instant::now() >= d) {
                    // The restart law: a replayed row older than this thread's spawn is a PREVIOUS
                    // session's activity and is dropped here, whatever the venue closure returned.
                    // Counted, then reported ONCE per pass — this is a 5s-cadence background lane,
                    // never the core fold, so one aggregated line is both visible and bounded.
                    let mut pre_spawn = 0usize;
                    for ev in resync() {
                        if is_pre_spawn(&ev, spawn_ms) {
                            pre_spawn += 1;
                            continue;
                        }
                        let _ = events.blocking_send(ev);
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

/// A venue's order-status re-query for [`ExecActor::with_confirm`] (audit ex1 residual): maps a
/// client-order-id to the authoritative event(s) to emit — typically a single
/// `vike_bridge_core::resolve_ambiguous_submit` result (Accepted when the venue HAS the order,
/// Rejected only when it confirms absent). Invoked on the actor's OWN short-lived worker thread
/// (blocking REST) so it NEVER runs on the single-writer core fold. `Vec` so a venue may emit more
/// than one event; an empty `Vec` = nothing conclusive to report.
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
    /// Venue order-status re-query (audit ex1 residual). `None` (default) = `confirm` is a clean
    /// no-op (venue has no re-query / is not an ExecActor venue). `Some` = `confirm` runs it on a
    /// short-lived worker thread ([`Self::confirm_threads`]) and emits its result on `events`.
    confirm: Option<ConfirmFn>,
    /// In-flight one-shot confirm workers — pruned when finished + joined on teardown so a re-query
    /// never strands a thread at process exit (deterministic teardown, like `background`).
    confirm_threads: Vec<JoinHandle<()>>,
    /// The HALT sentinel this actor watches — handed in by the venue's mount from
    /// `MountInputs::process.halt_path` ([`Self::with_halt_path`]), never resolved here.
    ///
    /// `None` (a test double, a tool) watches NO file: this actor used to fall back to the
    /// process-wide resolution (`halt::halt_path_from_env`) when none was given, which made every
    /// venue's kill switch work by reaching into process state at the moment an order arrived — the
    /// one place the mount contract's "a bridge reads no process-global state" did not hold, and it
    /// did not hold for ten venues at once (decision 0099). An actor with no path says so once, at
    /// `error` (`halt::sentinel_engaged`), because a mount that forgot to wire it is a dead kill
    /// switch.
    halt_path: Option<PathBuf>,
    /// Which classification path [`Self::on_submit_error`] takes (venue-error-taxonomy lane).
    /// [`ExecErrorPolicy::Legacy`] (the default) reproduces the pre-lane behavior exactly.
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
    /// BEFORE the channel, which is byte-identical to this actor never having had a bulk lane.
    /// Set by [`Self::with_bulk_cancel`].
    bulk_cancel: bool,
}

impl ExecActor {
    /// Spawn the command thread. `run` MUST return on `ExecCommand::Shutdown` (or when the channel
    /// closes) so teardown is deterministic. `events` is a clone of the lane `run` pushes into; the
    /// actor keeps it to synthesize a terminal event if a command lands on a dead (closed) channel.
    pub fn spawn<F>(name: &str, events: EventSender, run: F) -> Self
    where
        F: FnOnce(Receiver<ExecCommand>) + Send + 'static,
    {
        // ⚠ This used to RESOLVE — and, once per process, REPORT — the HALT sentinel path here
        // (`halt::halt_path_from_env`), so an unarmable switch landed in the trace file when a real
        // venue came up. It does not any more: a spawn is a bridge's act, and the process-wide
        // resolver is the composition layer's. `vike-mount` resolves it before the first mount
        // (`ProcessFacts::halt_path`), which is the same moment and the same report.
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
    /// stuck order (audit ex1 residual): the closure re-runs the venue's EXISTING re-query (the one
    /// behind `resolve_ambiguous_submit`) and returns the authoritative event(s) to emit. The actor
    /// runs it on its OWN short-lived worker thread (blocking REST) and pushes the result onto the
    /// venue event lane — so the core's watchdog can prod a wedged adapter with zero network on the
    /// fold. Venues without a status re-query simply never call this: `confirm` then stays the clean
    /// default no-op and only the watchdog's last-resort reject backstops the order.
    #[must_use]
    pub fn with_confirm(mut self, confirm: ConfirmFn) -> Self {
        self.confirm = Some(confirm);
        self
    }

    /// Declare that this venue's command loop handles [`ExecCommand::CancelBatch`], so a
    /// multi-order cancel reaches the venue as ONE command instead of `n` shredded singles.
    ///
    /// The payoff is LATENCY, and it is bought at a small cost in venue rate allowance rather than
    /// for free — Polymarket's `DELETE /cancel-all` debits `1 + n` where `n` individual cancels
    /// debit `n`, while costing ONE round trip instead of `n`. That is exactly the trade an
    /// emergency flatten wants (twenty resting orders is ~2s of serial HTTP at 100ms a hop), and
    /// deciding it is the VENUE's job, not this seam's: the batch arrives whole, with its
    /// [`CancelIntent`], and the venue's own planner picks bulk or targeted.
    ///
    /// **Not calling this is the default and leaves the fan-out**, which every venue in the tree
    /// but Polymarket wants: `n` per-id [`ExecCommand::Cancel`]s in the caller's order, each
    /// carrying the intent, byte-identical to the behaviour that shipped before this lane.
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
    /// which is also how a unit test exercises the boundary without mutating the process
    /// environment (the repo avoids `set_var` under threads — see `credentials.rs`).
    #[must_use]
    pub fn with_halt_path(mut self, path: PathBuf) -> Self {
        self.halt_path = Some(path);
        self
    }

    /// Opt into the classified submit-error path (venue-error-taxonomy lane), optionally with this
    /// venue's own error table (`vike_binance::error_codes::TAXONOMY` and its bybit/okx siblings).
    ///
    /// **Not calling this leaves [`ExecErrorPolicy::Legacy`], which reproduces the behavior that
    /// shipped before the lane** — BOTH of its arms: the ambiguous-timeout re-query the venues do
    /// first, and the raw-message terminal reject they do otherwise. The flip is deliberately
    /// per-venue and deliberately explicit.
    ///
    /// ### The intended flip
    /// A venue's command loop currently answers a REST submit failure with two hand-written arms:
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
    ///
    /// That change is per-venue and NOT part of this lane — the seam and its tests land first so the
    /// flip is a small, reviewable diff per adapter.
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
    /// - [`ExecErrorPolicy::Legacy`] (default): reproduces the venues' EXISTING two arms — the
    ///   ambiguous-timeout sentinel returns [`SubmitDisposition::Requery`] emitting nothing (what
    ///   `binance/spot.rs`, `binance/perp.rs` and the bybit/okx twins do before anything else), and
    ///   every other failure emits a reject carrying the RAW venue message and returns
    ///   [`SubmitDisposition::Reject`] carrying the real central-baseline
    ///   [`ErrorKind`](crate::transport::ErrorKind) (informational — Legacy's EVENT is unchanged).
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
    /// synthesizing a reject over a possibly-filled order is the phantom-position failure audit T1
    /// exists to prevent. A stuck re-query is closed by the venue's status confirm
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
                let _ = self.events.blocking_send(Event::OrderRejected(OrderRejected {
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
                let _ = self.events.blocking_send(Event::OrderRejected(OrderRejected {
                    client_order_id: client_order_id.to_string(),
                    reason: reject_reason(kind, &err.msg).into(),
                    ts,
                }));
                disposition
            }
            SubmitDisposition::Retry(kind) if self.retry_budget.exhausted(attempt) => {
                // The bound: a retryable failure that has outlasted the budget still owes the order
                // exactly one terminal event. Emit it here rather than trust an unwritten loop.
                let _ = self.events.blocking_send(Event::OrderRejected(OrderRejected {
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

    /// True when the HALT sentinel file currently exists → new order placement must be refused.
    /// Consulted only at the submit/modify boundary (submits are infrequent) — never a hot path.
    fn halt_engaged(&self) -> bool {
        halt::sentinel_engaged(self.halt_path.as_deref())
    }

    /// Signal every thread this actor owns to wind down, and JOIN NOTHING.
    ///
    /// The first half of [`Self::stop`], hoisted so a caller holding SEVERAL actors can raise all
    /// their flags before it joins the first — see [`ExecutionClient::begin_detach`], which this
    /// backs, for why that split is worth a method. `stop` still calls it, so the single-actor path
    /// is byte-identical and there is no second copy of the signalling to keep in step.
    ///
    /// **IDEMPOTENT by construction, which the seam requires**: the `Shutdown` send is already
    /// `let _ =` (a command thread that has exited makes it an ignored error) and a `bool` flag
    /// store is a store either way. So `begin_detach` → `detach` re-signals harmlessly, and a plain
    /// `detach` with no `begin_detach` before it is unchanged.
    ///
    /// ⚠ `Shutdown` rides the SAME command channel as submits and cancels, so anything already
    /// queued — the shutdown-policy cancel sweep above all — is still drained ahead of it. Raising
    /// the flag early does not cut a cancel that was already sent.
    fn signal_stop(&mut self) {
        let _ = self.tx.send(ExecCommand::Shutdown);
        for (flag, _) in &self.background {
            flag.store(true, Ordering::Relaxed);
        }
    }

    fn stop(&mut self) {
        // signal everything first, THEN join, so the command + background threads wind down together.
        self.signal_stop();
        if let Some(j) = self.join.take() {
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
        // adapter AND for hyperliquid's bespoke copy). A cancel passing through was never enough to
        // satisfy "a kill switch must never trap you in a position": a cancel removes a resting
        // ORDER, while getting OUT of a position means SENDING one — and `OrderIntent::Flatten`'s
        // `reduce_only` MARKET leg used to be refused right here, so `market-exit` under a HALT file
        // cancelled everything and could then close nothing.
        //
        // `halt_engaged()` is evaluated ONCE and reused: it is a filesystem `exists()`, and
        // rearranging these arms must not turn one syscall per submit into two.
        if self.halt_engaged() {
            if !halt::halt_admits_submit(request) {
                let _ = self.events.blocking_send(Event::OrderRejected(OrderRejected {
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
            let _ = self.events.blocking_send(Event::OrderRejected(OrderRejected {
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
        // Deliberately NOT halt-gated, exactly like `cancel` always was: a kill switch must let an
        // operator reduce/exit, never trap a position.
        if self
            .tx
            .send(ExecCommand::Cancel { client_order_id: client_order_id.to_string(), intent })
            .is_err()
        {
            // Dead venue thread: the cancel could not be delivered. Non-terminal advisory — the
            // order (if live) stays live; surface the failure rather than swallow it.
            let _ = self.events.blocking_send(Event::OrderCancelRejected(OrderCancelRejected {
                client_order_id: client_order_id.to_string(),
                reason: "venue exec thread unavailable".to_string().into(),
                ts: 0,
            }));
        }
    }
    /// ONE `CancelBatch` command for a venue that DECLARED a bulk lane
    /// ([`Self::with_bulk_cancel`]); the per-id fan-out for every venue that did not.
    ///
    /// The fan-out arm is the behaviour this method always had and is unchanged: `n` queued
    /// `Cancel`s in the caller's order, each naming why it was issued. The bulk arm is what lets a
    /// batch survive the seam — before it existed, a venue's `cancel_batch` was unreachable through
    /// this actor no matter what the venue implemented, because the batch was already shredded by
    /// the time any venue code ran.
    fn cancel_batch_with_intent(&mut self, client_order_ids: &[String], intent: CancelIntent) {
        if !self.bulk_cancel {
            for c in client_order_ids {
                self.cancel_with_intent(c, intent);
            }
            return;
        }
        // Deliberately NOT halt-gated, exactly like the single-cancel door: a kill switch must let
        // an operator reduce/exit, never trap a position.
        let cmd = ExecCommand::CancelBatch { client_order_ids: client_order_ids.to_vec(), intent };
        if self.tx.send(cmd).is_err() {
            // Dead venue thread: the batch could not be delivered. Reported PER ID, exactly as the
            // fan-out would have — non-terminal, so every order (if live) stays live, and one
            // undeliverable batch never looks different downstream from `n` undeliverable singles.
            for c in client_order_ids {
                let _ =
                    self.events.blocking_send(Event::OrderCancelRejected(OrderCancelRejected {
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
        // (always allowed), never modify. The VERDICT is unchanged and must never drift.
        //
        // ⚠ It used to `return` here with no event and no log line, and that silence was
        // `docs/ops/kill-switches.md`'s gap 5: a blocked modify was byte-indistinguishable from a
        // LOST one — same absence of events, same resting order at its old terms — so an operator
        // amending under a halt could not tell a working kill switch from a wedged adapter. The
        // advisory is NON-TERMINAL (`vike_exec::order`'s FSM maps `OrderModifyRejected` back to
        // MODIFIABLE), so the resting order keeps its terms exactly as before and nothing about
        // what a halt ADMITS changes; only whether you are told. cTrader has emitted precisely this
        // event, with precisely this reason, since its own halt arm landed — the shared wording is
        // what lets one recogniser match all four clients.
        if self.halt_engaged() {
            let _ = self.events.blocking_send(Event::OrderModifyRejected(OrderModifyRejected {
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
        // No re-query wired (venue has none, or this isn't a status-re-query venue) → clean no-op;
        // the watchdog's stage-2 last-resort reject still backstops the order. NOT gated on HALT: a
        // read-only status confirm places no order, and observing venue truth must never be blocked.
        let Some(confirm) = self.confirm.clone() else {
            return;
        };
        let events = self.events.clone();
        let coid = client_order_id.to_string();
        // Run the re-query on a SHORT-LIVED worker thread — NEVER on the caller (the single-writer
        // core fold): the closure does blocking REST. It emits the venue's authoritative terminal on
        // the same lossless ingest lane every other venue event rides, so the core just folds it. If
        // the spawn itself fails (OS thread exhaustion), the watchdog's stage-2 reject still covers it.
        if let Ok(handle) =
            thread::Builder::new().name("exec-confirm".to_string()).spawn(move || {
                for ev in confirm(&coid) {
                    let _ = events.blocking_send(ev);
                }
            })
        {
            self.confirm_threads.push(handle);
        }
    }
    /// Raise the stop flags, join nothing — the trait's phase-one seam, wired here because this is
    /// the shared home most of the roster bridges reach `detach` through: the `.0` newtypes, plus
    /// `crates/bridges/vike-ibkr/src/lib.rs`'s `IbkrExecutionClient`, which holds its actor as a
    /// named field. Which bridges override the seam at all — those, plus the ones that own their
    /// own threads — is deliberately a command and not a count, because a count written here rots
    /// the day a bridge is added: the roster is `git grep -l 'fn begin_detach' -- crates/bridges`.
    ///
    /// ⚠ **A wrapper that does not ALSO delegate this inherits the trait's no-op** and silently
    /// keeps the serial cost — the same shadowing hazard the `Box<dyn ExecutionClient + Send>`
    /// impl's doc in `crates/vike-exec/src/execution_engine/client.rs` was written for. Nothing
    /// fails when a wrapper forgets; it just pays for it at shutdown.
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

#[path = "submit_error_policy_tests.rs"]
#[cfg(test)]
mod submit_error_policy_tests;

#[path = "run_loop_tests.rs"]
#[cfg(test)]
mod run_loop_tests;

#[cfg(test)]
mod cancel_event_tests {
    use super::{CancelOutcome, cancel_event};
    use vike_model::events::Event;

    #[test]
    fn confirmed_cancel_maps_to_terminal_order_canceled() {
        match cancel_event("c1", CancelOutcome::Canceled) {
            Event::OrderCanceled(e) => assert_eq!(e.client_order_id, "c1"),
            other => panic!("expected OrderCanceled, got {other:?}"),
        }
    }

    #[test]
    fn failed_cancel_maps_to_nonterminal_reject_with_reason() {
        match cancel_event("c1", CancelOutcome::Rejected("venue down".into())) {
            Event::OrderCancelRejected(e) => {
                assert_eq!(e.client_order_id, "c1");
                assert_eq!(e.reason, "venue down");
            }
            other => panic!("expected OrderCancelRejected, got {other:?}"),
        }
    }
}
