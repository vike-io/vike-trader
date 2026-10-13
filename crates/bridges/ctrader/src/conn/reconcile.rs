//! The RECONCILE round trip: the position seed at connect, the reconnect reconcile, the replay.

use std::collections::HashSet;
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

use prost::Message;

use vike_data::LiveDataSink;
use vike_exec::EventSender;
use vike_model::events::{Event, OrderAccepted};
use vike_model::now_ms;

use super::inbound::on_inbound;
use super::outbound::{
    close_command_coid, forget_close_on_write_failure, write_command, write_failure_event,
};
use super::{
    CloseState, Command, OrderIdMap, PositionMap, RECONCILE_TIMEOUT, Reader, SEED_TIMEOUT, send,
};
use crate::positions;
use crate::proto::{
    ProtoMessage, ProtoOaReconcileReq, ProtoOaReconcileRes, ProtoOaTrendbarPeriod, pt,
};
use crate::symbols::SymbolMap;

#[cfg(doc)]
use super::inbound::update_order_map;
#[cfg(doc)]
use super::session::reconnect_with_backoff;

/// The result of waiting for a `RECONCILE_RES` in [`read_reconcile_res`]. Distinguished so the
/// caller can log a NETWORK stall (`TimedOut`, includes socket-death) apart from a venue-side
/// reconcile REJECTION (`Rejected`, an `ERROR_RES`) — different triage (F3 fix 4).
pub(super) enum ReconcileOutcome {
    /// The venue answered with a decodable `RECONCILE_RES`.
    Received(ProtoOaReconcileRes),
    /// The venue answered with an `ERROR_RES` (reconcile rejected), or a `RECONCILE_RES` that
    /// failed to decode.
    Rejected,
    /// The deadline elapsed / the socket died before any reconcile response arrived.
    TimedOut,
}

/// Read frames until a `RECONCILE_RES` arrives (decoded + returned), the deadline elapses, an
/// `ERROR_RES` arrives, or the socket closes. Every OTHER frame read while waiting — critically an
/// `EXECUTION_EVENT` (cTrader pushes fills/accepts/cancels UNCONDITIONALLY once authorized, NOT
/// subscription-gated, so a real fill can land inside this ≤5s window) but also any spot/other
/// frame — is BUFFERED into `buffered` (not dropped) so the caller can dispatch it through the
/// normal inbound path after the reconcile resolves; dropping it would silently diverge `Account`/
/// the OMS ("no order silently vanishes"). Never propagates an error — reconcile is best-effort.
pub(super) fn read_reconcile_res(
    reader: &mut Reader,
    buffered: &mut Vec<ProtoMessage>,
    timeout: Duration,
) -> ReconcileOutcome {
    let deadline = Instant::now() + timeout;
    loop {
        if Instant::now() >= deadline {
            return ReconcileOutcome::TimedOut;
        }
        match reader.next_frame() {
            Ok(Some(msg)) if msg.payload_type == pt::RECONCILE_RES => {
                return match ProtoOaReconcileRes::decode(msg.payload.as_deref().unwrap_or(&[])) {
                    Ok(res) => ReconcileOutcome::Received(res),
                    Err(_) => ReconcileOutcome::Rejected,
                };
            }
            Ok(Some(msg)) if msg.payload_type == pt::ERROR_RES => {
                return ReconcileOutcome::Rejected;
            }
            Ok(Some(msg)) => buffered.push(msg), // in-flight exec/spot/other frame — buffer, replay
            Ok(None) => {
                if reader.get_mut().eof.load(Ordering::SeqCst) {
                    return ReconcileOutcome::TimedOut; // socket died — next read reconnects
                }
                // read timeout / partial — retry until the deadline
            }
            Err(_) => return ReconcileOutcome::TimedOut,
        }
    }
}

/// Pull the referenced order's correlation id (its dedicated `client_order_id`, falling back to the
/// `label` we set at submit time), or `None` when neither is present/non-empty. Mirrors
/// [`update_order_map`]'s key derivation so the reconcile-rebuilt map keys match the live path.
fn reconcile_order_coid(order: &crate::proto::ProtoOaOrder) -> Option<String> {
    order
        .client_order_id
        .clone()
        .or_else(|| order.trade_data.label.clone())
        .filter(|c| !c.is_empty())
}

/// Fold a `RECONCILE_RES`'s OPEN-position set into the shared book — marking it FETCHED **only if
/// it is about THIS account and every row was understood**. The one writer of
/// [`crate::positions::PositionBook::replace_all`], shared by the INITIAL-connect seed
/// ([`seed_positions_at_connect`]) and the reconnect rebuild ([`reconcile_after_reconnect`]) so the
/// two cannot disagree about what a reconcile answer means.
///
/// ⚠ **`ctid` is checked FIRST, and its absence was a live defect.** `ProtoOAReconcileRes` carries
/// the `ctidTraderAccountId` it answers for, and nothing compared it. An answer for ANOTHER account
/// was folded in wholesale — replacing this account's routing entries with a stranger's, and
/// marking the result AUTHORITATIVE, so a stranger's flat book became this account's `Flat` and
/// refused its exit under `halt_admit = "verify"`
/// (`crates/bridges/ctrader/tests/exec_halt.rs`'s
/// `a_reconcile_answer_for_another_account_is_not_this_accounts_evidence` measured exactly that).
/// A mismatched answer therefore changes NOTHING except to `invalidate` the evidence flag: we
/// asked, and what came back was not about us, so whatever we believed is now of unknown standing.
///
/// ⚠ **The `unreadable` arm is not defensive decoration — it is the difference between evidence and
/// a manufactured fact.** A row this build cannot classify
/// (`crates/bridges/ctrader/src/positions.rs`'s `reconcile_rows`: an ERROR-status position, a
/// `positionStatus` the vendored proto has no variant for, an OPEN row with a non-positive volume)
/// is a HOLE, and a hole is indistinguishable from an absence once it is dropped. So the entries are
/// taken either way — routing runs on the best guess available, exactly as after an
/// [`crate::positions::PositionBook::invalidate`] — and the book declines to call itself evidence,
/// which lands at the halt boundary as `Unknown` and ADMITS.
///
/// ⚠ **Neither arm can see a row the venue never SENT**, and that is why the halt boundary does not
/// rest on this function alone: `PositionBook::unauthoritative_for` additionally demands POSITIVE
/// coverage of the symbol being judged. Everything here is about the provenance of an answer; only
/// coverage is about what the answer actually said.
pub(super) fn rebuild_position_book(res: &ProtoOaReconcileRes, ctid: i64, positions: &PositionMap) {
    if res.ctid_trader_account_id != ctid {
        if let Ok(mut book) = positions.lock() {
            book.invalidate();
        }
        tracing::error!(
            target: "ctrader",
            expected_ctid = ctid,
            answered_for = res.ctid_trader_account_id,
            positions = res.position.len(),
            "a RECONCILE_RES arrived for a DIFFERENT ctidTraderAccountId; discarding it and \
             clearing this book's evidence flag — another account's positions (or its emptiness) \
             are not this account's"
        );
        return;
    }
    let (tracked, unreadable) = positions::reconcile_rows(&res.position);
    let Ok(mut book) = positions.lock() else { return };
    if unreadable == 0 {
        book.replace_all(tracked, now_ms());
        return;
    }
    let kept = tracked.len();
    book.replace_unverified(tracked);
    tracing::warn!(
        target: "ctrader",
        reported = res.position.len(),
        tracked = kept,
        unreadable,
        "a reconcile answer carried position rows this build could not classify; the position \
         book is REPLACED for routing but stays UNVERIFIED, so a halt under halt_admit=verify \
         will ADMIT rather than read the gap as a flat account"
    );
}

/// Ask the venue for its OPEN positions at INITIAL connect and seed the book with the answer.
///
/// ⚠ **This did not exist, and its absence was a live bug.** `ProtoOAReconcileReq` was issued only
/// on RECONNECT, so a freshly-mounted process started with an EMPTY position map and — on a process
/// that neither trades nor loses its socket — kept it empty forever. Against an empty map
/// [`crate::positions::plan_reduce`] sees no opposing exposure and routes a flatten as a NEW ORDER,
/// stacking a hedge on a hedging account; `crates/bridges/ctrader/src/exec.rs`'s `close_all` exists
/// precisely as the manual workaround for the book that resulted. Seeding here fixes the routing
/// bug on its own, independently of any halt policy.
///
/// It also makes "flat" a FACT rather than an absence, which is what a position-verified halt admit
/// needs (`crates/bridges/ctrader/src/positions.rs`'s `PositionBook`).
///
/// Runs **before the actor thread starts**, on the handshake's own socket and its own reader, so a
/// mount returns with a seeded book rather than one that fills in asynchronously. Best-effort
/// throughout, exactly like the reconnect twin: a write failure, a venue rejection or the bounded
/// [`SEED_TIMEOUT`] leaves the book UNFETCHED (the honest state — everything downstream reads
/// that as "unknown") and the mount proceeds. Frames that arrive while waiting are BUFFERED and
/// dispatched through the normal inbound path, never dropped.
///
/// # ⚠ What this costs, in milliseconds an operator waits
///
/// This is a BLOCKING round trip added to the mount path, and cTrader is the one venue whose exec
/// client connects SYNCHRONOUSLY at mount, inside [`crate::mount::live_mount_for_account`] (called
/// by [`crate::mount::CtraderVenueMount`], which `crates/vike-mount/src/engine.rs`'s `make_engine`
/// reaches through the venue mount contract), so the number is startup latency, not background
/// work. All three figures are MEASURED, none estimated:
///
/// - **Healthy: 24ms median (23ms min / 25ms max, 15 real mounts against
///   `demo.ctraderapi.com:5035`, the CI box 2026-08-08 —
///   `crates/bridges/ctrader/tests/ctrader_seed_smoke.rs` is the harness and reruns it).** For
///   scale, the SAME 15 mounts' whole-handshake wall times ranged 353ms–35.3s, dominated by OAuth
///   and the symbol-list download; the seed is a rounding error beside the connect it rides on.
/// - **Mute venue (accepts, handshakes, then never answers this one request): 2.08s measured**
///   (2.030s / 2.077s / 2.080s) — [`SEED_TIMEOUT`] rounded up to the socket's 1s read granularity,
///   which that constant's doc explains. That is the honest worst case, and the reason the bound is
///   its own constant rather than the reconnect twin's: measured under [`RECONCILE_TIMEOUT`]
///   instead, the identical stall was **5.120s**.
/// - **Every OTHER failure is fast, not bounded-slow**: a write failure and a venue `ERROR_RES`
///   both return on the spot, and a dead socket returns at EOF rather than at the deadline
///   ([`read_reconcile_res`]'s `Ok(None)` + `eof` arm).
///
/// **It does NOT stack, on either axis.**
/// - *Per process*: one seed. `crates/vike-mount/src/engine.rs`'s `make_engine` builds ONE cTrader
///   engine (`crates/vike-tradehub/src/wired_markets.rs`'s `CTRADER_MARKET`), extra symbols ride that same
///   handle as declared legs rather than as second mounts, and a DATA connection is excluded
///   outright by the `events.is_some()` gate at the call site.
/// - *Per ACCOUNT*: one seed per mounted account — `crates/bridges/ctrader/src/config.rs`'s
///   `CtraderConfig::from_credentials` loads exactly one account's grant and nothing loops over a
///   set. A second mounted account is a second `connect_and_auth_exec`, and it pays its own seed,
///   sequentially; that is a reason to bound this call, and it is bounded.
/// - *Per RECONNECT*: nothing new. [`reconcile_after_reconnect`] has always issued this same
///   `ProtoOAReconcileReq` under [`RECONCILE_TIMEOUT`], on the actor thread, so a reconnect storm
///   costs exactly what it cost before this function existed — and costs it where nobody is
///   blocked.
///
/// Every outcome logs `elapsed_ms`, so the cost is observable in production rather than inferable
/// from this doc.
///
/// Deliberately seeds positions ONLY — not the coid→orderId map, and it re-emits no `OrderAccepted`.
/// At initial connect the core tracks no orders, so both would be dropped as unknown coids; the
/// reconnect path owns those because it has a BEFORE state to reconcile against.
pub(super) fn seed_positions_at_connect(
    reader: &mut Reader,
    ctid: i64,
    symbols: &SymbolMap,
    money_digits: u32,
    sink: &dyn LiveDataSink,
    events: Option<&EventSender>,
    orders: &OrderIdMap,
    positions: &PositionMap,
    closes: &CloseState,
) {
    let req = ProtoOaReconcileReq { ctid_trader_account_id: ctid, ..Default::default() };
    // EMPTY clientMsgId, for the reason `reconcile_after_reconnect` spells out: a LATE `ERROR_RES`
    // for this reconcile must not carry a string `on_error_res` would misread as an order's coid.
    if send(reader, pt::RECONCILE_REQ, &req, "").is_err() {
        tracing::warn!(
            target: "ctrader",
            "initial position-seed request write failed; the position book stays UNFETCHED \
             (reduce routing may open a hedge against a pre-existing position — use close_all)"
        );
        return;
    }

    // `elapsed_ms` on EVERY arm, because this call is SYNCHRONOUS STARTUP COST an operator pays and
    // could not otherwise see: it is the only thing standing between "the mount is slow" and "the
    // mount is hung". The three numbers it reports are the ones the timeout below is sized against.
    let started = Instant::now();
    let mut buffered: Vec<ProtoMessage> = Vec::new();
    match read_reconcile_res(reader, &mut buffered, SEED_TIMEOUT) {
        ReconcileOutcome::Received(res) => {
            rebuild_position_book(&res, ctid, positions);
            tracing::info!(
                target: "ctrader",
                positions = res.position.len(),
                elapsed_ms = started.elapsed().as_millis() as u64,
                "position book seeded at connect"
            );
        }
        ReconcileOutcome::Rejected => tracing::warn!(
            target: "ctrader",
            elapsed_ms = started.elapsed().as_millis() as u64,
            "initial position seed rejected by the venue (ERROR_RES); the position book stays \
             UNFETCHED"
        ),
        ReconcileOutcome::TimedOut => tracing::warn!(
            target: "ctrader",
            timeout_ms = SEED_TIMEOUT.as_millis() as u64,
            elapsed_ms = started.elapsed().as_millis() as u64,
            "initial position seed not answered before timeout; the position book stays UNFETCHED"
        ),
    }

    for msg in buffered {
        on_inbound(&msg, symbols, money_digits, sink, events, orders, positions, closes);
    }
}

/// F3 (reconcile exec state on reconnect). Right after a reconnect+reauth — BEFORE subscriptions
/// replay — ask the venue for its current OPEN positions + PENDING orders (`ProtoOAReconcileReq`)
/// and, from the answer:
///   1. **Rebuild the coid→orderId map** from the pending orders, so post-reconnect cancel/modify
///      (which key on the numeric `orderId`) resolve again — the concretely-valuable, unambiguously
///      -safe fix.
///   2. **Re-emit an `OrderAccepted`** (NOT a fill) for each still-pending order. This is SAFE
///      because `OrderAccepted` never folds into `Account` (no trades/PnL under any path in the core
///      engine's `on_event`): a duplicate on an order the core still tracks is dropped by the FSM
///      (accept is legal only from SUBMITTED), and one for an order the core lost is dropped as an
///      unknown coid — neither double-counts anything. It re-establishes a live order the core may
///      have lost during the dead window; harmless if already known.
///   3. **Gap observability only**: for any coid the adapter tracked BEFORE the reconnect that is
///      ABSENT from the pending set, log ONE `warn` (a possible gap fill/cancel). We do NOT
///      synthesize a terminal (`OrderFilled`/`OrderCanceled`) — reconcile alone cannot tell fill
///      from cancel, and a wrong terminal is worse than a logged gap. Full deal-history recovery is
///      out of scope.
///
/// Best-effort throughout: a write/response failure logs once and returns; the caller still replays
/// subscriptions.
pub(super) fn reconcile_after_reconnect(
    reader: &mut Reader,
    ctid: i64,
    symbols: &SymbolMap,
    money_digits: u32,
    sink: &dyn LiveDataSink,
    orders: &OrderIdMap,
    positions: &PositionMap,
    closes: &CloseState,
    events: Option<&EventSender>,
) {
    // Snapshot the coids the adapter knew BEFORE the reconnect (for the gap check below). Terminal
    // coids are pruned from the map as their FILLED/CANCELLED/REJECTED/EXPIRED events arrive
    // (`update_order_map`), so this set holds only still-live orders — the gap-diff below is
    // therefore actionable, not spammed by historically-completed coids.
    let pre: HashSet<String> =
        orders.lock().map(|m| m.keys().cloned().collect()).unwrap_or_default();

    let req = ProtoOaReconcileReq { ctid_trader_account_id: ctid, ..Default::default() };
    // Non-order request → EMPTY clientMsgId (F2b invariant, see `write_command`'s doc): a late
    // `ERROR_RES` for this reconcile — one arriving AFTER our timeout, so it falls through to
    // `on_error_res` on the main loop — must not carry a coid that `on_error_res` would misread as a
    // real order's, fabricating a phantom `OrderRejected{client_order_id:"recon"}`.
    if send(reader, pt::RECONCILE_REQ, &req, "").is_err() {
        tracing::warn!(
            target: "ctrader",
            "reconcile request write failed; skipping reconcile (subscriptions still replay)"
        );
        return;
    }

    // Buffer any non-reconcile frame read while waiting (in-flight EXECUTION_EVENTs, spot, …); they
    // are dispatched through the normal inbound path AFTER the reconcile resolves, below — never
    // dropped.
    let mut buffered: Vec<ProtoMessage> = Vec::new();
    let res = match read_reconcile_res(reader, &mut buffered, RECONCILE_TIMEOUT) {
        ReconcileOutcome::Received(res) => Some(res),
        ReconcileOutcome::Rejected => {
            tracing::warn!(
                target: "ctrader",
                "reconcile rejected by the venue (ERROR_RES); skipping reconcile \
                 (subscriptions still replay)"
            );
            None
        }
        ReconcileOutcome::TimedOut => {
            tracing::warn!(
                target: "ctrader",
                timeout_ms = RECONCILE_TIMEOUT.as_millis() as u64,
                "reconcile response not received before timeout (network stall / socket death); \
                 skipping reconcile (subscriptions still replay)"
            );
            None
        }
    };

    if let Some(res) = res {
        // 1. Rebuild the coid→orderId correlation map from the pending orders.
        let mut present: HashSet<String> = HashSet::new();
        if let Ok(mut map) = orders.lock() {
            for order in &res.order {
                if let Some(coid) = reconcile_order_coid(order) {
                    map.insert(coid.clone(), order.order_id);
                    present.insert(coid);
                }
            }
        }

        // 2. Re-emit OrderAccepted for each still-pending order (idempotent — never a fill).
        if let Some(events) = events {
            for order in &res.order {
                let Some(coid) = reconcile_order_coid(order) else { continue };
                let accepted = Event::OrderAccepted(OrderAccepted {
                    client_order_id: coid,
                    venue_order_id: Some(order.order_id.to_string().into()),
                    ts: order.utc_last_update_timestamp.unwrap_or_else(now_ms),
                });
                if events.blocking_send(accepted).is_err() {
                    tracing::warn!(
                        target: "ctrader",
                        "core ingest gone; dropping reconcile re-establish event"
                    );
                }
            }
        }

        // 3. Rebuild the OPEN-position map from the reconcile's authoritative position set, so the
        //    exec client's reduce routing survives a reconnect (an open the adapter learned before
        //    the outage would otherwise be forgotten, misrouting a post-reconnect flatten as an
        //    open). The reconcile lists ONLY open positions, so replacing the set wholesale is
        //    correct — anything closed during the dead window is absent and thus dropped. This also
        //    RE-VALIDATES the book as evidence (`fetched_at_ms`), which the reconnect invalidated;
        //    a reconcile that never answered leaves it invalidated, which is the honest state.
        rebuild_position_book(&res, ctid, positions);

        // 4. Observability-only gap check: coids tracked before the reconnect but no longer pending.
        for coid in pre.difference(&present) {
            tracing::warn!(
                target: "ctrader",
                coid = %coid,
                "reconcile: previously-tracked order absent from the venue's pending set on \
                 reconnect; possible fill/cancel during the dead window (no terminal synthesized — \
                 deal-history recovery is out of scope)"
            );
        }
        tracing::info!(
            target: "ctrader",
            pending = res.order.len(),
            positions = res.position.len(),
            "reconcile complete on reconnect"
        );
    }

    // Dispatch every buffered in-flight frame through the SAME inbound handler the main actor loop
    // uses, IN ARRIVAL ORDER — so a fill/accept/cancel (or spot) that landed during the reconcile
    // window is processed normally (folded into the ingest lane / sink) rather than lost. Runs
    // regardless of the reconcile outcome above (a fill can arrive even if the reconcile itself
    // times out or is rejected).
    for msg in buffered {
        on_inbound(&msg, symbols, money_digits, sink, events, orders, positions, closes);
    }
}

/// After a successful reconnect+reauth, reconcile exec state (F3, [`reconcile_after_reconnect`])
/// then re-issue every currently-active spot/trendbar subscription (cTrader does not remember
/// subscriptions across a dropped socket) and flush any commands that
/// arrived from `rx` while the actor was reconnecting (buffered by [`reconnect_with_backoff`], in
/// arrival order). A failed SUBSCRIBE replay is swallowed — if the fresh connection is ALSO already
/// broken, the next read hits an error/EOF and triggers another reconnect naturally. But a failed
/// ORDER-command replay must NOT vanish: it synthesizes the same terminal/non-terminal reject the
/// main drain loop does (via [`write_failure_event`]) so the venue-adapter contract still holds.
#[expect(clippy::too_many_arguments)]
pub(super) fn replay_after_reconnect(
    reader: &mut Reader,
    ctid: i64,
    symbols: &SymbolMap,
    money_digits: u32,
    sink: &dyn LiveDataSink,
    spot_subs: &HashSet<i64>,
    trendbar_subs: &HashSet<(i64, ProtoOaTrendbarPeriod)>,
    pending: Vec<Command>,
    events: Option<&EventSender>,
    orders: &OrderIdMap,
    positions: &PositionMap,
    closes: &CloseState,
) {
    // Reconcile FIRST — restore the coid→orderId map + re-establish pending orders before any new
    // subscription frame can interleave (and before a replayed order command needs the map).
    reconcile_after_reconnect(
        reader,
        ctid,
        symbols,
        money_digits,
        sink,
        orders,
        positions,
        closes,
        events,
    );
    for &symbol_id in spot_subs {
        let _ = write_command(reader, ctid, Command::SubscribeSpots { symbol_id });
    }
    for &(symbol_id, period) in trendbar_subs {
        let _ = write_command(reader, ctid, Command::SubscribeTrendbar { symbol_id, period });
    }
    for cmd in pending {
        // Captured BEFORE `cmd` is consumed by `write_command` (mirrors the main drain loop).
        let on_write_failure = write_failure_event(&cmd, closes);
        let close_coid = close_command_coid(&cmd);
        if write_command(reader, ctid, cmd).is_err() {
            forget_close_on_write_failure(&close_coid, closes);
            if let (Some(event), Some(events)) = (on_write_failure, events)
                && events.blocking_send(event).is_err()
            {
                tracing::warn!(
                    target: "ctrader",
                    "core ingest gone; dropping synthesized reject on replay"
                );
            }
        }
    }
}
