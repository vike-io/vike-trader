//! The RECONCILE round trip: the position seed at connect, the reconnect reconcile, the replay.

use std::collections::HashSet;
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

use prost::Message;

use vike_data::LiveDataSink;
use vike_exec::EventSender;
use vike_model::events::{Event, OrderAccepted};
use vike_model::now_ms;

use super::inbound::{emit_event, on_inbound};
use super::outbound::{
    close_command_coid, forget_close_on_write_failure, mark_new_order_sent, new_order_coid,
    restore_bound_exit_on_write_failure, write_command, write_failure_event,
};
use super::{
    CloseState, Command, OrderIdMap, PositionMap, RECONCILE_TIMEOUT, Reader, SEED_TIMEOUT, send,
};
use crate::positions::{self, ExitKind, OrphanedExit};
use crate::proto::{
    ProtoMessage, ProtoOaOrderListByPositionIdReq, ProtoOaOrderListByPositionIdRes,
    ProtoOaOrderType, ProtoOaReconcileReq, ProtoOaReconcileRes, ProtoOaTradeSide,
    ProtoOaTrendbarPeriod, pt,
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
    match read_reply(reader, buffered, timeout, pt::RECONCILE_RES) {
        Reply::Got(msg) => match ProtoOaReconcileRes::decode(msg.payload.as_deref().unwrap_or(&[]))
        {
            Ok(res) => ReconcileOutcome::Received(res),
            Err(_) => ReconcileOutcome::Rejected,
        },
        Reply::Refused => ReconcileOutcome::Rejected,
        Reply::TimedOut => ReconcileOutcome::TimedOut,
    }
}

/// What waiting for ONE response frame came to ([`read_reply`]).
pub(super) enum Reply {
    /// The frame of the wanted payload type.
    Got(ProtoMessage),
    /// The venue answered with an `ERROR_RES` instead.
    Refused,
    /// The deadline elapsed, or the socket died or failed, before the wanted frame arrived.
    TimedOut,
}

/// [`read_reconcile_res`]'s loop for ANY one response type: read frames until one of payload type `want` arrives
/// (returned undecoded), an `ERROR_RES` arrives, the deadline elapses or the socket closes. Every OTHER frame is
/// BUFFERED for the caller to dispatch afterwards, never dropped (see [`read_reconcile_res`] for why).
pub(super) fn read_reply(
    reader: &mut Reader,
    buffered: &mut Vec<ProtoMessage>,
    timeout: Duration,
    want: u32,
) -> Reply {
    let deadline = Instant::now() + timeout;
    loop {
        if Instant::now() >= deadline {
            return Reply::TimedOut;
        }
        match reader.next_frame() {
            Ok(Some(msg)) if msg.payload_type == want => return Reply::Got(msg),
            Ok(Some(msg)) if msg.payload_type == pt::ERROR_RES => return Reply::Refused,
            Ok(Some(msg)) => buffered.push(msg), // in-flight exec/spot/other frame — buffer, replay
            Ok(None) => {
                if reader.get_mut().eof.load(Ordering::SeqCst) {
                    return Reply::TimedOut; // socket died — next read reconnects
                }
                // read timeout / partial — retry until the deadline
            }
            Err(_) => return Reply::TimedOut,
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

/// Teach the coid→orderId map every coid-bearing PENDING order of a `RECONCILE_RES`, and return the
/// coids it taught. The one writer of that map from a reconcile answer, shared by the INITIAL-connect
/// seed ([`seed_positions_at_connect`]) and the reconnect rebuild ([`reconcile_after_reconnect`]), so
/// a cancel or an amend of a resting order resolves its venue `orderId` after either. It emits
/// nothing: whether to re-announce the orders is the caller's.
fn learn_pending_orders(res: &ProtoOaReconcileRes, orders: &OrderIdMap) -> HashSet<String> {
    let mut present: HashSet<String> = HashSet::new();
    if let Ok(mut map) = orders.lock() {
        for order in &res.order {
            if let Some(coid) = reconcile_order_coid(order) {
                map.insert(coid.clone(), order.order_id);
                present.insert(coid);
            }
        }
    }
    present
}

/// Teach the bound-exit table every resting STOP or LIMIT of a `RECONCILE_RES` that sits on an OPEN
/// position of the book and faces it (a SELL on a long): an exit bound to a live position, placed by
/// an earlier run of this process or by hand. Without the row the bridge could not cancel it when the
/// position later closes, and the venue will not. HEDGED accounts only (the netted path is untouched),
/// and only for an answer about THIS account. Call it AFTER [`rebuild_position_book`], so the position
/// set it checks against is the answer's.
///
/// ⚠ **It adopts only what is provably bound to a LIVE position.** A resting order whose `positionId` is
/// not an open position is either a plain pending order (the venue gives it a new, empty position) or a
/// bound exit that outlived its position: the two are indistinguishable from the answer, so neither is
/// adopted. That second case (a position closed while the bridge was down) is the unsolved restart
/// residual in `crates/bridges/ctrader/CLAUDE.md`'s "A hedged account binds a reduce-only resting exit".
pub(super) fn adopt_bound_exits(
    res: &ProtoOaReconcileRes,
    ctid: i64,
    symbols: &SymbolMap,
    positions: &PositionMap,
) {
    if res.ctid_trader_account_id != ctid || !symbols.account_may_hold_a_hedge() {
        return;
    }
    let Ok(mut book) = positions.lock() else { return };
    for order in &res.order {
        let Some(coid) = reconcile_order_coid(order) else { continue };
        let Some(position_id) = order.position_id else { continue };
        let kind = match ProtoOaOrderType::try_from(order.order_type) {
            Ok(ProtoOaOrderType::Stop) => ExitKind::Stop,
            Ok(ProtoOaOrderType::Limit) => ExitKind::Limit,
            _ => continue,
        };
        let Some(position) = book.position(position_id) else { continue };
        let order_side =
            if order.trade_data.trade_side == ProtoOaTradeSide::Sell as i32 { -1 } else { 1 };
        // `closing_order` is deliberately NOT consulted: the proto says it marks an order that "closes part or
        // whole of a position" and "must have a positionId", which is exactly what a bound exit is, so a venue
        // that sets it would otherwise make this adopt nothing, ever. (A bound stop was measured with it
        // false; a position-attached protection order is the `StopLossTakeProfit` type, skipped above.)
        if position.side == -order_side {
            book.bind_exit(&coid, position_id, kind, order.trade_data.volume);
            book.mark_sent(&coid); // it rests at the venue: a reconnect judges it by the pending list
        }
    }
}

/// After a reconnect's reconcile answer (this account's): forget the bound-exit rows of the orders that
/// were live before the socket died (`pre`: the coids the order map knew) and are not pending now
/// (`present`). Such an exit filled, was cancelled or expired during the gap; its row would still count
/// toward its position's cover, so a re-armed stop would be refused as already covered and the position
/// left naked. A row for an order not yet WRITTEN (a `NewOrder` queued behind the reconnect, `BoundExit::sent`
/// false) is left alone; one that was written and is not pending now went with the socket and loses its row,
/// whether or not the order map ever learned its venue id.
pub(super) fn forget_vanished_exits(
    pre: &HashSet<String>,
    present: &HashSet<String>,
    positions: &PositionMap,
) {
    let Ok(mut book) = positions.lock() else { return };
    for coid in pre.difference(present) {
        book.unbind_exit(coid);
    }
    // And the rows of orders that were WRITTEN but never became known (the socket died between the write and
    // the venue's accept): not in `pre`, not pending, and a row the order map cannot cancel.
    book.forget_sent_exits_not_in(present);
    // A leg of an exit that spans positions whose row is gone rests no more and will never report: it must not hold
    // its exit open (`crate::exit_legs`).
    book.retire_exit_legs_without_rows();
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
/// start returns with a seeded book rather than one that fills in asynchronously. Best-effort
/// throughout, exactly like the reconnect twin: a write failure, a venue rejection or the bounded
/// [`SEED_TIMEOUT`] leaves the book UNFETCHED (the honest state — everything downstream reads
/// that as "unknown") and the start proceeds. Frames that arrive while waiting are BUFFERED and
/// dispatched through the normal inbound path, never dropped.
///
/// # ⚠ What this costs, in milliseconds an operator waits
///
/// This is a BLOCKING round trip added to the start path, and cTrader is the one venue whose exec
/// client connects SYNCHRONOUSLY at start, inside [`crate::adapter::live_runtime_for_account`] (called
/// by [`crate::adapter::CtraderVenueAdapter`], which `crates/vike-tradehub-core/src/engine.rs`'s `make_engine`
/// reaches through the venue-adapter contract), so the number is startup latency, not background
/// work. All three figures are MEASURED, none estimated:
///
/// - **Healthy: 24ms median (23ms min / 25ms max, 15 real starts against
///   `demo.ctraderapi.com:5035`, the CI box 2026-08-08 —
///   `crates/bridges/ctrader/tests/ctrader_seed_smoke.rs` is the harness and reruns it).** For
///   scale, the SAME 15 starts' whole-handshake wall times ranged 353ms–35.3s, dominated by OAuth
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
/// - *Per process*: one seed. `crates/vike-tradehub-core/src/engine.rs`'s `make_engine` builds ONE cTrader
///   engine (`crates/vike-venues/src/venue_wiring.rs`'s `VENUE_WIRING`, the ctrader row), extra symbols ride that same
///   handle as declared legs rather than as second starts, and a DATA connection is excluded
///   outright by the `events.is_some()` gate at the call site.
/// - *Per ACCOUNT*: one seed per started account — `crates/bridges/ctrader/src/config.rs`'s
///   `CtraderConfig::from_credentials` loads exactly one account's grant and nothing loops over a
///   set. A second started account is a second `connect_and_auth_exec`, and it pays its own seed,
///   sequentially; that is a reason to bound this call, and it is bounded.
/// - *Per RECONNECT*: nothing new. [`reconcile_after_reconnect`] has always issued this same
///   `ProtoOAReconcileReq` under [`RECONCILE_TIMEOUT`], on the actor thread, so a reconnect storm
///   costs exactly what it cost before this function existed — and costs it where nobody is
///   blocked.
///
/// Every outcome logs `elapsed_ms`, so the cost is observable in production rather than inferable
/// from this doc.
///
/// It seeds the positions AND the coid→orderId map ([`learn_pending_orders`]), but re-emits no
/// `OrderAccepted`. The map is owed here because a resting order outlives the process that placed it
/// (a bracket's stop-loss and take-profit stay across a restart): without it the restarted process
/// cannot cancel or amend an order that plainly rests, since a cancel keys on the venue `orderId`
/// and the map starts empty. The announcement stays the reconnect path's: it has a BEFORE state to
/// reconcile against, and at initial connect the core would drop it as an unknown coid.
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
) -> Option<ProtoOaReconcileRes> {
    let req = ProtoOaReconcileReq { ctid_trader_account_id: ctid, ..Default::default() };
    // EMPTY clientMsgId, for the reason `reconcile_after_reconnect` spells out: a LATE `ERROR_RES`
    // for this reconcile must not carry a string `on_error_res` would misread as an order's coid.
    if send(reader, pt::RECONCILE_REQ, &req, "").is_err() {
        tracing::warn!(
            target: "ctrader",
            "initial position-seed request write failed; the position book stays UNFETCHED \
             (reduce routing may open a hedge against a pre-existing position — use close_all)"
        );
        return None;
    }

    // `elapsed_ms` on EVERY arm, because this call is SYNCHRONOUS STARTUP COST an operator pays and
    // could not otherwise see: it is the only thing standing between "the start is slow" and "the
    // start is hung". The three numbers it reports are the ones the timeout below is sized against.
    let started = Instant::now();
    let mut buffered: Vec<ProtoMessage> = Vec::new();
    let answer = match read_reconcile_res(reader, &mut buffered, SEED_TIMEOUT) {
        ReconcileOutcome::Received(res) => {
            rebuild_position_book(&res, ctid, positions);
            // An answer for ANOTHER account says nothing about this one's orders (the book rebuild
            // above has logged it): learn nothing from it.
            if res.ctid_trader_account_id == ctid {
                learn_pending_orders(&res, orders);
                adopt_bound_exits(&res, ctid, symbols, positions);
            }
            tracing::info!(
                target: "ctrader",
                positions = res.position.len(),
                pending_orders = res.order.len(),
                elapsed_ms = started.elapsed().as_millis() as u64,
                "position book seeded at connect"
            );
            (res.ctid_trader_account_id == ctid).then_some(res)
        }
        ReconcileOutcome::Rejected => {
            tracing::warn!(
                target: "ctrader",
                elapsed_ms = started.elapsed().as_millis() as u64,
                "initial position seed rejected by the venue (ERROR_RES); the position book stays \
                 UNFETCHED"
            );
            None
        }
        ReconcileOutcome::TimedOut => {
            tracing::warn!(
                target: "ctrader",
                timeout_ms = SEED_TIMEOUT.as_millis() as u64,
                elapsed_ms = started.elapsed().as_millis() as u64,
                "initial position seed not answered before timeout; the position book stays \
                 UNFETCHED"
            );
            None
        }
    };

    for msg in buffered {
        on_inbound(&msg, symbols, money_digits, sink, events, orders, positions, closes);
    }
    answer
}

/// **Read the account's OPEN positions from the venue now, on the live actor socket**, for a `submit`
/// that is waiting on the answer ([`Command::ReadPositions`]); `true` when an answer for THIS account arrived
/// and was folded into the book. It is the connect-time seed's round trip ([`seed_positions_at_connect`])
/// asked again mid-session, and it takes the seed's bound ([`SEED_TIMEOUT`]) for the seed's reason: somebody
/// is blocked on it. Only the POSITIONS are folded ([`rebuild_position_book`], which also orphans the bound
/// exits whose position the answer no longer lists, the actor's next pass cancels them): the orders map and
/// the bound-exit table are the live events' and the reconnect reconcile's, and a read asked for by one
/// order must not rewrite them.
///
/// Why it exists: the book the exec client routes a reduce by is EMPTY and not authoritative after a seed
/// that failed on a start that held a position (a venue `ERROR_RES`, the seed's short bound, a write
/// failure) and after a socket death until a reconcile answers, and a `reduce_only` MARKET order sent plain
/// against an empty book OPENS a hedge on a hedged account (`crates/bridges/ctrader/src/exec.rs`'s `submit`).
/// Frames that arrive while waiting are buffered and dispatched afterwards, as in the seed, never dropped.
/// An answer that is not about this account, a venue refusal, a timeout and a write failure are all `false`.
pub(super) fn read_positions_now(
    reader: &mut Reader,
    ctid: i64,
    symbols: &SymbolMap,
    money_digits: u32,
    sink: &dyn LiveDataSink,
    events: Option<&EventSender>,
    orders: &OrderIdMap,
    positions: &PositionMap,
    closes: &CloseState,
) -> bool {
    let req = ProtoOaReconcileReq { ctid_trader_account_id: ctid, ..Default::default() };
    // EMPTY clientMsgId, for the reason `reconcile_after_reconnect` spells out.
    if send(reader, pt::RECONCILE_REQ, &req, "").is_err() {
        tracing::warn!(target: "ctrader", "positions read: the request could not be written");
        return false;
    }
    let started = Instant::now();
    let mut buffered: Vec<ProtoMessage> = Vec::new();
    let answered = match read_reconcile_res(reader, &mut buffered, SEED_TIMEOUT) {
        ReconcileOutcome::Received(res) => {
            rebuild_position_book(&res, ctid, positions);
            tracing::info!(
                target: "ctrader",
                positions = res.position.len(),
                elapsed_ms = started.elapsed().as_millis() as u64,
                "positions read from the venue for a waiting reduce order"
            );
            res.ctid_trader_account_id == ctid
        }
        ReconcileOutcome::Rejected => {
            tracing::warn!(target: "ctrader", "positions read refused by the venue (ERROR_RES)");
            false
        }
        ReconcileOutcome::TimedOut => {
            tracing::warn!(
                target: "ctrader",
                timeout_ms = SEED_TIMEOUT.as_millis() as u64,
                "positions read not answered before its timeout"
            );
            false
        }
    };
    for msg in buffered {
        on_inbound(&msg, symbols, money_digits, sink, events, orders, positions, closes);
    }
    answered
}

/// How many resting exits one connect looks up (each is one request): a handful is the realistic number, and the
/// look is startup cost on the synchronous start path, so the rest are counted and said, not asked.
const ORPHAN_LOOKUPS_MAX: usize = 16;

/// The window of the second try of a look-up, in milliseconds: `ProtoOAOrderListByPositionIdReq` is asked with NO
/// bounds first (the proto marks both optional), and over the last week if the venue refuses that, the limit its
/// other range reads carry. UNMEASURED: no demo account has answered either form yet.
const ORPHAN_WINDOW_FALLBACK_MS: i64 = 7 * 24 * 3_600_000;

/// **Find the resting exits that outlived their position while the bridge was NOT watching, and SAY SO.** `answer`
/// is the connect-time `RECONCILE_RES` of this account, already folded into the book by
/// [`seed_positions_at_connect`]. A resting STOP or LIMIT that names a position the answer does not list as open is
/// either a plain pending order (the venue gives it a new, empty position) or a bound exit whose position closed
/// while the bridge was down (the UI, a stop that fired, `close_all`); the answer cannot say which, and the second is
/// dangerous: the venue does not cancel a bound order when its position closes another way, and one that reaches
/// the market OPENS a new position under the reused position id (measured on the hedged demo account, 2026-10-26).
/// So each candidate ([`positions::orphan_candidates`]) is looked up with `ProtoOAOrderListByPositionIdReq`: a
/// position whose orders include a FILLED one other than the candidate existed, and is gone
/// ([`positions::proves_a_closed_position`]); that exit is logged at `error`, naming the order, the position and the
/// remedy, and kept in the book ([`positions::PositionBook::orphaned_exits`]) until it ends.
///
/// ⚠ **It NEVER cancels.** An order the bridge did not see placed or closed is its owner's, and the operator's
/// hand cancels it (the cTrader UI, or `recon cancel` on a demo account): the node's reconcile already holds it as
/// an `UnknownOrder` (the restore row for cTrader is `Never`), and this adds the one fact that row cannot carry.
/// ⚠ **A look-up that cannot prove is not a verdict**: an unanswered, refused, truncated (`hasMore`) or unproven
/// look-up leaves the order exactly as it was, a held plain-looking order, and says which. Only an exec start on an
/// account that can hold a hedge, and only against a book read in full, looks at all. Bounded: at most
/// [`ORPHAN_LOOKUPS_MAX`] requests inside [`SEED_TIMEOUT`] in all, so a quiet venue cannot stall the start. Frames
/// that arrive meanwhile are buffered and dispatched afterwards, as in the seed. Returns the exits it flagged.
pub(super) fn flag_orphaned_exits(
    reader: &mut Reader,
    ctid: i64,
    symbols: &SymbolMap,
    money_digits: u32,
    sink: &dyn LiveDataSink,
    events: Option<&EventSender>,
    orders: &OrderIdMap,
    positions: &PositionMap,
    closes: &CloseState,
    answer: &ProtoOaReconcileRes,
) -> Vec<OrphanedExit> {
    if answer.ctid_trader_account_id != ctid || !symbols.account_may_hold_a_hedge() {
        return Vec::new();
    }
    let candidates: Vec<OrphanedExit> = match positions.lock() {
        Ok(book) => positions::orphan_candidates(&answer.order, &book)
            .into_iter()
            .filter_map(OrphanedExit::of_order)
            .collect(),
        Err(_) => return Vec::new(),
    };
    if candidates.is_empty() {
        return Vec::new();
    }
    let started = Instant::now();
    let mut buffered: Vec<ProtoMessage> = Vec::new();
    let mut flagged: Vec<OrphanedExit> = Vec::new();
    let mut unchecked = 0usize;
    for (n, exit) in candidates.into_iter().enumerate() {
        if n >= ORPHAN_LOOKUPS_MAX || started.elapsed() >= SEED_TIMEOUT {
            unchecked += 1;
            continue;
        }
        let symbol = symbols.name_of(exit.symbol_id).unwrap_or("?").to_string();
        match orders_of_position(reader, ctid, exit.position_id, &mut buffered) {
            Some(listed) if positions::proves_a_closed_position(&listed.order, exit.order_id) => {
                tracing::error!(
                    target: "ctrader",
                    order_id = exit.order_id,
                    coid = ?exit.coid,
                    position_id = exit.position_id,
                    symbol = %symbol,
                    side = exit.side,
                    kind = ?exit.kind,
                    volume_units = exit.volume as f64 / 100.0,
                    price = ?exit.price,
                    "ORPHANED EXIT: a resting order is bound to a position that is NOT open any more (it \
                     closed while this bridge was not watching), and if the market reaches the order it \
                     WILL OPEN a position. The bridge does NOT cancel it by itself: cancel it by hand \
                     (the cTrader platform, or `recon cancel` on a demo account)"
                );
                flagged.push(exit);
            }
            Some(listed) if !listed.has_more => tracing::info!(
                target: "ctrader",
                order_id = exit.order_id,
                position_id = exit.position_id,
                symbol = %symbol,
                "a resting order names a position that is not open and never had a fill: a plain pending \
                 order, left alone"
            ),
            _ => tracing::warn!(
                target: "ctrader",
                order_id = exit.order_id,
                position_id = exit.position_id,
                symbol = %symbol,
                "a resting order names a position that is not open and the venue's orders of that \
                 position could not tell whether it was ever filled: it may be an exit that outlived \
                 its position (it would OPEN a position if hit), left as it is"
            ),
        }
    }
    if unchecked > 0 {
        tracing::warn!(
            target: "ctrader",
            unchecked,
            "resting orders naming a position that is not open were NOT looked up (the connect-time \
             bound): some may be exits that outlived their position"
        );
    }
    if let Ok(mut book) = positions.lock() {
        book.record_orphaned_exits(flagged.clone());
    }
    for msg in buffered {
        on_inbound(&msg, symbols, money_digits, sink, events, orders, positions, closes);
    }
    flagged
}

/// The orders the venue lists for ONE position, or `None` when it could not be had: the request not written, the
/// venue silent, a decode failure, an answer for another account, or an `ERROR_RES` to both forms of the request
/// (without bounds, then over [`ORPHAN_WINDOW_FALLBACK_MS`]). One empty `clientMsgId` for the reason the seed spells out.
fn orders_of_position(
    reader: &mut Reader,
    ctid: i64,
    position_id: i64,
    buffered: &mut Vec<ProtoMessage>,
) -> Option<ProtoOaOrderListByPositionIdRes> {
    let now = now_ms();
    for window in [None, Some((now - ORPHAN_WINDOW_FALLBACK_MS, now))] {
        let req = ProtoOaOrderListByPositionIdReq {
            ctid_trader_account_id: ctid,
            position_id,
            from_timestamp: window.map(|(from, _)| from),
            to_timestamp: window.map(|(_, to)| to),
            ..Default::default()
        };
        if send(reader, pt::ORDER_LIST_BY_POSITION_ID_REQ, &req, "").is_err() {
            return None;
        }
        match read_reply(reader, buffered, SEED_TIMEOUT, pt::ORDER_LIST_BY_POSITION_ID_RES) {
            Reply::Got(msg) => {
                // Which form of the request the venue answers is NOT measured yet: say it, once per look-up.
                tracing::info!(
                    target: "ctrader",
                    position_id,
                    bounded = window.is_some(),
                    "the venue answered the order look-up of a position"
                );
                return ProtoOaOrderListByPositionIdRes::decode(
                    msg.payload.as_deref().unwrap_or(&[]),
                )
                .ok()
                .filter(|res| res.ctid_trader_account_id == ctid);
            }
            Reply::Refused => continue, // the other form of the request
            Reply::TimedOut => return None,
        }
    }
    None
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
        let present = learn_pending_orders(&res, orders);

        // 2. Re-emit OrderAccepted for each still-pending order (idempotent — never a fill).
        if let Some(events) = events {
            for order in &res.order {
                let Some(coid) = reconcile_order_coid(order) else { continue };
                let accepted = Event::OrderAccepted(OrderAccepted {
                    client_order_id: coid,
                    venue_order_id: Some(order.order_id.to_string().into()),
                    ts: order.utc_last_update_timestamp.unwrap_or_else(now_ms),
                });
                emit_event(events, positions, accepted);
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
        // 3b. Exits resting on a position that is still open are bound exits of it (see the fn doc);
        //     the rebuild above already orphaned the bound exits whose position the answer no longer
        //     lists, and the actor cancels them on its next pass.
        adopt_bound_exits(&res, ctid, symbols, positions);

        // 4. Coids tracked before the reconnect but no longer pending: their bound-exit rows go (an answer
        //    for ANOTHER account says nothing about this one's orders), and each is logged as a possible gap.
        if res.ctid_trader_account_id == ctid {
            forget_vanished_exits(&pre, &present, positions);
        }
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
        let new_order = new_order_coid(&cmd);
        if write_command(reader, ctid, cmd).is_ok() {
            mark_new_order_sent(&new_order, positions);
        } else {
            forget_close_on_write_failure(&close_coid, closes);
            restore_bound_exit_on_write_failure(&on_write_failure, positions);
            if let (Some(event), Some(events)) = (on_write_failure, events) {
                emit_event(events, positions, event);
            }
        }
    }
}
