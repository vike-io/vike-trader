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
//! `OrderRejected` (never a silent vanish — the venue-adapter contract) and a modify is dropped
//! (the resting order keeps its terms). Cancel is deliberately never gated — halt must let an
//! operator reduce/exit, never trap a position. This works even when the runtime is wedged: an
//! operator `touch`es the file over ssh and the next submit stops.
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
//! Hence [`run_loop`] samples its own spawn instant ([`vike_model::clock::now_ms`], once, on entry)
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
use vike_model::events::{Event, OrderCancelRejected, OrderRejected};

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
    let spawn_ms = vike_model::clock::now_ms();
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
    /// Explicit HALT sentinel path override. `None` (the default) uses the process-wide resolution
    /// (`halt::halt_path_from_env` — `VIKE_HALT_FILE`, else the project state directory, else the
    /// exe directory); `Some` pins a fixed path (tests / bespoke deployments) so the boundary can be
    /// exercised without mutating the process env (the repo avoids `set_var` under threads — see
    /// `credentials.rs`).
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
        // Resolve — and, once per process, REPORT — the HALT sentinel path at MOUNT time rather
        // than at the first submit. `halt::halt_path_from_env` memoizes and logs an unarmable path
        // at `error`; asking for it here is what puts that line in the trace file when a real venue
        // comes up, instead of leaving an operator to discover an EROFS `touch` mid-incident. Cheap
        // after the first venue: one `OnceLock` load. A later `with_halt_path` override does not
        // suppress it — the process-wide default is still the answer the runbook would give.
        let _ = halt::halt_path_from_env();
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

    /// Pin an explicit HALT sentinel path (default `None` = the process-wide resolution,
    /// `halt::halt_path_from_env`). For tests and bespoke deployments that want a fixed sentinel
    /// location without touching the process environment.
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
        match &self.halt_path {
            Some(p) => p.exists(),
            None => halt::halt_path_from_env().exists(),
        }
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
        // (always allowed), never modify. Non-terminal like a dead channel: the resting order keeps
        // its current terms, so nothing is synthesized and nothing vanishes.
        if self.halt_engaged() {
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

#[cfg(test)]
mod submit_error_policy_tests {
    use super::*;
    use crate::transport::{E_TIMEOUT_AMBIGUOUS, ErrorKind};
    use vike_exec::event_channel;

    /// A taxonomy shaped like a real venue's: an insufficient-balance code, a maintenance code, and
    /// a rate-limit code.
    const TAX: VenueTaxonomy = VenueTaxonomy {
        venue: "test",
        by_code: |code| match code {
            110_004 => Some(ErrorKind::InsufficientFunds),
            50_001 => Some(ErrorKind::VenueMaintenance),
            _ => None,
        },
        by_msg: |_| None,
    };

    fn err(code: i64, msg: &str) -> VenueApiError {
        VenueApiError { code, msg: msg.to_string() }
    }

    /// Build an actor whose command thread returns immediately — this test exercises only
    /// `on_submit_error`, which is a pure decision + emit and never touches the command channel.
    type Rx = tokio::sync::mpsc::Receiver<vike_exec::Ingest>;

    fn actor(policy: ExecErrorPolicy, tax: Option<VenueTaxonomy>) -> (ExecActor, Rx) {
        let (tx, rx) = event_channel(64);
        let a = ExecActor::spawn("test-exec", tx, |_rx| {}).with_error_policy(policy, tax);
        (a, rx)
    }

    fn drain_rejects(rx: &mut Rx) -> Vec<(String, String)> {
        let mut out = Vec::new();
        while let Ok(vike_exec::Ingest::Event(Event::OrderRejected(r))) = rx.try_recv() {
            out.push((r.client_order_id.clone(), r.reason.to_string()));
        }
        out
    }

    /// THE non-regression pin, ORDINARY arm: the DEFAULT policy emits exactly what the venues emit
    /// today — one terminal reject carrying the venue's RAW message, with no taxonomy decoration.
    #[test]
    fn legacy_policy_is_byte_identical_to_the_pre_lane_behavior() {
        let (a, mut rx) = actor(ExecErrorPolicy::Legacy, Some(TAX));
        for e in [err(110_004, "Wallet balance is insufficient"), err(50_001, "system maintenance")]
        {
            let d = a.on_submit_error("c1", 7, &e, SubmitAttempt::first());
            assert!(
                matches!(d, SubmitDisposition::Reject(_)),
                "legacy rejects the non-timeout arm"
            );
        }
        let got = drain_rejects(&mut rx);
        assert_eq!(
            got,
            vec![
                ("c1".to_string(), "Wallet balance is insufficient".to_string()),
                ("c1".to_string(), "system maintenance".to_string()),
            ],
            "legacy must carry the RAW venue message, undecorated"
        );
    }

    /// REGRESSION (adversarial review, major): Legacy must reproduce BOTH of the venues' arms.
    /// `binance/spot.rs`, `binance/perp.rs` and the bybit/okx twins match `E_TIMEOUT_AMBIGUOUS`
    /// FIRST and route it to `resolve_ambiguous_submit`; only the fall-through arm rejects. The
    /// earlier seam collapsed both into a terminal reject — so an adapter doing the natural
    /// mechanical adoption ("route the failure through the seam, keep Legacy, flip later") would
    /// silently DELETE its audit-T1 re-query and emit a false reject over a possibly-filled order.
    #[test]
    fn legacy_preserves_the_venues_ambiguous_timeout_requery_arm() {
        let (a, mut rx) = actor(ExecErrorPolicy::Legacy, Some(TAX));
        assert_eq!(
            a.on_submit_error(
                "c1",
                7,
                &err(E_TIMEOUT_AMBIGUOUS, "timed out"),
                SubmitAttempt::first()
            ),
            SubmitDisposition::Requery,
            "legacy must re-query the ambiguous timeout, exactly like the venue sites do"
        );
        assert!(
            drain_rejects(&mut rx).is_empty(),
            "a phantom position is the cost of rejecting here — legacy must emit NOTHING"
        );
    }

    /// Legacy's returned kind is the REAL central-baseline classification, not a fabricated
    /// `Unknown` — so a caller that branches/meters on the kind (e.g. halting on `Auth`) sees the
    /// truth even while that venue is still on Legacy. The EVENT is unchanged (raw venue message).
    #[test]
    fn legacy_returns_the_real_kind_not_a_fabricated_unknown() {
        let (a, _rx) = actor(ExecErrorPolicy::Legacy, Some(TAX));
        assert_eq!(
            a.on_submit_error("c1", 0, &err(401, "bad key"), SubmitAttempt::first()),
            SubmitDisposition::Reject(ErrorKind::Auth)
        );
    }

    /// REGRESSION (adversarial review, major): `Retry` emits nothing, so an unbounded retry loop
    /// would leave the order with NO terminal event — the "no order may silently vanish" guarantee
    /// broken by omission in a caller that does not exist yet. The seam owns the bound: once the
    /// budget is spent, the would-be Retry becomes a Reject and the terminal event is emitted HERE.
    #[test]
    fn classified_retry_is_bounded_and_ends_in_exactly_one_terminal_event() {
        let (a, mut rx) = actor(ExecErrorPolicy::Classified, Some(TAX));
        let e = err(50_001, "system maintenance"); // persistently retryable
        let budget = SubmitRetryBudget::default();
        let mut attempt = SubmitAttempt::first();
        let mut terminal = 0;
        for _ in 0..50 {
            match a.on_submit_error("c9", 0, &e, attempt) {
                SubmitDisposition::Retry(k) => {
                    assert_eq!(k, ErrorKind::VenueMaintenance);
                    attempt = attempt.next(0);
                }
                SubmitDisposition::Reject(k) => {
                    assert_eq!(k, ErrorKind::VenueMaintenance, "the reject names the real cause");
                    terminal += 1;
                    break;
                }
                SubmitDisposition::Requery => panic!("a maintenance code must not re-query"),
            }
        }
        assert_eq!(terminal, 1, "a persistently-retryable submit MUST reach a terminal event");
        assert_eq!(attempt.attempt, budget.max_attempts, "and only after the budget is spent");
        let rejects = drain_rejects(&mut rx);
        assert_eq!(rejects.len(), 1, "exactly one terminal event, emitted by the seam");
        assert!(
            rejects[0].1.contains("retry budget exhausted") && rejects[0].1.contains("maintenance"),
            "the reason must show BOTH the venue text and that we gave up retrying: {}",
            rejects[0].1
        );
    }

    /// The elapsed-time arm of the same bound: a slow retry sequence terminates even with attempts
    /// to spare (a 30s-stale quote is not the order the operator asked for).
    #[test]
    fn classified_retry_budget_also_bounds_on_elapsed_time() {
        let (a, mut rx) = actor(ExecErrorPolicy::Classified, Some(TAX));
        let e = err(50_001, "system maintenance");
        assert_eq!(
            a.on_submit_error("c10", 0, &e, SubmitAttempt { attempt: 2, elapsed_ms: 30_000 }),
            SubmitDisposition::Reject(ErrorKind::VenueMaintenance)
        );
        assert_eq!(drain_rejects(&mut rx).len(), 1);
    }

    /// The ambiguous timeout is NEVER budget-converted: the venue may hold the order, so a
    /// synthesized reject would strand a phantom position. It stays `Requery` however long it runs.
    #[test]
    fn requery_is_never_converted_by_the_retry_budget() {
        let (a, mut rx) = actor(ExecErrorPolicy::Classified, Some(TAX));
        assert_eq!(
            a.on_submit_error(
                "c11",
                0,
                &err(E_TIMEOUT_AMBIGUOUS, "timed out"),
                SubmitAttempt { attempt: 999, elapsed_ms: u64::MAX }
            ),
            SubmitDisposition::Requery
        );
        assert!(drain_rejects(&mut rx).is_empty(), "audit T1 outranks the retry budget");
    }

    /// A terminal classified failure emits the reject WITH the kind named in the reason.
    #[test]
    fn classified_terminal_error_rejects_with_the_kind_in_the_reason() {
        let (a, mut rx) = actor(ExecErrorPolicy::Classified, Some(TAX));
        let d = a.on_submit_error(
            "c2",
            9,
            &err(110_004, "Wallet balance is insufficient"),
            SubmitAttempt::first(),
        );
        assert_eq!(d, SubmitDisposition::Reject(ErrorKind::InsufficientFunds));
        assert_eq!(
            drain_rejects(&mut rx),
            vec![(
                "c2".to_string(),
                "venue error [insufficient_funds]: Wallet balance is insufficient".to_string()
            )]
        );
    }

    /// A retryable failure emits NOTHING — losing an order a backoff would have placed is the exact
    /// failure mode the classified path exists to remove.
    #[test]
    fn classified_transient_error_emits_no_terminal_event() {
        let (a, mut rx) = actor(ExecErrorPolicy::Classified, Some(TAX));
        assert_eq!(
            a.on_submit_error("c3", 0, &err(50_001, "system maintenance"), SubmitAttempt::first()),
            SubmitDisposition::Retry(ErrorKind::VenueMaintenance)
        );
        assert_eq!(
            a.on_submit_error("c3", 0, &err(429, "http error"), SubmitAttempt::first()),
            SubmitDisposition::Retry(ErrorKind::RateLimited)
        );
        assert!(drain_rejects(&mut rx).is_empty(), "a retryable failure must NOT reject the order");
    }

    /// AUDIT T1 under the new policy: the ambiguous timeout re-queries and emits nothing. Emitting a
    /// reject here would strand a phantom position if the venue did accept the order.
    #[test]
    fn classified_ambiguous_timeout_requeries_and_never_rejects() {
        let (a, mut rx) = actor(ExecErrorPolicy::Classified, Some(TAX));
        assert_eq!(
            a.on_submit_error(
                "c4",
                0,
                &err(E_TIMEOUT_AMBIGUOUS, "timed out"),
                SubmitAttempt::first()
            ),
            SubmitDisposition::Requery
        );
        assert!(
            drain_rejects(&mut rx).is_empty(),
            "the ambiguous path must NEVER synthesize a reject"
        );
    }

    /// An unknown venue code is terminal — the safe posture — and still discharges the contract.
    #[test]
    fn classified_unknown_code_rejects() {
        let (a, mut rx) = actor(ExecErrorPolicy::Classified, Some(TAX));
        assert_eq!(
            a.on_submit_error("c5", 0, &err(987_654, "brand new failure"), SubmitAttempt::first()),
            SubmitDisposition::Reject(ErrorKind::Unknown)
        );
        assert_eq!(drain_rejects(&mut rx).len(), 1, "an unknown failure must not vanish");
    }

    /// Classified WITHOUT a taxonomy still works — it just falls back to the central baseline.
    #[test]
    fn classified_without_a_taxonomy_uses_the_central_baseline() {
        let (a, _rx) = actor(ExecErrorPolicy::Classified, None);
        // 110004 is unknown to the baseline (only the venue table knows it) → terminal Unknown.
        assert_eq!(
            a.on_submit_error(
                "c6",
                0,
                &err(110_004, "Wallet balance is insufficient"),
                SubmitAttempt::first()
            ),
            SubmitDisposition::Reject(ErrorKind::Unknown)
        );
        // A baseline-known code still classifies.
        assert_eq!(
            a.on_submit_error("c6", 0, &err(500, "http error"), SubmitAttempt::first()),
            SubmitDisposition::Retry(ErrorKind::ServerError)
        );
    }
}

#[cfg(test)]
mod run_loop_tests {
    //! Pure command→event wiring tests — a mock `VenueRest` (NO network, NO real orders) proves
    //! `run_loop` routes Submit→`submit_order` (forwarding its events), Cancel→`cancel_order`
    //! (audit-A2 cancel event, failure → non-terminal `OrderCancelRejected`), Modify→`modify_order`
    //! (forwarding a native amend's events; the `VenueRest` default no-op forwards nothing), and
    //! fires the gap-sentinel resync after an idle submit. Moved here WITH `run_loop` from the four
    //! venue copies (binance-family/deribit/bybit/okx `exec::tests`) — the venues keep only their
    //! venue-specific tests (`.P` routing, currency derivation, caps ties, properties recording).
    use super::*;
    use std::sync::Mutex;
    use vike_exec::event_channel;
    use vike_exec::lanes::Ingest;
    use vike_model::events::{OrderAccepted, OrderModified, OrderSubmitted};

    #[derive(Default)]
    struct MockRest {
        submitted: Mutex<Vec<String>>,
        canceled: Mutex<Vec<String>>,
        modified: Mutex<Vec<(String, Option<f64>)>>,
        cancel_fails: bool,
        /// When true, `modify_order` returns a native `OrderModified` (the native-amend capability);
        /// when false it is the `VenueRest` default no-op (`[]`).
        native_modify: bool,
    }

    impl VenueRest for MockRest {
        fn submit_order(&self, request: &OrderRequest) -> Vec<Event> {
            self.submitted.lock().unwrap().push(request.client_order_id.clone());
            vec![
                Event::OrderSubmitted(OrderSubmitted {
                    client_order_id: request.client_order_id.clone(),
                    ts: 0,
                }),
                Event::OrderAccepted(OrderAccepted {
                    client_order_id: request.client_order_id.clone(),
                    venue_order_id: Some("v-1".to_string().into()),
                    ts: 0,
                }),
            ]
        }
        fn cancel_order(&self, client_order_id: &str) -> Result<(), VenueApiError> {
            self.canceled.lock().unwrap().push(client_order_id.to_string());
            if self.cancel_fails {
                Err(VenueApiError { code: 1, msg: "boom".to_string() })
            } else {
                Ok(())
            }
        }
        fn modify_order(
            &self,
            order: &OrderRequest,
            new_qty: Option<f64>,
            new_price: Option<f64>,
        ) -> Vec<Event> {
            self.modified.lock().unwrap().push((order.client_order_id.clone(), new_price));
            if self.native_modify {
                vec![Event::OrderModified(OrderModified {
                    client_order_id: order.client_order_id.clone(),
                    venue_order_id: Some("v-1".to_string().into()),
                    new_qty,
                    new_price,
                    ts: 0,
                })]
            } else {
                Vec::new()
            }
        }
    }

    fn req(coid: &str) -> OrderRequest {
        serde_json::from_value(serde_json::json!({
            "client_order_id": coid, "venue": "test", "symbol": "BTCUSDT",
            "side": 1, "qty": 0.01, "order_type": "limit", "price": 50000.0
        }))
        .unwrap()
    }

    /// Drain everything currently on the ingest lane into a flat `Vec<Event>` (the receiver is a
    /// tokio mpsc; `try_recv` is non-blocking).
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

    #[test]
    fn submit_forwards_events_and_cancel_confirms() {
        let (sender, mut ingest) = event_channel(64);
        let rest = MockRest::default();
        let (tx, cmd_rx) = channel();
        tx.send(ExecCommand::Submit(Box::new(req("c1")))).unwrap();
        tx.send(ExecCommand::Cancel {
            client_order_id: "c1".to_string(),
            intent: CancelIntent::Unspecified,
        })
        .unwrap();
        tx.send(ExecCommand::Shutdown).unwrap();
        // long sentinel + Shutdown already queued → the gap-sentinel never fires here
        run_loop(&rest, &sender, cmd_rx, Vec::<Event>::new, Duration::from_secs(3600));

        assert_eq!(*rest.submitted.lock().unwrap(), vec!["c1".to_string()]);
        assert_eq!(*rest.canceled.lock().unwrap(), vec!["c1".to_string()]);
        let events = drain!(ingest);
        let kinds: Vec<&str> = events
            .iter()
            .map(|e| match e {
                Event::OrderSubmitted(_) => "Submitted",
                Event::OrderAccepted(_) => "Accepted",
                Event::OrderCanceled(_) => "Canceled",
                Event::OrderCancelRejected(_) => "CancelRejected",
                _ => "other",
            })
            .collect();
        assert_eq!(kinds, vec!["Submitted", "Accepted", "Canceled"]);
    }

    #[test]
    fn failed_cancel_surfaces_as_cancel_rejected() {
        let (sender, mut ingest) = event_channel(64);
        let rest = MockRest { cancel_fails: true, ..Default::default() };
        let (tx, cmd_rx) = channel();
        tx.send(ExecCommand::Cancel {
            client_order_id: "c9".to_string(),
            intent: CancelIntent::Unspecified,
        })
        .unwrap();
        tx.send(ExecCommand::Shutdown).unwrap();
        run_loop(&rest, &sender, cmd_rx, Vec::<Event>::new, Duration::from_secs(3600));

        let events = drain!(ingest);
        match events.as_slice() {
            [Event::OrderCancelRejected(e)] => {
                assert_eq!(e.client_order_id, "c9");
                assert_eq!(e.reason, "boom");
            }
            other => panic!("expected one OrderCancelRejected, got {other:?}"),
        }
    }

    /// Modify routing, BOTH arms: a native amend's `[OrderModified]` is forwarded to the ingest;
    /// the `VenueRest` default no-op forwards nothing (the resting order keeps its terms). The
    /// venue-reality ties (`caps_for(venue).supports_modify`) stay in each venue's own tests.
    #[test]
    fn modify_routes_to_modify_order_and_forwards_events() {
        // native amend: `[OrderModified]` reaches the ingest
        let (sender, mut ingest) = event_channel(64);
        let rest = MockRest { native_modify: true, ..Default::default() };
        let (tx, cmd_rx) = channel();
        tx.send(ExecCommand::Modify {
            order: Box::new(req("c1")),
            new_qty: None,
            new_price: Some(51_000.0),
        })
        .unwrap();
        tx.send(ExecCommand::Shutdown).unwrap();
        run_loop(&rest, &sender, cmd_rx, Vec::<Event>::new, Duration::from_secs(3600));

        assert_eq!(*rest.modified.lock().unwrap(), vec![("c1".to_string(), Some(51_000.0))]);
        let events = drain!(ingest);
        match events.as_slice() {
            [Event::OrderModified(e)] => {
                assert_eq!(e.client_order_id, "c1");
                assert_eq!(e.new_price, Some(51_000.0));
            }
            other => panic!("expected one OrderModified, got {other:?}"),
        }

        // default no-op: `modify_order` is still invoked, but nothing reaches the ingest
        let (sender, mut ingest) = event_channel(64);
        let rest = MockRest::default();
        let (tx, cmd_rx) = channel();
        tx.send(ExecCommand::Modify {
            order: Box::new(req("c2")),
            new_qty: None,
            new_price: Some(52_000.0),
        })
        .unwrap();
        tx.send(ExecCommand::Shutdown).unwrap();
        run_loop(&rest, &sender, cmd_rx, Vec::<Event>::new, Duration::from_secs(3600));

        assert_eq!(*rest.modified.lock().unwrap(), vec![("c2".to_string(), Some(52_000.0))]);
        assert!(drain!(ingest).is_empty(), "the default no-op must forward no events");
    }

    /// The gap-sentinel: after a Submit with NO follow-up command, `run_loop` fires the `resync`
    /// closure `fill_sentinel` later and forwards its events — the recovery path for a fill the WS
    /// lost. A normal WS delivery would just make this dedup'd downstream.
    #[test]
    fn sentinel_resyncs_after_idle_submit() {
        use std::sync::atomic::AtomicUsize;
        use vike_model::events::OrderCanceled;

        let (sender, mut ingest) = event_channel(64);
        let rest = MockRest::default();
        let (tx, cmd_rx) = channel();
        let calls = Arc::new(AtomicUsize::new(0));
        let resync = {
            let calls = calls.clone();
            move || {
                calls.fetch_add(1, Ordering::Relaxed);
                // a marker event proves the resync's output is forwarded to the ingest
                vec![Event::OrderCanceled(OrderCanceled {
                    client_order_id: "resync-marker".to_string(),
                    reason: String::new().into(),
                    ts: 0,
                })]
            }
        };

        tx.send(ExecCommand::Submit(Box::new(req("c1")))).unwrap();
        // NB: no follow-up command — the sentinel must fire on its own after `fill_sentinel`.
        let h = std::thread::spawn(move || {
            run_loop(&rest, &sender, cmd_rx, resync, Duration::from_millis(60))
        });
        std::thread::sleep(Duration::from_millis(220)); // > fill_sentinel: the one-shot resync fires
        tx.send(ExecCommand::Shutdown).unwrap();
        h.join().unwrap();

        assert!(calls.load(Ordering::Relaxed) >= 1, "sentinel must resync after an idle submit");
        let events = drain!(ingest);
        assert!(
            events.iter().any(
                |e| matches!(e, Event::OrderCanceled(c) if c.client_order_id == "resync-marker")
            ),
            "resync events must reach the ingest: {events:?}"
        );
    }

    /// One replayed fill, stamped.
    fn fill_at(coid: &str, trade_id: &'static str, ts: i64) -> Event {
        let fill: vike_model::events::FillEvent = serde_json::from_value(serde_json::json!({
            "trade_id": trade_id, "client_order_id": coid, "venue": "test", "symbol": "BTCUSDT",
            "side": 1, "last_qty": 1.0, "last_px": 50000.0, "commission": 0.1,
            "commission_asset": "USDT", "ts": ts
        }))
        .unwrap();
        Event::Fill(fill)
    }

    /// **The restart law** (module doc). The gap-sentinel's `resync` closure replays a window of
    /// VENUE history bounded by a ROW COUNT and by no time at all, and the core's `seen_trade_ids`
    /// dedup is EMPTY in a fresh process — so without a floor, the first firing after a restart
    /// folds a PREVIOUS session's fills through the non-idempotent `Account::apply_fill`
    /// (`balance -= commission`, `realized_pnl +=`). Proven on the CI box: each unexplained equity step
    /// equalled the sum of prior sessions' costs exactly.
    ///
    /// A scripted history closure returns one fill stamped an hour BEFORE this `run_loop`'s spawn
    /// and one stamped an hour after. Only the second may reach the ingest — and the second is the
    /// legitimate case this must not break (a mid-process WS reconnect's gap fills).
    #[test]
    fn history_replay_drops_fills_older_than_the_actor_spawn() {
        let (sender, mut ingest) = event_channel(64);
        let rest = MockRest::default();
        let (tx, cmd_rx) = channel();
        let now = vike_model::clock::now_ms();
        let resync = move || {
            vec![
                // a PREVIOUS session's fill — retained by the venue, never placed by this process
                fill_at("c_prev_session", "e_old", now - 3_600_000),
                // ...and one from THIS process's life (the WS-gap fill the replay exists for)
                fill_at("c_this_session", "e_new", now + 3_600_000),
                // unstamped: never fabricated into a verdict, so it rides through
                Event::OrderCanceled(vike_model::events::OrderCanceled {
                    client_order_id: "c_unstamped".to_string(),
                    reason: String::new().into(),
                    ts: 0,
                }),
            ]
        };

        tx.send(ExecCommand::Submit(Box::new(req("c1")))).unwrap();
        // no follow-up command — the one-shot sentinel fires on its own
        let h = std::thread::spawn(move || {
            run_loop(&rest, &sender, cmd_rx, resync, Duration::from_millis(60))
        });
        std::thread::sleep(Duration::from_millis(220));
        tx.send(ExecCommand::Shutdown).unwrap();
        h.join().unwrap();

        let events = drain!(ingest);
        let trade_ids: Vec<String> = events
            .iter()
            .filter_map(|e| match e {
                Event::Fill(f) => Some(f.trade_id.to_string()),
                _ => None,
            })
            .collect();
        assert!(
            !trade_ids.iter().any(|t| t == "e_old"),
            "a fill stamped BEFORE the actor spawned is a previous session's — it must never \
             reach the ingest, where apply_fill folds its fee and PnL again: {events:?}"
        );
        assert!(
            trade_ids.iter().any(|t| t == "e_new"),
            "a fill stamped after the spawn is the WS-gap recovery this lane exists for and must \
             still be delivered: {events:?}"
        );
        assert!(
            events.iter().any(
                |e| matches!(e, Event::OrderCanceled(c) if c.client_order_id == "c_unstamped")
            ),
            "an UNSTAMPED event carries no evidence of being historical and must ride through \
             (no timestamp is ever fabricated here): {events:?}"
        );
    }

    /// The floor's predicate, directly: only a POSITIVE stamp strictly below the floor is dropped.
    /// The two boundaries are the ones a mutation would flip — `ts == 0` (unstamped) and
    /// `ts == spawn_ms` (the instant itself) both survive.
    #[test]
    fn only_a_positive_stamp_strictly_below_the_floor_is_pre_spawn() {
        let floor = 1_000_000i64;
        assert!(is_pre_spawn(&fill_at("c", "e", floor - 1), floor));
        assert!(!is_pre_spawn(&fill_at("c", "e", floor), floor));
        assert!(!is_pre_spawn(&fill_at("c", "e", floor + 1), floor));
        assert!(!is_pre_spawn(&fill_at("c", "e", 0), floor), "unstamped is not historical");
        // and a floor of 0 (a clock that could not be read) can never drop anything
        assert!(!is_pre_spawn(&fill_at("c", "e", 1), 0));
    }
}

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
