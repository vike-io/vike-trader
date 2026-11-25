//! The RECONCILE round trip: the position seed at connect, the reconnect reconcile, the replay.

use std::collections::{HashMap, HashSet};
use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use prost::Message;

use vike_bridge_core::execution_client::order_client_id::OrderVenueIdRegistry;
use vike_data::DataClientSink;
use vike_execution::EventSender;
use vike_model::events::{Event, OrderAccepted};
use vike_model::now_ms;

use super::inbound::{close_terminal_event, emit_event, on_inbound};
use super::outbound::{
    book_write, close_command_order_client_id, new_order_order_client_id, resubscribe,
    track_subscription, write_command, write_failure_event,
};
use super::{
    CloseState, ConnectionCommand, PositionsByIdShared, RECONCILE_TIMEOUT, Reader, SEED_TIMEOUT,
    send,
};
use crate::execution::positions::{self, OrphanedExit, StrandedClose};
use crate::instrument::symbols::SymbolMap;
use crate::transport::proto::{
    ProtoMessage, ProtoOaDeal, ProtoOaDealListReq, ProtoOaDealListRes, ProtoOaExecutionEvent,
    ProtoOaExecutionType, ProtoOaOrder, ProtoOaOrderListByPositionIdReq,
    ProtoOaOrderListByPositionIdRes, ProtoOaOrderListReq, ProtoOaOrderListRes, ProtoOaOrderStatus,
    ProtoOaOrderType, ProtoOaReconcileReq, ProtoOaReconcileRes, ProtoOaTradeSide,
    ProtoOaTrendbarPeriod, pt,
};
use vike_bridge_core::exit::ExitKind;

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
            // Every request read here carries an EMPTY clientMsgId, so an `ERROR_RES` with an id is the
            // answer to an ORDER written before it, and is that order's, or the actor's probe's
            // (`super::session::PROBE_MESSAGE_ID`): it goes back to the caller's dispatch.
            Ok(Some(msg))
                if msg.payload_type == pt::ERROR_RES
                    && msg.client_msg_id.as_deref().is_none_or(str::is_empty) =>
            {
                return Reply::Refused;
            }
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

/// Pull the referenced order's correlation id (its dedicated `order_client_id`, falling back to the
/// `label` we set at submit time), or `None` when neither is present/non-empty. Mirrors
/// [`update_order_map`]'s key derivation so the reconcile-rebuilt map keys match the live path.
fn reconcile_order_order_client_id(
    order: &crate::transport::proto::ProtoOaOrder,
) -> Option<String> {
    order
        .client_order_id
        .clone()
        .or_else(|| order.trade_data.label.clone())
        .filter(|c| !c.is_empty())
}

/// The order client id of a PENDING order the NODE placed: `crate::feed::decode` writes its order client id into both `clientOrderId` and
/// `label`, and the venue echoes both (measured on the demo 2026-11-06, `crates/bridges/ctrader/CLAUDE.md`). A label typed
/// in the platform is not one, and neither is the id the platform gives its own request: such an order is its owner's.
fn node_placed_order_client_id(order: &crate::transport::proto::ProtoOaOrder) -> Option<String> {
    let order_client_id = order.client_order_id.as_deref().filter(|c| !c.is_empty())?;
    (order.trade_data.label.as_deref() == Some(order_client_id))
        .then(|| order_client_id.to_string())
}

/// Teach the order client id→orderId map every order-client-id-bearing PENDING order of a `RECONCILE_RES`, and return the
/// order client ids it taught. The one writer of that map from a reconcile answer, shared by the INITIAL-connect
/// seed ([`seed_positions_at_connect`]) and the reconnect rebuild ([`reconcile_after_reconnect`]), so
/// a cancel or an amend of a resting order resolves its venue `orderId` after either. It emits
/// nothing: whether to re-announce the orders is the caller's.
fn learn_pending_orders(
    res: &ProtoOaReconcileRes,
    orders: &Arc<Mutex<OrderVenueIdRegistry<i64>>>,
) -> HashSet<String> {
    let mut present: HashSet<String> = HashSet::new();
    if let Ok(mut map) = orders.lock() {
        for order in &res.order {
            if let Some(order_client_id) = reconcile_order_order_client_id(order) {
                map.bind(&order_client_id, order.order_id);
                present.insert(order_client_id);
            }
        }
    }
    present
}

/// Teach the bound-exit table every resting STOP, STOP_LIMIT or LIMIT of a `RECONCILE_RES` that sits on an OPEN
/// position of the book and faces it (a SELL on a long): an exit bound to a live position, placed by
/// an earlier run of this process. Without the row the bridge could not cancel it when the position
/// later closes, and the venue will not. HEDGED accounts only (the netted path is untouched), and only
/// for an answer about THIS account. Call it AFTER [`rebuild_position_book`], so the position set it
/// checks against is the answer's.
///
/// ⚠ **Only the node's own order ([`node_placed_order_client_id`]):** a row here is cancelled when its position
/// closes, and a foreign order is cancelled by hand only, on any venue.
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
    positions: &PositionsByIdShared,
) {
    if res.ctid_trader_account_id != ctid || !symbols.account_may_hold_a_hedge() {
        return;
    }
    let Ok(mut book) = positions.lock() else { return };
    for order in &res.order {
        let Some(order_client_id) = node_placed_order_client_id(order) else { continue };
        let Some(position_id) = order.position_id else { continue };
        let kind = match ProtoOaOrderType::try_from(order.order_type) {
            // A stop-limit binds as the position's stop (`ExecutionClientCtrader::submit`), so it is adopted as one.
            Ok(ProtoOaOrderType::Stop | ProtoOaOrderType::StopLimit) => ExitKind::Stop,
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
            book.bind_exit(&order_client_id, position_id, kind, order.trade_data.volume);
            book.mark_sent(&order_client_id); // it rests at the venue: a reconnect judges it by the pending list
        }
    }
}

/// After a reconnect's reconcile answer (this account's): forget the bound-exit rows of the orders that
/// were live before the socket died (`pre`: the order client ids the order map knew) and are not pending now
/// (`present`). Such an exit filled, was cancelled or expired during the gap; its row would still count
/// toward its position's cover, so a re-armed stop would be refused as already covered and the position
/// left naked. A row for an order not yet WRITTEN (a `NewOrder` queued behind the reconnect, `BoundExit::sent`
/// false) is left alone; one that was written and is not pending now went with the socket and loses its row,
/// whether or not the order map ever learned its venue id.
pub(super) fn forget_vanished_exits(
    pre: &HashSet<String>,
    present: &HashSet<String>,
    positions: &PositionsByIdShared,
) {
    let Ok(mut book) = positions.lock() else { return };
    for order_client_id in pre.difference(present) {
        book.unbind_exit(order_client_id);
    }
    // And the rows of orders that were WRITTEN but never became known (the socket died between the write and
    // the venue's accept): not in `pre`, not pending, and a row the order map cannot cancel.
    book.forget_sent_exits_not_in(present);
    // A leg of an exit that spans positions whose row is gone rests no more and will never report: it must not hold
    // its exit open (`crate::execution::exit_legs`).
    book.retire_exit_legs_without_rows();
}

/// Fold a `RECONCILE_RES`'s OPEN-position set into the shared book — marking it FETCHED **only if
/// it is about THIS account and every row was understood**. The one writer of
/// [`crate::execution::positions::PositionsById::replace_all`], shared by the INITIAL-connect seed
/// ([`seed_positions_at_connect`]) and the reconnect rebuild ([`reconcile_after_reconnect`]) so the
/// two cannot disagree about what a reconcile answer means.
///
/// ⚠ **`ctid` is checked FIRST, and its absence was a live defect.** `ProtoOAReconcileRes` carries
/// the `ctidTraderAccountId` it answers for, and nothing compared it. An answer for ANOTHER account
/// was folded in wholesale — replacing this account's routing entries with a stranger's, and
/// marking the result AUTHORITATIVE, so a stranger's flat book became this account's `Flat` and
/// refused its exit under `halt_admit = "verify"`
/// (`crates/bridges/ctrader/tests/offline/execution_halt.rs`'s
/// `a_reconcile_answer_for_another_account_is_not_this_accounts_evidence` measured exactly that).
/// A mismatched answer therefore changes NOTHING except to `invalidate` the evidence flag: we
/// asked, and what came back was not about us, so whatever we believed is now of unknown standing.
///
/// ⚠ **The `unreadable` arm is not defensive decoration — it is the difference between evidence and
/// a manufactured fact.** A row this build cannot classify
/// (`crates/bridges/ctrader/src/execution/positions.rs`'s `reconcile_rows`: an ERROR-status position, a
/// `positionStatus` the vendored proto has no variant for, an OPEN row with a non-positive volume)
/// is a HOLE, and a hole is indistinguishable from an absence once it is dropped. So the entries are
/// taken either way — routing runs on the best guess available, exactly as after an
/// [`crate::execution::positions::PositionsById::invalidate`] — and the book declines to call itself evidence,
/// which lands at the halt boundary as `Unknown` and ADMITS.
///
/// ⚠ **Neither arm can see a row the venue never SENT**, and that is why the halt boundary does not
/// rest on this function alone: `PositionsById::unauthoritative_for` additionally demands POSITIVE
/// coverage of the symbol being judged. Everything here is about the provenance of an answer; only
/// coverage is about what the answer actually said.
pub(super) fn rebuild_position_book(
    res: &ProtoOaReconcileRes,
    ctid: i64,
    positions: &PositionsByIdShared,
) {
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
/// [`crate::execution::positions::plan_reduce`] sees no opposing exposure and routes a flatten as a NEW ORDER,
/// stacking a hedge on a hedging account; `crates/bridges/ctrader/src/execution/mod.rs`'s `close_all` exists
/// precisely as the manual workaround for the book that resulted. Seeding here fixes the routing
/// bug on its own, independently of any halt policy.
///
/// It also makes "flat" a FACT rather than an absence, which is what a position-verified halt admit
/// needs (`crates/bridges/ctrader/src/execution/positions.rs`'s `PositionsById`).
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
/// by [`crate::adapter::VenueAdapterCtrader`], which `crates/vike-tradehub-core/src/engine.rs`'s `make_engine`
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
///   set. A second started account is a second `connect_and_auth_execution`, and it pays its own seed,
///   sequentially; that is a reason to bound this call, and it is bounded.
/// - *Per RECONNECT*: nothing new. [`reconcile_after_reconnect`] has always issued this same
///   `ProtoOAReconcileReq` under [`RECONCILE_TIMEOUT`], on the actor thread, so a reconnect storm
///   costs exactly what it cost before this function existed — and costs it where nobody is
///   blocked.
///
/// Every outcome logs `elapsed_ms`, so the cost is observable in production rather than inferable
/// from this doc.
///
/// It seeds the positions AND the order client id→orderId map ([`learn_pending_orders`]), but re-emits no
/// `OrderAccepted`. The map is owed here because a resting order outlives the process that placed it
/// (a bracket's stop-loss and take-profit stay across a restart): without it the restarted process
/// cannot cancel or amend an order that plainly rests, since a cancel keys on the venue `orderId`
/// and the map starts empty. The announcement stays the reconnect path's: it has a BEFORE state to
/// reconcile against, and at initial connect the core would drop it as an unknown order client id.
pub(super) fn seed_positions_at_connect(
    reader: &mut Reader,
    ctid: i64,
    symbols: &SymbolMap,
    money_digits: u32,
    sink: &dyn DataClientSink,
    events: Option<&EventSender>,
    orders: &Arc<Mutex<OrderVenueIdRegistry<i64>>>,
    positions: &PositionsByIdShared,
    closes: &CloseState,
) -> Option<ProtoOaReconcileRes> {
    let req = ProtoOaReconcileReq { ctid_trader_account_id: ctid, ..Default::default() };
    // EMPTY clientMsgId, for the reason `reconcile_after_reconnect` spells out: a LATE `ERROR_RES`
    // for this reconcile must not carry a string `on_error_res` would misread as an order's order client id.
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
/// that is waiting on the answer ([`ConnectionCommand::ReadPositions`]); `true` when an answer for THIS account arrived
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
/// against an empty book OPENS a hedge on a hedged account (`crates/bridges/ctrader/src/execution/mod.rs`'s `submit`).
/// Frames that arrive while waiting are buffered and dispatched afterwards, as in the seed, never dropped.
/// An answer that is not about this account, a venue refusal, a timeout and a write failure are all `false`.
pub(super) fn read_positions_now(
    reader: &mut Reader,
    ctid: i64,
    symbols: &SymbolMap,
    money_digits: u32,
    sink: &dyn DataClientSink,
    events: Option<&EventSender>,
    orders: &Arc<Mutex<OrderVenueIdRegistry<i64>>>,
    positions: &PositionsByIdShared,
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
const ORPHAN_WINDOW_FALLBACK_MS: i64 = vike_model::MS_PER_WEEK;

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
/// remedy, and kept in the book ([`positions::PositionsById::orphaned_exits`]) until it ends.
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
    sink: &dyn DataClientSink,
    events: Option<&EventSender>,
    orders: &Arc<Mutex<OrderVenueIdRegistry<i64>>>,
    positions: &PositionsByIdShared,
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
                    order_client_id = ?exit.order_client_id,
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

/// What a close is refused in when its socket died before the venue answered it and the venue still lists its position.
const CLOSE_UNANSWERED: &str = "ctrader: the connection to the venue dropped before this close was answered, and the venue still lists the position, so this close is over; send it again if it is still wanted";

/// F3 (reconcile exec state on reconnect). Right after a reconnect+reauth — BEFORE subscriptions
/// replay — ask the venue for its current OPEN positions + PENDING orders (`ProtoOAReconcileReq`)
/// and, from the answer:
///   1. **Rebuild the order client id→orderId map** from the pending orders, so post-reconnect cancel/modify
///      (which key on the numeric `orderId`) resolve again — the concretely-valuable, unambiguously
///      -safe fix.
///   2. **Re-emit an `OrderAccepted`** (NOT a fill) for each still-pending order. This is SAFE
///      because `OrderAccepted` never folds into `Account` (no trades/PnL under any path in the core
///      engine's `on_event`): a duplicate on an order the core still tracks is dropped by the FSM
///      (accept is legal only from SUBMITTED), and one for an order the core lost is dropped as an
///      unknown order client id — neither double-counts anything. It re-establishes a live order the core may
///      have lost during the dead window; harmless if already known.
///   3. **Gap observability only**: for any order client id the adapter tracked BEFORE the reconnect that is
///      ABSENT from the pending set, log ONE `warn` (a possible gap fill/cancel). We do NOT
///      synthesize a terminal (`OrderFilled`/`OrderCanceled`) — reconcile alone cannot tell fill
///      from cancel, and a wrong terminal is worse than a logged gap. Full deal-history recovery is
///      out of scope.
///   4. **Ask the venue about what the dead socket left unanswered** ([`ask_about_unanswered`]): an order
///      written and not pending now, a close in `stranded`. A market order that filled while nobody listened
///      is in no pending list and no event comes for it, and the stuck-order ladder would reject it over a
///      live position. What the venue lists has its events; what it does not list has none (absent proves nothing).
///   5. **End each close in `stranded` the list did not settle whose positions the venue still lists WHOLE** (at
///      least the volume each held when the socket died, [`end_stranded_closes`]): the engine's order would otherwise
///      wait for the ladder. A close whose position is gone or smaller may have done something while nobody
///      listened: no terminal here, its outcome is unknown.
///
/// Best-effort throughout: a write/response failure logs once and returns; the caller still replays
/// subscriptions.
pub(super) fn reconcile_after_reconnect(
    reader: &mut Reader,
    ctid: i64,
    symbols: &SymbolMap,
    money_digits: u32,
    sink: &dyn DataClientSink,
    orders: &Arc<Mutex<OrderVenueIdRegistry<i64>>>,
    positions: &PositionsByIdShared,
    closes: &CloseState,
    stranded: Vec<StrandedClose>,
    events: Option<&EventSender>,
) {
    // Snapshot the order client ids the adapter knew BEFORE the reconnect (for the gap check below). Terminal
    // order client ids are pruned from the map as their FILLED/CANCELLED/REJECTED/EXPIRED events arrive
    // (`update_order_map`), so this set holds only still-live orders — the gap-diff below is
    // therefore actionable, not spammed by historically-completed order client ids.
    let pre: HashSet<String> = orders
        .lock()
        .map(|m| m.order_client_ids().map(str::to_owned).collect())
        .unwrap_or_default();
    // The volume each stranded close's position held when the socket died, before the rebuild below replaces the book:
    // a position the venue lists with less has had something executed on it while nobody listened (5.).
    let held_at_death: HashMap<i64, i64> = positions
        .lock()
        .map(|book| {
            stranded
                .iter()
                .flat_map(|close| &close.positions)
                .filter_map(|id| book.position(*id).map(|p| (*id, p.volume)))
                .collect()
        })
        .unwrap_or_default();

    let req = ProtoOaReconcileReq { ctid_trader_account_id: ctid, ..Default::default() };
    // Non-order request → EMPTY clientMsgId (F2b invariant, see `write_command`'s doc): a late
    // `ERROR_RES` for this reconcile — one arriving AFTER our timeout, so it falls through to
    // `on_error_res` on the main loop — must not carry an order client id that `on_error_res` would misread as a
    // real order's, fabricating a phantom `OrderRejected{order_client_id:"recon"}`.
    if send(reader, pt::RECONCILE_REQ, &req, "").is_err() {
        tracing::warn!(
            target: "ctrader",
            "reconcile request write failed; skipping reconcile (subscriptions still replay)"
        );
        settled(positions);
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
        // 1. Rebuild the order client id→orderId correlation map from the pending orders.
        let present = learn_pending_orders(&res, orders);

        // 2. Re-emit OrderAccepted for each still-pending order (idempotent — never a fill).
        if let Some(events) = events {
            for order in &res.order {
                let Some(order_client_id) = reconcile_order_order_client_id(order) else {
                    continue;
                };
                let accepted = Event::OrderAccepted(OrderAccepted {
                    order_client_id,
                    venue_order_id: Some(order.order_id.to_string().into()),
                    timestamp: order.utc_last_update_timestamp.unwrap_or_else(now_ms),
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

        // 4. Order client ids tracked before the reconnect but no longer pending: their bound-exit rows go (an answer
        //    for ANOTHER account says nothing about this one's orders), and each is logged as a possible gap.
        if res.ctid_trader_account_id == ctid {
            forget_vanished_exits(&pre, &present, positions);
            if events.is_some() {
                let unsettled = ask_about_unanswered(
                    reader,
                    ctid,
                    &present,
                    stranded,
                    positions,
                    &mut buffered,
                );
                end_stranded_closes(unsettled, &held_at_death, positions, events);
            }
        }
        for order_client_id in pre.difference(&present) {
            tracing::warn!(
                target: "ctrader",
                order_client_id = %order_client_id,
                "reconcile: previously-tracked order absent from the venue's pending set on \
                 reconnect; possible fill/cancel during the dead window (an order this process wrote is \
                 looked up in the venue's lists; no other terminal is synthesized)"
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
    settled(positions);
}

/// The end of a reconnect's settle ([`crate::execution::positions::PositionsById::is_settling`]): what it could learn is dispatched.
fn settled(positions: &PositionsByIdShared) {
    if let Ok(mut book) = positions.lock() {
        book.settled();
    }
}

/// How far either side of what is asked about the order and deal lists read, because the venue's clock is not this box's.
pub(crate) const LIST_WINDOW_MARGIN_MS: i64 = 60_000;

/// **What the venue says of the orders and closes a dead socket left unanswered, as execution events for the caller's
/// inbound dispatch** (`buffered`); returns the closes it did not settle. `ProtoOAOrderListReq` lists an order in a final
/// state under its order client id (measured on the hedged demo account, `crates/bridges/ctrader/CLAUDE.md`'s "What the broker answered
/// on the hedged demo account, 2026-10-27 and 2026-10-28 (v0.1.96)"; the same list carries a closing order under the
/// close's order client id, its envelope id) and `ProtoOADealListReq` the deals of it, whose ids are the ones the live path and the
/// reconcile key a fill on, so a fill the engine already has is a duplicate it drops.
///
/// ⚠ **Absent proves nothing, and nothing is emitted for it**: the venue's rejects make no order, so a refused order is
/// absent exactly like one it never received, and the stuck-order ladder keeps it. A list that cannot be read, or that says
/// `hasMore`, is the same for whatever it did not show.
///
/// shortcut: one page of each list, because an outage's window is short. Page them if `hasMore` is ever logged here.
fn ask_about_unanswered(
    reader: &mut Reader,
    ctid: i64,
    present: &HashSet<String>,
    stranded: Vec<StrandedClose>,
    positions: &PositionsByIdShared,
    buffered: &mut Vec<ProtoMessage>,
) -> Vec<StrandedClose> {
    let mut asked =
        positions.lock().map(|mut b| b.take_written_orders_not_in(present)).unwrap_or_default();
    asked.extend(stranded.iter().map(|c| (c.order_client_id.clone(), c.written_ms)));
    let Some(&oldest) = asked.values().min() else { return stranded };
    let now = now_ms();
    let to = now + LIST_WINDOW_MARGIN_MS;
    let from = (oldest - LIST_WINDOW_MARGIN_MS).max(to - ORPHAN_WINDOW_FALLBACK_MS);
    let req = ProtoOaOrderListReq {
        ctid_trader_account_id: ctid,
        from_timestamp: Some(from),
        to_timestamp: Some(to),
        ..Default::default()
    };
    let listed = ask_list::<_, ProtoOaOrderListRes>(
        reader,
        pt::ORDER_LIST_REQ,
        &req,
        pt::ORDER_LIST_RES,
        buffered,
    )
    .filter(|res| res.ctid_trader_account_id == ctid);
    let Some(listed) = listed else {
        tracing::warn!(
            target: "ctrader",
            unanswered = ?asked.keys().collect::<Vec<_>>(),
            "the venue's order list could not be read after a reconnect: what the dead socket left unanswered stays \
             unknown, and the stuck-order ladder ends it"
        );
        return stranded;
    };
    let ours: Vec<&ProtoOaOrder> = listed
        .order
        .iter()
        .filter(|o| reconcile_order_order_client_id(o).is_some_and(|c| asked.contains_key(&c)))
        .collect();
    let deals = if ours.iter().any(|o| executed(o)) {
        let req = ProtoOaDealListReq {
            ctid_trader_account_id: ctid,
            from_timestamp: Some(from),
            to_timestamp: Some(to),
            ..Default::default()
        };
        ask_list::<_, ProtoOaDealListRes>(
            reader,
            pt::DEAL_LIST_REQ,
            &req,
            pt::DEAL_LIST_RES,
            buffered,
        )
        .filter(|res| res.ctid_trader_account_id == ctid)
        .map(|res| res.deal)
        .unwrap_or_default()
    } else {
        Vec::new()
    };
    let mut settled: HashSet<String> = HashSet::new();
    for order in ours {
        let Some(order_client_id) = reconcile_order_order_client_id(order) else { continue };
        tracing::warn!(
            target: "ctrader",
            %order_client_id,
            order_id = order.order_id,
            order_status = order.order_status,
            "the venue lists an order a dead socket left unanswered: taking its state from the list"
        );
        buffered.extend(frames_of_listed_order(order, &deals, ctid));
        settled.insert(order_client_id);
    }
    let silent: Vec<&String> = asked.keys().filter(|c| !settled.contains(*c)).collect();
    if !silent.is_empty() {
        tracing::warn!(
            target: "ctrader",
            ?silent,
            has_more = listed.has_more,
            "the venue's order list does not hold these, which proves nothing (a refused order is absent too): the \
             stuck-order ladder ends them"
        );
    }
    stranded.into_iter().filter(|c| !settled.contains(&c.order_client_id)).collect()
}

/// Whether a listed order has executed any volume, so its deals are worth reading.
fn executed(order: &ProtoOaOrder) -> bool {
    order.order_status == ProtoOaOrderStatus::OrderStatusFilled as i32
        || order.executed_volume.is_some_and(|v| v > 0)
}

/// One bounded list request on the reconnect's socket, EMPTY clientMsgId for the reason [`reconcile_after_reconnect`]
/// spells out. `None` for a failed write, a refusal, a timeout or an undecodable answer.
fn ask_list<SeenRequest: Message, Res: Message + Default>(
    reader: &mut Reader,
    req_type: u32,
    req: &SeenRequest,
    res_type: u32,
    buffered: &mut Vec<ProtoMessage>,
) -> Option<Res> {
    send(reader, req_type, req, "").ok()?;
    match read_reply(reader, buffered, SEED_TIMEOUT, res_type) {
        Reply::Got(msg) => Res::decode(msg.payload.as_deref().unwrap_or(&[])).ok(),
        Reply::Refused | Reply::TimedOut => None,
    }
}

/// A listed order as the execution events the live path would have read: the accept first (a fill ahead of it is refused
/// by the FSM, and a repeat of it is dropped), one per deal, oldest first, the last of a FILLED order the one that ends it,
/// then the end of an order that ended otherwise. A FILLED order whose deals could not be read gets the accept alone: the
/// order is truly live at the venue's word, and the fill is the reconcile's `MissingFill`.
fn frames_of_listed_order(
    order: &ProtoOaOrder,
    deals: &[ProtoOaDeal],
    ctid: i64,
) -> Vec<ProtoMessage> {
    let frame = |kind: ProtoOaExecutionType, deal: Option<ProtoOaDeal>| {
        let event = ProtoOaExecutionEvent {
            ctid_trader_account_id: ctid,
            execution_type: kind as i32,
            order: Some(order.clone()),
            deal,
            ..Default::default()
        };
        ProtoMessage {
            payload_type: pt::EXECUTION_EVENT,
            payload: Some(event.encode_to_vec()),
            client_msg_id: None,
        }
    };
    let status = ProtoOaOrderStatus::try_from(order.order_status);
    if matches!(status, Ok(ProtoOaOrderStatus::OrderStatusRejected)) {
        return vec![frame(ProtoOaExecutionType::OrderRejected, None)];
    }
    let mut own: Vec<ProtoOaDeal> = deals
        .iter()
        .filter(|d| d.order_id == order.order_id && d.filled_volume > 0)
        .cloned()
        .collect();
    own.sort_by_key(|d| d.execution_timestamp);
    let last = own.pop();
    let mut frames = vec![frame(ProtoOaExecutionType::OrderAccepted, None)];
    frames.extend(own.into_iter().map(|d| frame(ProtoOaExecutionType::OrderPartialFill, Some(d))));
    match status {
        Ok(ProtoOaOrderStatus::OrderStatusFilled) => {
            frames.extend(last.map(|d| frame(ProtoOaExecutionType::OrderFilled, Some(d))));
        }
        Ok(ProtoOaOrderStatus::OrderStatusCancelled | ProtoOaOrderStatus::OrderStatusExpired) => {
            frames.extend(last.map(|d| frame(ProtoOaExecutionType::OrderPartialFill, Some(d))));
            frames.push(frame(
                if matches!(status, Ok(ProtoOaOrderStatus::OrderStatusCancelled)) {
                    ProtoOaExecutionType::OrderCancelled
                } else {
                    ProtoOaExecutionType::OrderExpired
                },
                None,
            ));
        }
        _ => {}
    }
    frames
}

/// The closes a dead socket stranded, against the book a reconcile answer just rebuilt: a close EVERY one of whose
/// positions is still listed with at least the volume it held when the socket died (`held_at_death`) executed nothing,
/// so it gets its legal terminal ([`close_terminal_event`]). Any other close may have filled unheard (a leg closing
/// part of a position leaves that position listed, smaller), and no terminal is true of it yet.
pub(super) fn end_stranded_closes(
    stranded: Vec<StrandedClose>,
    held_at_death: &HashMap<i64, i64>,
    positions: &PositionsByIdShared,
    events: Option<&EventSender>,
) {
    let Some(events) = events else { return };
    for close in stranded {
        let untouched = !close.positions.is_empty()
            && positions.lock().is_ok_and(|book| {
                close.positions.iter().all(|id| {
                    let listed = book.position(*id).map(|p| p.volume);
                    listed.is_some_and(|now| held_at_death.get(id).is_some_and(|held| now >= *held))
                })
            });
        if untouched {
            tracing::warn!(target: "ctrader", order_client_id = %close.order_client_id, positions = ?close.positions, "{CLOSE_UNANSWERED}");
            // shortcut: the close WAS written, and the venue's list proves it executed nothing, so it is safe to send
            // again, which is the catalog's not-sent meaning; give it an id of its own if the catalog grows one for a
            // write a read proved ended nothing.
            let event = close_terminal_event(
                &close.order_client_id,
                close.progressed,
                CLOSE_UNANSWERED,
                now_ms(),
                vike_model::OrderRefusalId::NotSent,
            );
            emit_event(events, positions, event);
        } else {
            tracing::warn!(
                target: "ctrader",
                order_client_id = %close.order_client_id,
                positions = ?close.positions,
                "a close's socket died before its answer and the venue no longer lists its position whole: it may \
                 have filled while nobody listened, so no terminal is sent (outcome unknown)"
            );
        }
    }
}

/// After a successful reconnect+reauth, reconcile exec state (F3, [`reconcile_after_reconnect`])
/// then re-issue every currently-active spot/trendbar subscription (cTrader does not remember
/// subscriptions across a dropped socket) and flush any commands that
/// arrived from `rx` while the actor was reconnecting (buffered by [`reconnect_with_backoff`], in
/// arrival order). A failed SUBSCRIBE replay is swallowed — if the fresh connection is ALSO already
/// broken, the next read hits an error/EOF and triggers another reconnect naturally. But a failed
/// ORDER-command replay must NOT vanish: it is booked as the main drain loop books it (`book_write`): NOT SENT
/// synthesizes the same terminal/non-terminal reject (via [`write_failure_event`]) so the venue-adapter contract
/// still holds, and an unknown outcome is a written order the next reconnect asks about.
///
/// A flushed command is TRACKED as the main drain tracks it: a key subscribed during the outage would
/// otherwise be lost at the NEXT reconnect, and a key released during it replayed for ever.
#[expect(clippy::too_many_arguments)]
pub(super) fn replay_after_reconnect(
    reader: &mut Reader,
    ctid: i64,
    symbols: &SymbolMap,
    money_digits: u32,
    sink: &dyn DataClientSink,
    spot_subs: &mut HashSet<i64>,
    trendbar_subs: &mut HashSet<(i64, ProtoOaTrendbarPeriod)>,
    pending: Vec<ConnectionCommand>,
    stranded: Vec<StrandedClose>,
    events: Option<&EventSender>,
    orders: &Arc<Mutex<OrderVenueIdRegistry<i64>>>,
    positions: &PositionsByIdShared,
    closes: &CloseState,
) {
    // Reconcile FIRST — restore the order client id→orderId map + re-establish pending orders before any new
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
        stranded,
        events,
    );
    resubscribe(reader, ctid, spot_subs, trendbar_subs);
    for cmd in pending {
        track_subscription(&cmd, spot_subs, trendbar_subs);
        // Captured BEFORE `cmd` is consumed by `write_command` (mirrors the main drain loop).
        let on_write_failure = write_failure_event(&cmd, closes);
        let close_order_client_id = close_command_order_client_id(&cmd);
        let new_order = new_order_order_client_id(&cmd);
        let written = write_command(reader, ctid, cmd).map(drop).map_err(|failure| failure.outcome);
        book_write(
            written,
            on_write_failure,
            &new_order,
            &close_order_client_id,
            positions,
            closes,
            events,
        );
    }
}
