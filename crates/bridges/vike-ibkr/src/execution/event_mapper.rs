//! The event-correctness heart: IB callback reports → canonical vike `Event`s, with the four traps
//! the ibapi and reference-adapter studies surfaced baked in:
//!   1. Exec-before-open synthesis — a fill arriving before ACCEPTED synthesizes `OrderAccepted`.
//!   2. `execution_id` commission join — execDetails + commissionReport are two async messages;
//!      buffer by exec_id and emit the fill only once its commission joins (never pair by arrival).
//!      The join is **symmetric**: IBKR delivers the two halves in EITHER order, so whichever lands
//!      first is parked (`pending` for the execution, `commissions` for the commission) and the
//!      second one completes the pair.
//!      `ibapi` 3.2.1's `CommissionReport`
//!      (<https://docs.rs/ibapi/3.2.1/ibapi/orders/struct.CommissionReport.html>) is the
//!      authority — *"There is **no** temporal pairing to reason about: IBKR may deliver the
//!      ExecutionData and the matching CommissionReport in either order, but the `execution_id` is
//!      the stable key linking them … a wrapper should index commissions by `execution_id` rather
//!      than guessing at arrival order."* Only `pending` existed before, so a commission-first pair
//!      dropped the commission AND stranded its execution forever — the fill never emitted at all.
//!   3. Data-vs-Notice — advisory codes (2100–2169) emit nothing.
//!   4. Typed terminal status drives order client id⇄orderId map cleanup.
//!   5. **A commission that never arrives is RECOVERED from the venue, never invented.** The fill
//!      is emitted exclusively by the join, so an execution whose commissionReport is lost on an
//!      order that is never cancelled used to sit in `pending` forever and its fill never emitted
//!      — position and realized PnL silently short. The cure is NOT a commission-optional emit
//!      path (`FillEvent.commission` is a bare `f64` that cannot express "unknown", and a
//!      fabricated `0.0` trades a silent loss for a silent understatement of cost basis): IBKR
//!      can be ASKED again. `reqExecutions` re-delivers the CURRENT DAY's executions *and* their
//!      commission reports
//!      (`ibapi` 3.2.1's blocking `Client::executions`,
//!      <https://docs.rs/ibapi/3.2.1/ibapi/client/blocking/struct.Client.html#method.executions>
//!      — *"Along with the ExecutionData, the CommissionReport will also be returned"*), so
//!      [`EventMapper::sweep_pending`] reports a stranded execution to the exec loop, which
//!      re-requests them and lets the ordinary join emit the fill with the REAL commission. See
//!      that method for the bound, the retry budget, and the residual it does NOT close.
//!
//!   6. **A fresh connection LEARNS the venue** ([`EventMapper::begin_learning`], `on_venue_order`): the
//!      rows of the open-order snapshot that are THIS client's rebind the order client id⇄orderId map, seed the
//!      order's `fill_state` (`filled` 0, `total` from the row: the executions replay that follows adds
//!      the fills, so the first replayed partial is a partial) and announce a working order as accepted;
//!      an execution that resolves to no order is remembered as unroutable so its commission is dropped,
//!      not parked. Foreign rows never reach here (`OwnOrders`, checked by the exec loop).
//!   7. **A fill carries the symbol its ORDER was submitted under** ([`EventMapper::register_symbol`], [`EventMapper::fill_symbol`]),
//!      never the instrument IB echoes on the execution: the engine drops a bare `Fill` whose symbol it neither mounts nor
//!      registered the order under, in silence. An order the connection REBOUND from the venue that the host never taught
//!      ([`EventMapper::is_untaught`]), like the replayed execution of a finished order nobody taught, emits NO fill at all:
//!      it is tracked, announced and cancellable, and its fills reach the books through the reconcile.
//!   8. **A venue cancel waits for the commissions of the order's unjoined fills** ([`EventMapper::release_held_cancels`]), so a fill
//!      goes out with its real fee instead of an invented `0.0`; when the bound runs out the venue is ASKED once for the
//!      commission ([`EventMapper::ask_for_held_commissions`]: `reqExecutions`, as in trap 5), and past a second, short bound the
//!      fee is booked unknown, loudly.
//!
//! Pure + fixture-tested; owns an `OrderVenueIdAllocator`. Consumed by the exec loop (Task 8,
//! `execution::run_execution` folds transport inbound through it).
//!
//! **Clockless by construction.** Nothing here reads a clock. `sweep_pending` takes `now_ms` as a
//! PARAMETER and uses only DIFFERENCES of it, so every test passes explicit millis and stays
//! deterministic — the same event-driven-not-clock-driven discipline the parked-commission expiry
//! in [`EventMapper::on_execution_details`] already follows.

use std::collections::{HashMap, VecDeque};

use crate::execution::amend::{PendingAmend, PendingAmends};
use crate::execution::order::OrderStatusKind;
use crate::execution::registry::{OrderVenueIdAllocator, Rebound};
use crate::transport::error_codes::{CodeClass, classify_code};
use vike_bridge_core::per_symbol::OrderClientIdSymbols;
use vike_bridge_core::terminal_events;
use vike_model::events::{
    Event, FeeAdjustmentEvent, FillEvent, OrderAccepted, OrderCanceled, OrderRejected, TradeId,
};

#[derive(Default)]
pub struct IbkrOrderStatus {
    pub order_id: i32,
    pub order_ref: String,
    pub status: String,
    pub filled: f64,
    pub average_fill_price: f64,
    /// The API client that placed the order, when the backend says (0 = it does not).
    pub client_id: i32,
}

/// One row of the VENUE's open-order snapshot (IB `openOrder`), as the socket backend decodes it. Unlike the
/// bare `IbkrInbound::OpenOrder` pair (the cpapi pump's, which has no more to say), it carries what is needed to
/// tell an order of THIS client from any other (`client_id`, `account`) and to seed the order's fill tracking
/// (`total_qty`).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct IbkrOpenOrder {
    pub order_id: i32,
    /// IB's `orderRef`: our order client id when this client placed the order, EMPTY for an order made in TWS.
    pub order_ref: String,
    /// The API client that placed the order (0 for an order made in TWS).
    pub client_id: i32,
    pub account: String,
    /// The order's total quantity; not a positive finite number = the row is unusable.
    pub total_qty: f64,
    /// IB's status word for the order (`Submitted`, `PreSubmitted`, ...).
    pub status: String,
    /// The quantity the VENUE says is already filled (all days, a GTC may have filled yesterday).
    pub filled_qty: f64,
    /// The order's limit price as the venue holds it (`None` = the backend did not say, or a market/stop order).
    /// Read only to confirm an amend ([`crate::execution::amend::PendingAmends`]).
    pub limit_price: Option<f64>,
    /// The order's auxiliary (stop trigger) price as the venue holds it (`None` = not said or not a stop).
    pub aux_price: Option<f64>,
}

#[derive(Default)]
pub struct IbkrExecutionDetails {
    pub order_id: i32,
    pub order_ref: String,
    pub exec_id: String,
    pub symbol: String,
    pub side_buy: bool,
    pub shares: f64,
    pub price: f64,
    pub ts: i64,
    /// The API client that placed the order, when the backend says (0 = it does not).
    pub client_id: i32,
    /// Set by the exec loop for an execution of THIS client's order (by `orderRef`) replayed while the
    /// connection learns the venue: when it matches no order the connection knows, it is a fill of an order
    /// that finished while the connection was down, and is recovered as a partial - when the host taught the
    /// order's symbol (`register_symbol`); an untaught one is dropped as unroutable. See
    /// [`EventMapper::on_execution_details`].
    pub recoverable: bool,
}

pub struct IbkrCommissionReport {
    pub exec_id: String,
    pub commission: f64,
    pub currency: String,
}

/// A fill awaiting its commission (join by exec_id).
struct PendingFill {
    order_id: i32,
    /// The IB `execution_id` as the validated dedup key, resolved ONCE at ingress
    /// ([`EventMapper::on_execution_details`]) and carried so neither of the two `FillEvent` sites — the
    /// commission join and the cancel flush — has to re-validate it or can disagree about it.
    trade_id: TradeId,
    order_client_id: String,
    symbol: String,
    side: i32,
    shares: f64,
    price: f64,
    ts: i64,
    /// A fill of an order that finished while the connection was down, recovered from the replay: always
    /// a PARTIAL (nothing says how the order ended) and it changes no order bookkeeping.
    recovered: bool,
    /// The fill of an order this connection REBOUND from the venue's open orders and nobody taught the symbol of
    /// ([`EventMapper::is_untaught`]), decided when the execution arrived. Its bookkeeping runs as for any fill
    /// (the order's fill tracking, the dedup, the final fill's retirement) but NO fill event is emitted for it:
    /// see [`EventMapper::log_untaught_fill`].
    untaught: bool,
    /// Monotonic-ms stamp of the FIRST [`EventMapper::sweep_pending`] that observed this entry —
    /// not of its arrival. The mapper is clockless (module doc), so time can only enter through
    /// that call's parameter; the measured age is therefore `(grace - sweep_interval, grace]`
    /// rather than exact, which is precisely the accuracy this decision needs.
    first_seen_ms: Option<i64>,
    /// Venue re-requests already spent trying to recover this entry's commissionReport, capped by
    /// [`MAX_PENDING_RECOVERY_ATTEMPTS`].
    recovery_attempts: u32,
    /// Set once the retry budget is exhausted and the entry has been reported to the operator, so
    /// the escalation is logged EXACTLY once instead of every sweep. The entry itself is still
    /// held — see [`EventMapper::sweep_pending`] for why it is never evicted.
    escalated: bool,
}

/// A commission awaiting its execution (join by exec_id) — the MIRROR of [`PendingFill`], for the
/// arrival order IBKR explicitly permits (module doc trap 2).
struct PendingCommission {
    commission: f64,
    currency: String,
    /// Monotonic park order. `HashMap` has no insertion order, and the cap needs to evict the
    /// OLDEST park — see [`EventMapper::park_commission`].
    seq: u64,
}

/// Ceiling on [`EventMapper::commissions`], the commissions parked awaiting their execution.
///
/// **Why a cap at all:** the key is a venue-supplied `execution_id`, so an unbounded map is a
/// memory hazard driven by the venue rather than by us — and unlike `pending` (whose entries are
/// each created by an execution WE can attribute to a local order), a parked commission can be for
/// an execution that will never be routable, so nothing else would ever remove it.
///
/// **Why 1024:** the join partner normally arrives on the very next message of the same stream, so
/// the realistic depth is single digits; 1024 is ~5x the largest single-order execution
/// fragmentation burst worth planning for, and ≈100 KB at the ceiling. Small enough that on an
/// account with any flow, a genuinely orphaned commission is pushed out within 1024 subsequent
/// reports instead of living for the process lifetime — the cap IS the backstop expiry (see
/// [`EventMapper::park_commission`]; the definitive expiry is in [`EventMapper::on_execution_details`]).
const MAX_PARKED_COMMISSIONS: usize = 1024;

/// How long a buffered execution may sit in [`EventMapper::pending`] without its commissionReport
/// before the bridge asks IBKR to re-deliver the day's executions (see
/// [`EventMapper::sweep_pending`]). The join partner normally arrives on the very next message of
/// the same stream, so this is ~3 orders of magnitude of slack: long enough that an ordinary pair
/// never triggers a re-request, short enough that a genuinely stranded fill is recovered inside the
/// same trading minute rather than at the next reconnect (which might be hours away, or never).
pub(crate) const PENDING_COMMISSION_GRACE_MS: i64 = 15_000;

/// How many venue re-requests ONE stranded execution gets before it is escalated to the operator.
/// Each attempt restarts that entry's clock, so the ladder is ~15s / 30s / 45s before the
/// `error!`. Bounded because a re-request that has already failed three times is not failing for a
/// reason a fourth will fix — it means IBKR has no commissionReport for that execution, which is an
/// operator problem, not a retry problem.
pub(crate) const MAX_PENDING_RECOVERY_ATTEMPTS: u32 = 3;

/// Ceiling on [`EventMapper::emitted`], the exec_ids whose fill has already been emitted.
///
/// **Why a cap:** the key is a venue-supplied `execution_id` and entries are only ever added, so on
/// a long-lived daemon this would otherwise grow without bound — the same hazard
/// [`MAX_PARKED_COMMISSIONS`] exists for.
///
/// **Why eviction is safe here, unlike for `pending`:** evicting an `emitted` entry cannot lose a
/// fill. The worst case is that a replayed pair for an execution older than the last 4096
/// re-emits — and `vike_execution::Engine` carries an ALWAYS-ON `seen_trade_ids` /
/// `seen_fsm_trade_ids` dedup keyed on `FillEvent.trade_id` (which IS the IB `execution_id`)
/// precisely for reconnect replays, so the duplicate is dropped one layer up. The one genuine
/// residual is local: `fill_state.filled` would double-count that replay, which can only matter if
/// the SAME order is still live 4096 executions later.
const MAX_EMITTED_EXECUTION_IDS: usize = 4096;

/// One execution whose fill could not be emitted: its commissionReport never arrived and the venue
/// re-request budget is spent. Reported by [`EventMapper::sweep_pending`] so the exec loop can name
/// exactly which fill the platform is short — the entry itself is still held and still joinable.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct StrandedFill {
    pub exec_id: String,
    pub order_client_id: String,
    pub order_id: i32,
    pub symbol: String,
    pub side: i32,
    pub shares: f64,
    pub price: f64,
}

/// What one [`EventMapper::sweep_pending`] pass found. Both vectors are sorted by `exec_id`, so the
/// result is deterministic despite `HashMap`'s random iteration order.
#[derive(Debug, Clone, Default, PartialEq)]
pub(crate) struct PendingSweep {
    /// Executions past the grace window with retry budget left: the caller should ask the venue to
    /// re-deliver the day's executions. ONE account-wide request covers every id in here.
    pub recover: Vec<String>,
    /// Executions whose budget is spent, reported ONCE each.
    pub stranded: Vec<StrandedFill>,
}

impl PendingSweep {
    /// Nothing to do this pass — the steady state.
    pub(crate) fn is_empty(&self) -> bool {
        self.recover.is_empty() && self.stranded.is_empty()
    }
}

/// Cumulative-vs-total fill tracking for one order, keyed by `order_id` — see
/// `on_commission_report`'s partial-vs-final decision.
#[derive(Default)]
struct FillState {
    /// The filled quantity at which the order is complete. Follows the order's quantity when the venue changes
    /// it ([`EventMapper::follow_venue_qty`]): an amend that raises or lowers the quantity moves it by the same
    /// amount, so the fill that completes the amended order is final and the one before it is not.
    total: f64,
    /// The order's total quantity as the venue last said it (the `total` before any learned reduction).
    qty: f64,
    filled: f64,
    /// What the venue said was ALREADY filled when the order was learned (`IbkrOpenOrder::filled_qty`); `0`
    /// for an order placed on this connection. See [`EventMapper::settle_learned_totals`].
    venue_filled: f64,
}

/// Where the mapper is in LEARNING the venue after a fresh connection (socket backend): the open orders
/// first (the executions replay of a rebound order needs its binding), then the day's executions. See
/// [`EventMapper::begin_learning`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Learning {
    /// Nothing was asked: the connection never learns (cpapi, and every test that scripts no `Connected`).
    Idle,
    /// The open-order snapshot was requested and has not ended.
    OpenOrders,
    /// The open orders are known; the executions replay was requested and has not ended.
    Executions,
    /// Both ended complete: the bridge knows the venue's open orders and has replayed the day's fills.
    Done,
    /// A request failed or timed out: the bridge does NOT know the venue's open orders (or fills).
    Failed,
}

pub(crate) struct EventMapper {
    ids: OrderVenueIdAllocator,
    accepted: std::collections::HashSet<String>, // order client ids that have emitted OrderAccepted
    /// exec_id → fill awaiting its commission.
    ///
    /// **Deliberately UNCAPPED, and never evicted by the sweep** (module doc trap 5). The two maps
    /// are not symmetric: evicting a parked commission loses a fee figure, whereas evicting a
    /// `pending` entry would lose THE FILL ITSELF — the exact failure #949 removed. An entry that
    /// outlives its commissionReport is therefore recovered (`sweep_pending` → the exec loop asks
    /// IBKR to re-deliver the day's executions) or escalated, never dropped.
    ///
    /// Removal paths: the join (`on_commission_report` / `on_execution_details`) and
    /// `on_order_status`'s Cancelled/ApiCancelled flush. `sweep_pending` only ANNOTATES.
    pending: HashMap<String, PendingFill>,
    /// exec_id → commission awaiting its execution: the reverse index `ibapi` 3.2.1's docs
    /// prescribe (module doc trap 2). Empty in steady state — a commission that arrives after its
    /// execution joins and emits within the same call, so an entry surviving means that
    /// execution genuinely has not been delivered yet. Bounded by [`MAX_PARKED_COMMISSIONS`].
    commissions: HashMap<String, PendingCommission>,
    /// Monotonic counter stamped into each [`PendingCommission::seq`]. Only ever increments.
    commission_sequence: u64,
    /// Order ids submitted but not yet acked (active status) or terminal. The routing population
    /// for the id-less async-rejection path — see `on_error`.
    unacked: std::collections::HashSet<i32>,
    /// order id of `unacked` → since when the loop has seen it unanswered, on the loop's clock, or [`ASKED`] once the venue's
    /// open orders were read for it ([`EventMapper::silent_submit_due`]). Empty in a mapper no loop drives.
    silent_since: HashMap<i32, i64>,
    /// Per-order cumulative-vs-total fill tracking, populated by `on_submit` and consulted (and
    /// forgotten on the final fill) by `on_commission_report`. Absent entry ⇒ unknown total
    /// (external/rebound order) ⇒ single-fill-terminal fallback.
    fill_state: HashMap<i32, FillState>,
    /// exec_id → park seq, for every exec_id whose fill has ALREADY been emitted — by the join
    /// (`join_fill`) or by the cancel-with-commission-in-flight flush. Consulted, WITHOUT removing,
    /// at the top of BOTH `on_execution_details` and `on_commission_report`, so a re-delivered half is a
    /// no-op on either side.
    ///
    /// It began (#949) as `flushed`: a cancel-flush-only set, consumed on lookup. Both properties
    /// had to change for the venue re-request in trap 5 to be safe, because `reqExecutions` replays
    /// the day's executions INDISCRIMINATELY — including pairs whose fill already emitted:
    /// - **non-consuming**, so a replayed execDetails is dropped instead of re-entering `pending`
    ///   where the replayed commission would re-join it. Without this, a replayed PARTIAL of a
    ///   still-live order re-advances `fill_state.filled`, which can classify a later genuine
    ///   partial as FINAL — forgetting the order while it is still filling, so its remaining
    ///   executions resolve to nothing and are silently lost. That would be a strictly worse bug
    ///   than the one trap 5 fixes.
    /// - **exec-side too**, because a replayed commission alone would otherwise PARK (leaking one
    ///   `commissions` entry per replay per already-emitted fill).
    ///
    /// Bounded by [`MAX_EMITTED_EXECUTION_IDS`], oldest-first — see there for why evicting from THIS
    /// map is safe while evicting from `pending` is not.
    emitted: HashMap<String, u64>,
    /// Monotonic counter stamped into each `emitted` value; only ever increments. Separate from
    /// `commission_sequence` so the two caps evict independently.
    emitted_sequence: u64,
    /// Learning progress, see [`Learning`].
    learning: Learning,
    /// exec_id → park seq, for every execution that resolved to NO local order (the day's executions of
    /// orders that finished before this connection, replayed by `reqExecutions`). Its commission reaches
    /// `on_commission_report` afterwards and would PARK for ever (nothing can join it); this set drops it
    /// instead. Bounded like `emitted`.
    unroutable: HashMap<String, u64>,
    unroutable_sequence: u64,
    /// What the open-order snapshot being learned contained.
    stats: LearnStats,
    /// order client id → the canonical symbol the order was SUBMITTED under (`register_symbol`: the submit, or the host
    /// teaching a restored order). The symbol a fill carries comes from here, never from the instrument IB
    /// echoes on the execution: see [`EventMapper::fill_symbol`]. Removed when the order retires.
    symbols: OrderClientIdSymbols,
    /// The order client ids this connection REBOUND from the venue's open orders (`on_venue_order` during the learning): orders a
    /// previous connection of this client placed. One of them with no entry in [`EventMapper::symbols`] is UNTAUGHT
    /// ([`EventMapper::is_untaught`]): its fills emit nothing. Removed when the order retires.
    rebound: std::collections::HashSet<String>,
    /// The order client ids that reached a terminal on THIS connection, newest last (bounded by [`MAX_RECENT_FINISHED`]):
    /// the cancel of one of them is not the alarm a cancel of an unknown order is.
    finished: VecDeque<String>,
    /// A venue cancel that arrived while fills of the order still wait for their commission: the cancel (and
    /// the unjoined fills) are held back a bounded time so the fills carry their REAL commission. See
    /// [`EventMapper::release_held_cancels`].
    held_cancels: HashMap<i32, HeldCancel>,
    /// exec_id → what the engine was told about the fee of the fill emitted for it ([`FeeBook`]): the fee the
    /// FILL carried (`0.0` with no currency for a fill booked with its fee UNKNOWN: a cancel held the unjoined
    /// fill for [`CANCEL_COMMISSION_WAIT_MS`] and the commission still did not come) and the correction told
    /// since. A commissionReport that arrives for an execution already in here (late, or IB revising it) becomes
    /// an `Event::FeeAdjustment` (decision 0238: [`EventMapper::late_commission`]). Bounded like `emitted`.
    fee_book: HashMap<String, FeeBook>,
    fee_book_sequence: u64,
    /// The open-order snapshot of this connection is NOT known: learning is running in its first phase, or it
    /// failed there. A submit is refused while this holds (an order id handed out now could be a resting
    /// order's). False when nothing was ever asked (cpapi, scripted tests) and once the open orders ended.
    orders_unknown: bool,
    /// The host has not yet handed the client over (`execution::Taught`): the executions replay of the learning is
    /// not asked for until it has, so the fills of the restored orders it replays carry the symbols the host
    /// teaches. Set by the exec loop every turn; false in every mapper that is not driven by it.
    hold_executions: bool,
    /// The open orders ended while [`EventMapper::hold_executions`] held and the replay is still to be asked for.
    executions_owed: bool,
    /// Amends written to the venue and not yet answered (`execution::amend`).
    amends: PendingAmends,
    /// order id → what has executed of it, for the orders `execution::ClosesOut` follows ([`EventMapper::executed`]).
    executions: HashMap<i32, f64>,
    /// order id → IB's words refusing an ACCEPTED order, kept until IB's own status ends it (that end carries them) or
    /// [`EventMapper::expire_refusals`] gives up on one: a code alone never ends an order that may still rest at IB.
    refusals: HashMap<i32, OrderRefusal>,
    /// The orders that ended on THIS connection, as `(order id, order client id)`, newest last (bounded by [`MAX_RECENT_FINISHED`]):
    /// IB's refusal arriving after its order's end is logged naming the order, never attached to that end.
    ended: VecDeque<(i32, String)>,
    /// The exec loop's monotonic clock as of its last turn ([`EventMapper::tick`]). `None` in a mapper no loop drives:
    /// it holds no end, every end goes out at once as before [`EventMapper::fresh_ends`] existed.
    clock_ms: Option<i64>,
    /// order id → when this connection told the accept of an order it placed itself, while that is less than
    /// [`FRESH_END_WINDOW_MS`] ago and the loop has not asked to cancel it ([`EventMapper::note_cancel_sent`]).
    fresh_accepts: HashMap<i32, i64>,
    /// order id → IB's own end of a FRESH order (one of [`EventMapper::fresh_accepts`]) with no fill, held
    /// [`REFUSAL_HOLD_MS`] for the refusal IB sends right behind it: that refusal ends the order in IB's words, and with
    /// none the end goes out bare when the hold runs out ([`EventMapper::tick`]).
    fresh_ends: HashMap<i32, FreshEnd>,
}

/// IB's end of an order it ended right after accepting it, held for IB's words ([`EventMapper::fresh_ends`]).
struct FreshEnd {
    order_client_id: String,
    /// What the end says without IB's words (empty for `Cancelled`, the `Inactive` sentence for that word).
    reason: String,
    held_ms: i64,
}

/// An end IB sends this soon after the accept of an order this connection placed (and was not asked to cancel) is held
/// for IB's refusal ([`EventMapper::fresh_ends`]). MEASURED on the paper gateway 2026-11-09 (account #12, six orders IB
/// refused with `[201]`): the accept (the `openOrder` row) and IB's `Cancelled` came within 0.2 ms of each other, every
/// time. Generous on purpose: an end later than this is a venue end of a working order, sent at once as always.
pub(crate) const FRESH_END_WINDOW_MS: i64 = 2_000;

/// How long such an end waits for IB's refusal. MEASURED the same day: the `[201]` followed the `Cancelled` by 2 to 41 ms.
/// The cost: an order IB ends on its own right after taking it with no refusal (an IOC or FOK that met nothing) reports
/// its end this much later.
pub(crate) const REFUSAL_HOLD_MS: i64 = 500;

/// [`EventMapper::silent_since`]'s mark of an order the venue's open orders were already read for.
const ASKED: i64 = i64::MAX;

/// IB's words refusing an accepted order, waiting for its status ([`EventMapper::refusals`]).
struct OrderRefusal {
    words: String,
    /// Stamped by the first sweep that sees it (the mapper has no clock), like [`HeldCancel::first_seen_ms`].
    first_seen_ms: Option<i64>,
}

/// What the engine was told about the fee of ONE emitted fill, kept so a later `commissionReport` for the same
/// `execId` can be told as a correction ([`EventMapper::late_commission`], `Event::FeeAdjustment`, decision 0238).
/// Everything the event needs that the report does not carry rides here: the order, the symbol the fill was stamped
/// with, its price and its time.
struct FeeBook {
    trade_id: TradeId,
    order_client_id: String,
    symbol: String,
    px: f64,
    ts: i64,
    /// The commission the FILL carried: `0.0` with no currency when it went out with the fee unknown.
    fill_commission: f64,
    fill_asset: String,
    /// How much more than [`Self::fill_commission`] the engine has been told the fee is, in total (`0.0` until a
    /// correction went out), and the currency that was told. The next report is compared with this, so a repeated
    /// figure (IB re-sending, a `reqExecutions` replay) emits nothing.
    told_extra: f64,
    told_asset: String,
    /// Monotonic park order, for the oldest-first cap of [`EventMapper::note_fee_book`].
    seq: u64,
}

/// Two commissions closer than this are the same figure (IB states a few decimals; a re-sent report is bit-equal, so
/// this only absorbs a re-encoding).
const FEE_EPS: f64 = 1e-9;

/// A cancel waiting for the commissions of the order's unjoined fills. See [`EventMapper::release_held_cancels`].
struct HeldCancel {
    order_client_id: String,
    /// Stamped by the first release pass that saw it (the mapper has no clock), like `PendingFill::first_seen_ms`.
    first_seen_ms: Option<i64>,
    /// When the venue was ASKED for the missing commission ([`EventMapper::ask_for_held_commissions`]); the fee is booked
    /// unknown only [`COMMISSION_REREQUEST_WAIT_MS`] after that, so the answer has a chance to land first.
    asked_ms: Option<i64>,
    /// The words the `OrderCanceled` carries: empty for IB's own `Cancelled`, which says what it is.
    reason: String,
}

/// What an order ended by IB's `word` reads in the engine, and an unknown word said in the log: an unknown word is a fact
/// about IB worth seeing, and the order is read as over on the owner's ruling, not on anything the word says.
fn order_over_words(order_client_id: &str, word: &str) -> String {
    if word == "Inactive" || word == "Rejected" {
        return format!(
            "IB says this order is {word}: it is no longer active, rejected or cancelled at the venue"
        );
    }
    tracing::warn!(%order_client_id, status = %word, "ibkr: an order status word nobody documents: reading the order as over");
    format!(
        "IB reports this order as {word:?}, a word nobody documents: it is read as over, not as resting"
    )
}

/// The most order client ids [`EventMapper::finished`] remembers.
const MAX_RECENT_FINISHED: usize = 256;

/// The most submitted-symbol entries [`EventMapper::symbols`] holds: past it a new order is not registered and
/// its fills carry the echoed instrument (loudly). Entries leave with their order, so this is a leak guard.
const MAX_REGISTERED_SYMBOLS: usize = 4096;

/// [`EventMapper::symbols`]' line when a new order finds it full (this venue's words, on this module's target).
fn symbol_table_full(order_client_id: &str, cap: usize) {
    tracing::warn!(
        order_client_id,
        cap,
        "ibkr: submitted-symbol table full - this order's fills will carry the instrument IB \
         echoes, which an engine may not mount"
    );
}

/// Whether IB's echo of an execution's instrument (`SYMBOL.EXCHANGE.CURRENCY`, the exec contract) names the one the
/// order was `registered` under: the same symbol and currency, on the same exchange or, for an order routed `SMART`,
/// on any (the echo then names the venue that filled it: `AAPL.ARCA.USD`, `AAPL.IEX.USD`). A forex pair matches in
/// either field order of the canonical. Anything else, or a canonical that does not parse, is a real mismatch.
fn echo_names(registered: &str, echoed: &str) -> bool {
    let Ok(order) = crate::instrument::contract::parse_simplified(registered) else { return false };
    let mut fields = echoed.split('.');
    let (Some(symbol), Some(exchange), Some(currency), None) =
        (fields.next(), fields.next(), fields.next(), fields.next())
    else {
        return false;
    };
    symbol.eq_ignore_ascii_case(&order.symbol)
        && currency.eq_ignore_ascii_case(&order.currency)
        && (order.exchange.eq_ignore_ascii_case("SMART")
            || exchange.eq_ignore_ascii_case(&order.exchange))
}

/// How long a venue cancel is held back for the commissions of the order's unjoined fills before they are booked
/// with an UNKNOWN fee (booked as 0.0, named in an `error!`). IB sends a commission report right behind its
/// execution, so this is the "the report is not coming" bound, not a normal-path delay.
pub(crate) const CANCEL_COMMISSION_WAIT_MS: i64 = 5_000;

/// How long a held cancel waits AFTER the venue was asked for the missing commission ([`EventMapper::ask_for_held_commissions`])
/// before the fee is booked unknown. The ask is `reqExecutions`, which re-delivers the day's executions and their commission
/// reports (module doc, trap 5): the first re-request of the stranded-fill ladder falls at [`PENDING_COMMISSION_GRACE_MS`],
/// three times later than the cancel's own bound, so without this the release always preempted the one mechanism that could
/// have recovered the real fee.
pub(crate) const COMMISSION_REREQUEST_WAIT_MS: i64 = 5_000;

impl EventMapper {
    pub(crate) fn new(ids: OrderVenueIdAllocator) -> Self {
        EventMapper {
            ids,
            accepted: Default::default(),
            pending: Default::default(),
            commissions: Default::default(),
            commission_sequence: 0,
            unacked: Default::default(),
            silent_since: Default::default(),
            fill_state: Default::default(),
            emitted: Default::default(),
            emitted_sequence: 0,
            learning: Learning::Idle,
            unroutable: Default::default(),
            unroutable_sequence: 0,
            stats: LearnStats::default(),
            symbols: OrderClientIdSymbols::with_cap(MAX_REGISTERED_SYMBOLS, symbol_table_full),
            rebound: Default::default(),
            finished: Default::default(),
            held_cancels: Default::default(),
            fee_book: Default::default(),
            fee_book_sequence: 0,
            orders_unknown: false,
            hold_executions: false,
            executions_owed: false,
            amends: PendingAmends::default(),
            executions: Default::default(),
            refusals: Default::default(),
            ended: Default::default(),
            clock_ms: None,
            fresh_accepts: Default::default(),
            fresh_ends: Default::default(),
        }
    }

    /// The exec loop's turn: its monotonic clock (`now_ms`, only differences of it are used), the fresh accepts that
    /// aged out, and the ends whose hold for IB's words ran out, which go out now, bare (the `OrderCanceled` IB's end
    /// always was). Each turn, so a hold lasts [`REFUSAL_HOLD_MS`] to within a loop turn. `i64::MAX` releases every one.
    pub(crate) fn tick(&mut self, now_ms: i64) -> Vec<Event> {
        self.clock_ms = Some(now_ms);
        self.fresh_accepts.retain(|_, at| now_ms.saturating_sub(*at) <= FRESH_END_WINDOW_MS);
        let mut due: Vec<i32> = self
            .fresh_ends
            .iter()
            .filter(|(_, end)| now_ms.saturating_sub(end.held_ms) >= REFUSAL_HOLD_MS)
            .map(|(order_id, _)| *order_id)
            .collect();
        due.sort_unstable();
        let mut out = Vec::new();
        for order_id in due {
            let Some(end) = self.fresh_ends.remove(&order_id) else { continue };
            // Still this order's id (a fill that completed it meanwhile ended it already).
            if self.ids.order_client_id_of(order_id) == Some(end.order_client_id.as_str()) {
                out.extend(self.end_over(order_id, end.order_client_id, end.reason));
            }
        }
        out
    }

    /// The order client id of `order_id` if it ended on this connection lately ([`EventMapper::ended`]).
    fn ended_here(&self, order_id: i32) -> Option<&str> {
        self.ended
            .iter()
            .rev()
            .find(|(id, _)| *id == order_id)
            .map(|(_, order_client_id)| order_client_id.as_str())
    }

    /// The loop asked the venue to cancel `order_id`: its end is the answer, not IB's own, and goes out at once.
    pub(crate) fn note_cancel_sent(&mut self, order_id: i32) {
        self.fresh_accepts.remove(&order_id);
    }

    /// When to hold `order_id`'s end for IB's words: an order this connection placed, accepted less than
    /// [`FRESH_END_WINDOW_MS`] ago, not asked to cancel, with nothing filled or waiting for its commission. The answer is
    /// the hold's start.
    fn hold_fresh_end(&mut self, order_id: i32) -> Option<i64> {
        let now = self.clock_ms?;
        let accepted_at = self.fresh_accepts.remove(&order_id)?;
        let untraded = self.fill_state.get(&order_id).is_none_or(|st| st.filled == 0.0)
            && !self.has_unjoined(order_id);
        (now.saturating_sub(accepted_at) <= FRESH_END_WINDOW_MS && untraded).then_some(now)
    }

    /// `order_id` (`order_client_id`) ended on this connection: its refusal words are moot, and a refusal arriving later is logged
    /// naming it ([`EventMapper::ended`]).
    fn note_ended(&mut self, order_id: i32, order_client_id: &str) {
        self.refusals.remove(&order_id);
        self.fresh_accepts.remove(&order_id);
        if self.ended.len() >= MAX_RECENT_FINISHED {
            self.ended.pop_front();
        }
        self.ended.push_back((order_id, order_client_id.to_string()));
    }

    /// Give up on IB's words for an accepted order whose status has said nothing for `wait_ms`: the order is LEFT LIVE
    /// (it may still rest at IB; the reconcile pass judges it) and the notice is logged. Clockless like
    /// [`EventMapper::release_held_cancels`], called on the same cadence with the same bound.
    pub(crate) fn expire_refusals(&mut self, now_ms: i64, wait_ms: i64) {
        let mut due: Vec<i32> = Vec::new();
        for (order_id, r) in self.refusals.iter_mut() {
            if now_ms.saturating_sub(*r.first_seen_ms.get_or_insert(now_ms)) >= wait_ms {
                due.push(*order_id);
            }
        }
        due.sort_unstable();
        for order_id in due {
            let Some(r) = self.refusals.remove(&order_id) else { continue };
            tracing::warn!(
                order_client_id = self.ids.order_client_id_of(order_id).unwrap_or(""),
                order_id,
                msg = %r.words,
                waited_ms = wait_ms,
                "ibkr: IB refused this order but no status of IB's says it ended: it is LEFT LIVE (it may still rest \
                 at IB); the reconcile pass judges it"
            );
        }
    }

    /// Record that `order_id` was submitted (called by the exec loop right after `bind`, before
    /// `place_order`). It stays "unacked" until the order goes active or terminal — this is the
    /// population an id-less async order-rejection is attributed to (see `on_error`). Also seeds
    /// `fill_state` with the requested `total_qty` so `on_commission_report` can tell a partial
    /// fill from the one that completes the order.
    pub(crate) fn on_submit(&mut self, order_id: i32, total_qty: f64) {
        self.unacked.insert(order_id);
        self.fill_state.insert(
            order_id,
            FillState { total: total_qty, qty: total_qty, filled: 0.0, venue_filled: 0.0 },
        );
    }

    /// Whether an order this connection wrote has had no answer from IB for `wait_ms` since the loop's first turn after its
    /// write: then the exec loop reads the venue's open orders ONCE, and a row listing it working accepts it
    /// (`on_venue_order`, the rebind of an order it holds). Each order is due once; one answered (its accept or its end
    /// took it out of `unacked`) is forgotten here.
    pub(crate) fn silent_submit_due(&mut self, now_ms: i64, wait_ms: i64) -> bool {
        for id in &self.unacked {
            self.silent_since.entry(*id).or_insert(now_ms);
        }
        let (unacked, mut due) = (&self.unacked, false);
        self.silent_since.retain(|id, since| {
            if *since != ASKED && now_ms - *since >= wait_ms {
                *since = ASKED;
                due = true;
            }
            unacked.contains(id)
        });
        due
    }

    /// Count the executions of `order_id` from now on ([`EventMapper::executed`]).
    pub(crate) fn watch_executions(&mut self, order_id: i32) {
        self.executions.insert(order_id, 0.0);
    }

    pub(crate) fn unwatch_executions(&mut self, order_id: i32) {
        self.executions.remove(&order_id);
    }

    /// What of the watched `order_id` has executed. Counted when the execution arrives, NOT when its commission joins, and
    /// it outlives the order's end on this connection.
    pub(crate) fn executed(&self, order_id: i32) -> f64 {
        self.executions.get(&order_id).copied().unwrap_or(0.0)
    }

    /// What of the watched `order_id` has not executed as far as this connection knows, `None` once the order has ended here.
    pub(crate) fn unexecuted(&self, order_id: i32) -> Option<f64> {
        // The id, because a rejected order keeps its `fill_state`.
        self.ids.order_client_id_of(order_id)?;
        self.fill_state.get(&order_id).map(|st| (st.total - self.executed(order_id)).max(0.0))
    }

    /// The venue holds `new_qty` as the total quantity of `order_id` (an amend it confirmed):
    /// the fill tracking follows. Without this a quantity amend 10 -> 12 makes the fill of 10 look final (the
    /// order retired, its id forgotten, the last 2 lots unroutable) and 10 -> 6 never reaches `filled >= total`.
    /// The total moves by the CHANGE in quantity, so what a learned reduction took off it (`settle_order`) stays
    /// off, and it never drops below what is already filled (the venue does not shrink an order below its
    /// fills; if it ever does, the order is complete at the venue and the next fill, if any, is final).
    fn follow_venue_qty(&mut self, order_id: i32, new_qty: f64) {
        if !(new_qty.is_finite() && new_qty > 0.0) {
            return;
        }
        let Some(st) = self.fill_state.get_mut(&order_id) else { return };
        if (new_qty - st.qty).abs() <= 1e-9 * new_qty.max(st.qty).max(1.0) {
            return;
        }
        let wanted = st.total + (new_qty - st.qty);
        if wanted + 1e-9 < st.filled {
            tracing::warn!(
                order_id,
                new_qty,
                filled = st.filled,
                "ibkr: the venue holds a quantity below what this order has already filled; the order is \
                 complete at the venue (the total is kept at the filled quantity)"
            );
        }
        st.total = wanted.max(st.filled);
        st.qty = new_qty;
    }

    /// A backend vouched for an amend of `order_client_id` without an echo (`AmendOutcome::Acknowledged`): the quantity
    /// and price it asked for are what the venue holds now.
    pub(crate) fn amend_acknowledged(
        &mut self,
        order_client_id: &str,
        order_id: i32,
        qty: f64,
        price: Option<f64>,
    ) {
        self.amends.record_confirmed(order_client_id, qty, price);
        self.follow_venue_qty(order_id, qty);
    }

    fn resolve(&self, order_id: i32, order_ref: &str) -> Option<String> {
        self.ids.resolve(order_id, order_ref)
    }

    /// Remember the canonical symbol `order_client_id` was submitted under (the exec loop at the submit, the host when it
    /// teaches a restored order). The fills of the order carry THIS symbol: the engine drops a bare `Fill`
    /// whose symbol it neither mounts nor registered the order under, and IB's own spelling of the instrument
    /// (the exec contract's symbol/exchange/currency) is not the canonical for a forex pair or a stock executed
    /// on a named exchange. See [`EventMapper::fill_symbol`].
    pub(crate) fn register_symbol(&mut self, order_client_id: &str, symbol: &str) {
        self.symbols.record(order_client_id, symbol);
    }

    /// The symbol a fill of `order_client_id` carries: the one the order was submitted/registered under, else (an order PLACED on
    /// this connection whose symbol was empty or past the table's cap; an order rebound from the venue and never
    /// taught, or an execution replayed for an order the venue no longer lists, never gets here: it emits no fill,
    /// see [`EventMapper::is_untaught`] and [`EventMapper::on_execution_details`]) the instrument IB echoed, with a `warn!`, because
    /// `vike_execution::Engine` drops a fill whose symbol it does not mount in silence (no log, no counter).
    fn fill_symbol(&self, order_client_id: &str, echoed: &str) -> String {
        match self.symbols.symbol_of(order_client_id) {
            Some(registered) => {
                if registered != echoed && !echo_names(&registered, echoed) {
                    tracing::warn!(
                        order_client_id,
                        registered = %registered,
                        echoed,
                        "ibkr: the execution names a different instrument than the order was submitted \
                         under; the fill carries the SUBMITTED symbol (the engine drops a fill whose \
                         symbol it neither mounts nor registered the order under)"
                    );
                }
                registered
            }
            None => {
                tracing::warn!(
                    order_client_id,
                    echoed,
                    "ibkr: no submitted symbol is known for this order (placed here with no symbol, or past \
                     the symbol table's cap): the fill carries the instrument IB echoed, and an engine that \
                     does not mount that exact symbol DROPS it silently"
                );
                echoed.to_string()
            }
        }
    }

    /// Whether `order_client_id` is an order this connection REBOUND from the venue's open orders and the host never taught the
    /// symbol of. Such an order is TRACKED like any other (its binding, its fill tracking, its status and cancel
    /// all work, and a later `register_symbol` makes it taught), but a fill of it carries only the instrument IB
    /// echoed - and whether the engine books or drops that is decided by the spelling alone, so NO fill event is
    /// emitted: the fact reaches the books through the reconcile pass (`MissingFill` / `PositionDrift`) and the
    /// operator's `recon confirm`, exactly like the replay of a finished order nobody taught
    /// ([`EventMapper::on_execution_details`]). An order placed on THIS connection is never untaught.
    fn is_untaught(&self, order_client_id: &str) -> bool {
        self.rebound.contains(order_client_id) && self.symbols.symbol_of(order_client_id).is_none()
    }

    /// The one `info!` line per execution whose fill is withheld because its order is untaught.
    fn log_untaught_fill(
        order_client_id: &str,
        exec_id: &str,
        side: i32,
        qty: f64,
        price: f64,
        echoed: &str,
    ) {
        tracing::info!(
            order_client_id = %order_client_id,
            exec_id = %exec_id,
            side = if side > 0 { "buy" } else { "sell" },
            qty,
            price,
            echoed = %echoed,
            "ibkr: execution of an order nobody taught this connection (rebound from the venue's open orders) - \
             its fill is NOT emitted (its symbol is unknown, so it could only carry the instrument IB echoed); \
             if the engine had not already booked it, the reconcile pass reports it as a MissingFill or \
             PositionDrift for the operator to confirm"
        );
    }

    /// Remember that `order_client_id` reached a terminal on this connection and drop what is kept per order.
    fn retire(&mut self, order_client_id: &str) {
        self.symbols.forget(order_client_id);
        self.rebound.remove(order_client_id);
        self.amends.forget(order_client_id);
        if self.finished.len() >= MAX_RECENT_FINISHED {
            self.finished.pop_front();
        }
        self.finished.push_back(order_client_id.to_string());
    }

    /// Whether `order_client_id` reached a terminal on THIS connection moments ago (the last
    /// [`MAX_RECENT_FINISHED`] terminals): the cancel of it is a race lost, not an unknown order.
    pub(crate) fn recently_finished(&self, order_client_id: &str) -> bool {
        self.finished.iter().any(|c| c == order_client_id)
    }

    /// The open-order snapshot of this connection is not known (see the field): a new order must be refused.
    pub(crate) fn orders_unknown(&self) -> bool {
        self.orders_unknown
    }

    /// Keep an amend that is about to be written; an older unanswered amend of the same order is closed with
    /// the advisory this returns (`execution::amend`).
    pub(crate) fn note_amend(
        &mut self,
        order_client_id: &str,
        pending: PendingAmend,
    ) -> Option<Event> {
        self.amends.note(order_client_id, pending)
    }

    /// Take the amend of `order_client_id` back out: the write failed, or the backend vouched for it synchronously.
    pub(crate) fn take_amend(&mut self, order_client_id: &str) -> Option<PendingAmend> {
        self.amends.take(order_client_id)
    }

    /// Close the amends that waited `wait_ms` for an echo that did not come (the loop's upkeep).
    pub(crate) fn sweep_amends(&mut self, now_ms: i64, wait_ms: i64) -> Vec<Event> {
        self.amends.sweep(now_ms, wait_ms)
    }

    /// Emit OrderAccepted once per order client id (idempotent) — used both by a real Submitted/PreSubmitted
    /// status AND by the exec-before-open synthesis path.
    fn ensure_accepted(&mut self, order_client_id: &str, order_id: i32) -> Option<Event> {
        // Active/filling ⇒ acked: it leaves the unacked set so a later id-less rejection can no
        // longer be misattributed to it.
        self.unacked.remove(&order_id);
        if self.accepted.insert(order_client_id.to_string()) {
            // An order this connection placed (a rebound one is a previous connection's): IB's end right behind this
            // accept may be a refusal whose words are still on their way (`fresh_ends`).
            if let Some(now) = self.clock_ms
                && !self.rebound.contains(order_client_id)
            {
                self.fresh_accepts.insert(order_id, now);
            }
            Some(Event::OrderAccepted(OrderAccepted {
                order_client_id: order_client_id.to_string(),
                venue_order_id: Some(order_id.to_string().into()),
                timestamp: 0,
            }))
        } else {
            None
        }
    }

    pub(crate) fn on_order_status(&mut self, s: IbkrOrderStatus) -> Vec<Event> {
        let Some(order_client_id) = self.resolve(s.order_id, &s.order_ref) else {
            return vec![]; // already terminal/forgotten, or unknown external order
        };
        let kind = OrderStatusKind::from_ibkr(&s.status);
        let mut out = vec![];
        if kind.is_active()
            && let Some(event) = self.ensure_accepted(&order_client_id, s.order_id)
        {
            out.push(event);
        }
        // ⚠ **IB's `Inactive`, and a word nobody documents (the owner's ruling: an unknown IBKR status is never resting), end
        // the order here too.** This path used to emit nothing for them, true in recon alone: `PendingSubmit` or `PreSubmitted`
        // gave `Accepted` (which empties `unacked`), then IB said `Inactive`, and the order read `Accepted` for ever. Never
        // accepted it is a reject; accepted, the FSM refuses a reject, so it ends as a cancel. An empty word says nothing.
        let inactive = kind == OrderStatusKind::Inactive && !s.status.is_empty();
        let reason =
            if inactive { order_over_words(&order_client_id, &s.status) } else { String::new() };
        if inactive && self.unacked.contains(&s.order_id) {
            return self.reject(s.order_id, order_client_id, &reason);
        }
        if inactive || matches!(kind, OrderStatusKind::Cancelled | OrderStatusKind::ApiCancelled) {
            if self.fresh_ends.contains_key(&s.order_id) {
                return out; // IB says the end it already said; the held one stands
            }
            // IB's refusal of this order, said before its status: the end carries IB's words. Said after: behind an end
            // IB made right after the accept it is waited for (`fresh_ends`); behind any other it is only logged
            // (`on_error`), and the end goes out at once, as it always did.
            if let Some(r) = self.refusals.remove(&s.order_id) {
                out.extend(self.end_over(s.order_id, order_client_id, r.words));
            } else if let Some(held_ms) = self.hold_fresh_end(s.order_id) {
                self.fresh_ends.insert(s.order_id, FreshEnd { order_client_id, reason, held_ms });
            } else {
                out.extend(self.end_over(s.order_id, order_client_id, reason));
            }
        }
        // Filled status is folded from execDetails+commission (below), not here (avg-price-only
        // status can't carry the exec_id vike's FillEvent needs).
        out
    }

    /// End `order_id` as over at the venue: an `OrderCanceled` carrying `reason`, or held back while a fill of it
    /// still waits for its commission.
    ///
    /// Fills still awaiting their commission: the cancel is HELD back for them (a bounded time,
    /// `release_held_cancels`) rather than flushing the fills with an invented fee. The real
    /// commission follows the execution within milliseconds, and the fill carries it when it does;
    /// the terminal then follows the fills, which is the order the engine's FSM needs
    /// ([...PartiallyFilled, Canceled]). `commissions` is deliberately NOT swept here: a parked
    /// commission is keyed only by exec_id and its execution never arrived, so there is nothing to
    /// attribute it to this order_id with. It expires on its own terms - when its execDetails finally
    /// arrives and resolves to no local order (this cancel forgot the id), `on_execution_details`
    /// releases it with a `warn!`.
    fn end_over(&mut self, order_id: i32, order_client_id: String, reason: String) -> Vec<Event> {
        if self.has_unjoined(order_id) {
            tracing::debug!(
                order_client_id = %order_client_id,
                order_id,
                "ibkr: cancel held back until the commission of the order's fill(s) arrives"
            );
            self.held_cancels.entry(order_id).or_insert_with(|| HeldCancel {
                order_client_id,
                first_seen_ms: None,
                asked_ms: None,
                reason,
            });
            return vec![];
        }
        self.end_as_cancelled(order_id, &order_client_id, reason)
    }

    pub(crate) fn on_execution_details(&mut self, e: IbkrExecutionDetails) -> Vec<Event> {
        // The IB `execution_id` IS this venue's `trade_id` (module doc), and this mapper's ENTIRE
        // dedup structure is keyed on it: `emitted` (which is what makes a `reqExecutions` replay
        // safe — trap 5 returns the whole day indiscriminately), `pending`, `commissions`, and the
        // engine's own `seen_trade_ids` downstream. Refused at INGRESS rather than at the two
        // `FillEvent` sites, because an id-less execution would poison all four maps under one
        // shared `""` key: distinct executions would overwrite each other's pending halves, and the
        // replayed copies would look already-emitted (or re-emit) at random. Nothing is synthesized
        // in its place — `order_id` is per-ORDER, so an order_id-keyed fill would swallow every
        // later fill of the same order. IB always sets `execId`, so this is a malformed-frame path.
        let Ok(trade_id) = TradeId::new(&e.exec_id) else {
            tracing::warn!(
                order_id = e.order_id,
                "ibkr: execDetails carries no `execId` — dropping it; that id is this venue's fill \
                 dedup key, and an id-less execution cannot be replay-deduped or joined to its \
                 commissionReport"
            );
            return vec![];
        };
        if self.emitted.contains_key(&e.exec_id) {
            // Re-delivery of an execution whose fill already emitted — the ordinary case after a
            // `reqExecutions` replay (module doc trap 5), which returns the whole day
            // indiscriminately. Dropping it here is what keeps the replay safe: re-buffering it
            // would let the replayed commission re-join and re-advance `fill_state` (see the
            // `emitted` field doc). Nothing is lost — the fill was already emitted once.
            tracing::debug!(
                exec_id = %e.exec_id,
                order_id = e.order_id,
                "ibkr: re-delivered execDetails for an already-emitted fill — ignored"
            );
            return vec![];
        }
        let (order_client_id, recovered) = match self.resolve(e.order_id, &e.order_ref) {
            Some(order_client_id) => (order_client_id, false),
            // An execution of THIS client's order (the exec loop checked the client id) that this
            // connection knows nothing about, replayed while it learns the venue: the order FINISHED while
            // the connection was down, and this is a fill it missed. It is recovered by its orderRef (our
            // order client id) as a PARTIAL (nothing here says how the order ended), changing no bookkeeping; the
            // engine's trade-id dedup drops what it already folded, and applies what it had not.
            // ONLY for an order the host TAUGHT a symbol: the fill then carries the symbol the engine mounts. An
            // untaught one would carry the instrument IB echoes (`AAPL.IBEOS.USD` for an overnight execution of an
            // `AAPL.SMART.USD` order), and whether the engine books it or drops it would be decided by that spelling
            // alone - so it is not emitted at all, and falls to the unroutable arm below.
            None if e.recoverable
                && !e.order_ref.is_empty()
                && self.symbols.symbol_of(&e.order_ref).is_some() =>
            {
                (e.order_ref.clone(), true)
            }
            None => {
                if e.recoverable
                    && !e.order_ref.is_empty()
                    && !self.unroutable.contains_key(&e.exec_id)
                {
                    tracing::info!(
                        order_client_id = %e.order_ref,
                        exec_id = %e.exec_id,
                        side = if e.side_buy { "buy" } else { "sell" },
                        qty = e.shares,
                        price = e.price,
                        echoed = %e.symbol,
                        "ibkr: replayed execution of an order nobody taught this connection - nothing emitted \
                         (its symbol is unknown, so the fill could only carry the instrument IB echoed); if the \
                         engine had not already booked it, the reconcile pass reports it as a MissingFill or \
                         PositionDrift for the operator to confirm"
                    );
                }
                // Unknown / already-forgotten order (an external fill, or one arriving after this
                // order's terminal): there is no order client id to attribute it to, so nothing can be emitted.
                // This drop is PRE-EXISTING; it is only logged now, because it is also the
                // DEFINITIVE EXPIRY for a parked commission — the execution has now ARRIVED and is
                // unroutable, so its parked half can never join and is released here rather than held
                // for the process lifetime.
                //
                // Event-driven, not clock-driven, deliberately: this mapper is a pure, fixture-tested
                // fold with no clock, and `IbkrCommissionReport` carries no timestamp to age by. A
                // `SystemTime::now()` sweep would buy a weaker guarantee at the cost of making every
                // fixture test time-dependent.
                //
                // The execution is also REMEMBERED as unroutable, so that a commission arriving AFTER it
                // is dropped instead of parked: a fresh connection replays the whole day's executions,
                // one commission each, and for the orders that finished before this connection nothing can
                // ever join them (review of W14b, finding 6).
                self.note_unroutable(e.exec_id.clone());
                if self.commissions.remove(&e.exec_id).is_some() {
                    tracing::warn!(
                        exec_id = %e.exec_id,
                        order_id = e.order_id,
                        "ibkr: releasing parked commission — its execDetails resolved to no local \
                         order, so the pair can never join"
                    );
                } else {
                    tracing::debug!(
                        exec_id = %e.exec_id,
                        order_id = e.order_id,
                        "ibkr: execDetails for an unknown/forgotten order — nothing emitted"
                    );
                }
                return vec![];
            }
        };
        // Routable now (an earlier delivery of it may not have been: the order may have been learned
        // since): its commission joins it as usual.
        self.unroutable.remove(&e.exec_id);
        let mut out = vec![];
        // Exec-before-open: a fill before ACCEPTED synthesizes the accept to backfill the mapping. NOT for a
        // recovered fill: that order is not one this connection holds (it finished at the venue while the
        // connection was down), so an accept for it would be an unfounded claim that it is alive. On an engine
        // order that is already terminal it is dropped as a liveness event onto a killed order (a
        // `stranded_terminal_drops` count and a "position may be STRANDED" error at EVERY replay), and its
        // numeric `order_id` is the PREVIOUS connection's: `ensure_accepted` would remove that number from
        // `unacked`, which on this connection may be a different, live order's.
        // The order is tracked under the id THIS connection bound for the order client id, not under the number the
        // execution names: an execution resolved by its `orderRef` can carry another number, and every
        // per-order structure (unacked, fill tracking, the cancel's flush) is keyed on the bound one.
        let order_id = if recovered {
            e.order_id
        } else {
            self.ids.order_id_of(&order_client_id).unwrap_or(e.order_id)
        };
        if !recovered && let Some(event) = self.ensure_accepted(&order_client_id, order_id) {
            out.push(event);
        }
        // Once per execution: a re-delivery of one still waiting for its commission replaces its entry below, and one whose
        // fill went out never gets here (`emitted`).
        if !recovered
            && !self.pending.contains_key(&e.exec_id)
            && let Some(executed) = self.executions.get_mut(&order_id)
        {
            *executed += e.shares;
        }
        // An order REBOUND from the venue and never taught (see `is_untaught`): the execution is tracked and joined
        // like any other, but its fill is not emitted (`join_fill`), so there is no symbol to stamp or to warn about.
        let untaught = self.is_untaught(&order_client_id);
        let symbol =
            if untaught { e.symbol.clone() } else { self.fill_symbol(&order_client_id, &e.symbol) };
        let p = PendingFill {
            order_id,
            trade_id,
            order_client_id,
            symbol,
            side: if e.side_buy { 1 } else { -1 },
            shares: e.shares,
            price: e.price,
            ts: e.ts,
            recovered,
            untaught,
            // Unstamped: the mapper has no clock, so the first `sweep_pending` stamps it. An entry
            // that joins before any sweep observes it is never stamped at all.
            first_seen_ms: None,
            recovery_attempts: 0,
            escalated: false,
        };
        // Commission-first: its half is already parked, so the pair completes HERE. Before the
        // reverse index existed this branch did not — the commission had been discarded on
        // arrival, and this execution buffered into `pending` awaiting a commission that had
        // already come and gone, so its fill was never emitted at all.
        if let Some(pc) = self.commissions.remove(&e.exec_id) {
            out.extend(self.join_fill(&e.exec_id, p, pc.commission, &pc.currency));
            return out;
        }
        // Exec-first: buffer the fill by exec_id; it emits when its commission joins.
        self.pending.insert(e.exec_id, p);
        out
    }

    pub(crate) fn on_commission_report(&mut self, c: IbkrCommissionReport) -> Vec<Event> {
        if self.emitted.contains_key(&c.exec_id) {
            // A commission for an exec whose fill already emitted: a late real report for a
            // cancel-flushed partial (booked with its fee unknown), IB REVISING a commission it sent, an IB
            // re-send, or a `reqExecutions` replay. The one that states a fee the engine was not told becomes an
            // `Event::FeeAdjustment` (decision 0238); every other is a no-op — and crucially NOT parked, so a
            // replay cannot leak one `commissions` entry per already-emitted fill.
            return self.late_commission(c);
        }
        if self.unroutable.contains_key(&c.exec_id) {
            // The commission of an execution that resolved to no local order (see `note_unroutable`):
            // nothing can join it, so it is dropped, not parked.
            tracing::debug!(
                exec_id = %c.exec_id,
                "ibkr: commissionReport for an execution that matched no local order — dropped"
            );
            return vec![];
        }
        let Some(p) = self.pending.remove(&c.exec_id) else {
            // NOT a drop any more. This arm used to `return vec![]`, on the assumption that a
            // commission can only precede its execution when IB re-sends. It cannot: IBKR
            // delivers the two halves in either order by design (module doc trap 2, quoting
            // `ibapi` 3.2.1's `CommissionReport` docs).
            // Discarding the commission ALSO stranded the execution that arrived afterwards — it
            // sat in `pending` forever, reachable by no sweep (the only one is `on_order_status`'s
            // cancel arm, and a filled order is not cancelled; reconnect re-requests OPEN orders
            // only, and a filled order is not open). So the fill never emitted: position and
            // realized PnL silently disagreed with the venue. Park the half we have;
            // `on_execution_details` completes it.
            self.park_commission(c);
            return vec![];
        };
        self.join_fill(&c.exec_id, p, c.commission, &c.currency)
    }

    /// A `commissionReport` for an execution whose fill already went out: the fee the engine holds for it is
    /// now stated, or revised. Emits an `Event::FeeAdjustment` carrying how much MORE the fee is than the fill
    /// carried, IN TOTAL, so the engine books the difference to what it already booked and the same figure again
    /// books nothing (`vike_execution::Account::apply_fee_adjustment`); emits nothing when the report states the fee
    /// the engine was already told.
    ///
    /// Nothing is emitted, and the reason is said, for a report this venue's shapes cannot make a correction of:
    /// a non-finite commission; a currency other than the one the fee was stated in (the correction is an amount
    /// in THAT currency); and an exactly-`0.0` report over a fee already non-zero, because an unset protobuf field
    /// is indistinguishable from a real zero (module doc) and must never zero out a fee the fill carried.
    ///
    /// ⚠ The memory is this mapper's: a rebuilt connection starts with none, so a revision that arrives after a
    /// rebuild is not told (its replayed pair is the engine's `seen_trade_ids` duplicate).
    fn late_commission(&mut self, c: IbkrCommissionReport) -> Vec<Event> {
        let Some(book) = self.fee_book.get_mut(&c.exec_id) else {
            // An untaught fill (nothing was emitted for it) or an execution past the book's bound: as before.
            return vec![];
        };
        if !c.commission.is_finite() {
            tracing::warn!(
                exec_id = %c.exec_id,
                commission = c.commission,
                "ibkr: a commissionReport for an already-emitted fill carries a non-finite commission - ignored, \
                 the engine's fee for this execution is unchanged"
            );
            return vec![];
        }
        let stated = if book.fill_asset.is_empty() { &book.told_asset } else { &book.fill_asset };
        if !stated.is_empty() && !c.currency.is_empty() && !stated.eq_ignore_ascii_case(&c.currency)
        {
            tracing::warn!(
                exec_id = %c.exec_id,
                commission = c.commission,
                currency = %c.currency,
                stated_in = %stated,
                "ibkr: a commissionReport for an already-emitted fill states its fee in another currency than \
                 the one the engine holds it in - ignored (a correction is an amount in the fee's own currency); \
                 reconcile the fee by hand"
            );
            return vec![];
        }
        if c.commission == 0.0 && (book.fill_commission != 0.0 || book.told_extra != 0.0) {
            tracing::warn!(
                exec_id = %c.exec_id,
                fill_commission = book.fill_commission,
                told_extra = book.told_extra,
                "ibkr: a commissionReport of exactly 0.0 arrived for a fill whose fee is already non-zero - ignored \
                 (an unset field is indistinguishable from a real zero, and must not undo a fee)"
            );
            return vec![];
        }
        let extra = c.commission - book.fill_commission;
        if (extra - book.told_extra).abs() < FEE_EPS {
            return vec![]; // the fee the engine already holds: a re-send or a replay
        }
        let previous = book.told_extra;
        book.told_extra = extra;
        if book.told_asset.is_empty() {
            book.told_asset = c.currency.clone();
        }
        let asset =
            if book.fill_asset.is_empty() { c.currency.as_str() } else { book.fill_asset.as_str() };
        tracing::info!(
            exec_id = %c.exec_id,
            order_client_id = %book.order_client_id,
            commission = c.commission,
            fill_commission = book.fill_commission,
            extra,
            previous,
            currency = %asset,
            "ibkr: the commission of an execution whose fill went out earlier is now stated (or revised): \
             a fee adjustment is sent so the account books the difference"
        );
        vec![Event::FeeAdjustment(FeeAdjustmentEvent {
            trade_id: book.trade_id.clone(),
            order_client_id: book.order_client_id.clone(),
            venue: ustr_lite("ibkr"),
            symbol: ustr_lite(&book.symbol),
            last_price: book.px,
            extra_commission: extra,
            commission_asset: ustr_lite(asset),
            timestamp: book.ts,
        })]
    }

    /// Park a commission whose execution has not arrived yet. Bounded by
    /// [`MAX_PARKED_COMMISSIONS`]: at the cap the OLDEST park is evicted and `warn!`ed.
    ///
    /// **Oldest-first is the correct direction here** (the mirror of #937's front-trim, for the
    /// opposite reason): the oldest park has had the longest opportunity for its execDetails to
    /// arrive and still has not, so it is the likeliest to be genuinely orphaned, while the
    /// newest parks are precisely the ones still mid-race. The `min_by_key` scan is O(n), but it
    /// runs ONLY at the cap.
    fn park_commission(&mut self, c: IbkrCommissionReport) {
        // A duplicate commission replaces its own entry and cannot grow the map, so it must not
        // evict anything.
        if self.commissions.len() >= MAX_PARKED_COMMISSIONS
            && !self.commissions.contains_key(&c.exec_id)
        {
            // Resolved to an OWNED key first, so the `iter()` borrow of `self.commissions` is
            // provably over before the `remove` below takes it mutably.
            let oldest =
                self.commissions.iter().min_by_key(|(_, pc)| pc.seq).map(|(id, _)| id.clone());
            if let Some(oldest) = oldest {
                self.commissions.remove(&oldest);
                // The ONE path that can still lose something, so it is loud. What is lost is a
                // FEE figure, not a fill — unless that execution does eventually arrive, in which
                // case it buffers into `pending` and strands (the documented residual; the cap is
                // ~5x above any realistic in-flight depth, so reaching it is already pathological
                // and this line is the evidence).
                tracing::warn!(
                    exec_id = %oldest,
                    cap = MAX_PARKED_COMMISSIONS,
                    "ibkr: parked-commission cap reached — evicted the oldest unjoined commission; \
                     if its execDetails still arrives, that fill will strand in `pending`"
                );
            }
        }
        let seq = self.commission_sequence;
        self.commission_sequence += 1;
        tracing::debug!(
            exec_id = %c.exec_id,
            "ibkr: commissionReport arrived before its execDetails — parked for join"
        );
        self.commissions.insert(
            c.exec_id,
            PendingCommission { commission: c.commission, currency: c.currency, seq },
        );
    }

    /// Record that a fill has been emitted for `exec_id`, so any re-delivered half of that pair is
    /// a no-op. Bounded by [`MAX_EMITTED_EXECUTION_IDS`], evicting the OLDEST first — the oldest
    /// execution is the least likely to still be replayed, and (unlike a `pending` eviction) losing
    /// the record cannot lose a fill.
    fn note_emitted(&mut self, exec_id: String) {
        if self.emitted.len() >= MAX_EMITTED_EXECUTION_IDS && !self.emitted.contains_key(&exec_id) {
            // Resolve to an OWNED key first so the `iter()` borrow ends before the `remove`.
            let oldest = self.emitted.iter().min_by_key(|(_, seq)| **seq).map(|(id, _)| id.clone());
            if let Some(oldest) = oldest {
                self.emitted.remove(&oldest);
            }
        }
        let seq = self.emitted_sequence;
        self.emitted_sequence += 1;
        self.emitted.insert(exec_id, seq);
    }

    /// Age every buffered-but-unjoined execution against `now_ms` and report what to do about the
    /// ones past `grace_ms`. Called by the exec loop on a fixed cadence; **emits no events and
    /// removes nothing** — it only annotates `pending` and hands the caller a decision.
    ///
    /// `now_ms` is an opaque MONOTONIC millisecond stamp: only differences of it are used, so the
    /// caller should derive it from an `Instant` (immune to wall-clock jumps) and tests can pass
    /// any literal. Nothing in this module reads a clock (module doc).
    ///
    /// ## Why this exists (module doc trap 5)
    /// The fill is emitted exclusively by the commission join, so an execution whose
    /// commissionReport never arrives — on an order that is never cancelled — used to be held
    /// forever and its fill never emitted: position and realized PnL silently short, with no trace.
    ///
    /// ## Why it recovers rather than emits
    /// The three shapes that "just emit it" could take are all worse:
    /// - `commission: 0.0` — silently understates cost basis on every recovered fill. A silent
    ///   wrong number is worse than a loud missing one, and this is the one thing the fix must not
    ///   do.
    /// - a commission-optional `FillEvent` — `commission` is a bare `f64` with no way to say
    ///   "unknown"; giving it one is a `vike-model` wire-schema change touching every venue, the
    ///   journal and the parity fixtures. Out of this crate. It remains the ONLY way to close the
    ///   final residual below.
    /// - flush on a `Filled` order status — unsafe, and already ruled out: that status routinely
    ///   arrives BETWEEN the two halves, so it would turn ordinary fills into zero-commission
    ///   partials and desynchronise `fill_state`.
    ///
    /// The venue has the number, so ask for it. `reqExecutions` re-delivers the current day's
    /// executions AND their commission reports, whereupon the ordinary join emits the fill with the
    /// REAL commission. Each entry gets [`MAX_PENDING_RECOVERY_ATTEMPTS`] requests, each restarting
    /// its clock; the entry is NEVER evicted, so even a very late commissionReport still emits it
    /// correctly.
    ///
    /// ## The residual, stated rather than hidden
    /// If IBKR genuinely has no commissionReport for an execution (or it aged past the current-day
    /// window `reqExecutions` is limited to), the fill still does not emit — it is reported ONCE as
    /// a [`StrandedFill`] carrying everything an operator needs, and held. Closing that last case
    /// needs the commission-optional `FillEvent` above.
    pub(crate) fn sweep_pending(&mut self, now_ms: i64, grace_ms: i64) -> PendingSweep {
        let mut out = PendingSweep::default();
        for (exec_id, p) in self.pending.iter_mut() {
            let first = *p.first_seen_ms.get_or_insert(now_ms);
            if now_ms.saturating_sub(first) < grace_ms {
                continue;
            }
            if p.recovery_attempts < MAX_PENDING_RECOVERY_ATTEMPTS {
                p.recovery_attempts += 1;
                p.first_seen_ms = Some(now_ms); // restart the clock for the next attempt
                out.recover.push(exec_id.clone());
            } else if !p.escalated {
                p.escalated = true;
                out.stranded.push(StrandedFill {
                    exec_id: exec_id.clone(),
                    order_client_id: p.order_client_id.clone(),
                    order_id: p.order_id,
                    symbol: p.symbol.clone(),
                    side: p.side,
                    shares: p.shares,
                    price: p.price,
                });
            }
        }
        // `HashMap` iteration order is random; sort so the result (and every log line built from
        // it) is deterministic.
        out.recover.sort();
        out.stranded.sort_by(|a, b| a.exec_id.cmp(&b.exec_id));
        out
    }

    /// Mint the joined `Fill` for `exec_id` and decide partial-vs-final. The ONE place a joined
    /// `FillEvent` is built, reached from BOTH arrival orders — so neither ordering can drift from
    /// the other in what it emits or in how it advances `fill_state`.
    fn join_fill(
        &mut self,
        exec_id: &str,
        p: PendingFill,
        commission: f64,
        currency: &str,
    ) -> Vec<Event> {
        let fill = FillEvent {
            // Validated at ingress and carried on the entry — see `PendingFill`. Equal to `exec_id`
            // by construction (the entry was stored under it), so the joined fill and the
            // cancel-flush fill cannot disagree about the dedup key.
            trade_id: p.trade_id.clone(),
            order_client_id: p.order_client_id.clone(),
            venue: ustr_lite("ibkr"),
            symbol: ustr_lite(&p.symbol),
            side: p.side,
            last_qty: p.shares,
            last_price: p.price,
            commission,
            commission_asset: ustr_lite(currency),
            liquidity_side: Default::default(),
            timestamp: p.ts,
            mark_price: None,
            position_side: Default::default(),
        };
        const EPS: f64 = 1e-9;
        // Record the emit BEFORE the partial-vs-final decision: a re-delivered half of this pair
        // must be a no-op on both sides from here on (see the `emitted` field doc).
        self.note_emitted(exec_id.to_string());
        // What the engine is told the fee is, kept so a LATER report for this execution (IB revising it) can be
        // told as a correction. Not for the fill of an order nobody taught (below): it emits nothing.
        if !p.untaught {
            self.note_fee_book(
                exec_id,
                FeeBook {
                    trade_id: p.trade_id.clone(),
                    order_client_id: p.order_client_id.clone(),
                    symbol: p.symbol.clone(),
                    px: p.price,
                    ts: p.ts,
                    fill_commission: commission,
                    fill_asset: currency.to_string(),
                    told_extra: 0.0,
                    told_asset: String::new(),
                    seq: 0,
                },
            );
        }
        let order_id = p.order_id;
        // A recovered fill (an order that finished while the connection was down) is always a partial and
        // touches no bookkeeping: its `order_id` may even be another order's.
        if !p.recovered
            && let Some(st) = self.fill_state.get_mut(&order_id)
        {
            st.filled += p.shares;
            // The learning ended while this order's last commission was still outstanding: its totals were
            // not settled then (they would have been settled against a `filled` that lacked this fill and
            // this fill counted twice), so they are settled now that nothing of it is unjoined.
            if self.learning == Learning::Done && !self.has_unjoined(order_id) {
                self.settle_order(order_id);
            }
        }
        let final_fill = if p.recovered {
            false
        } else {
            match self.fill_state.get(&order_id) {
                Some(st) => st.filled + EPS >= st.total,
                None => true, // unknown total → terminal (external/rebound; preserves prior behaviour)
            }
        };
        if final_fill {
            self.ids.forget(order_id);
            self.accepted.remove(&p.order_client_id);
            self.unacked.remove(&order_id);
            self.fill_state.remove(&order_id);
            self.retire(&p.order_client_id);
            // A cancel that waited for this fill's commission is moot: the order completed.
            self.held_cancels.remove(&order_id);
            self.note_ended(order_id, &p.order_client_id);
        }
        let held_order_client_id = (!p.recovered && !final_fill)
            .then(|| self.held_cancels.get(&order_id).map(|h| h.order_client_id.clone()))
            .flatten();
        // `[Fill, wrap]`, like every other bridge (`vike_bridge_core::terminal_events`): the bare
        // `Fill` is what reaches `Account::apply_fill` (position, realized PnL, commission), the
        // wrapper only advances the order's FSM. Both carry the same `trade_id` (the IB execution
        // id), the dedup key of both folds.
        // ...except the fill of an order nobody taught this connection (rebound from the venue, `is_untaught`): all of
        // the bookkeeping above ran (its fill tracking, the dedup, the retirement of an order that completed), but
        // no event leaves the bridge for it - nothing for the engine to book or drop by spelling, and no repeat
        // record for a fact the engine already folded. The order's cancel below is not a fill and still goes out.
        let mut out = if p.untaught {
            Self::log_untaught_fill(
                &p.order_client_id,
                exec_id,
                p.side,
                p.shares,
                p.price,
                &p.symbol,
            );
            Vec::new()
        } else {
            terminal_events(p.order_client_id, fill, p.ts, final_fill)
        };
        // The cancel held back for the commissions of this order's fills goes out behind the last of them.
        if let Some(order_client_id) = held_order_client_id
            && !self.has_unjoined(order_id)
        {
            out.extend(self.finish_cancel(order_id, &order_client_id));
        }
        out
    }

    /// Whether a fill of `order_id` (an order of THIS connection, not a recovered one) still waits for its
    /// commission.
    fn has_unjoined(&self, order_id: i32) -> bool {
        self.pending.values().any(|pf| !pf.recovered && pf.order_id == order_id)
    }

    /// The venue's cancel of `order_client_id`, all its fills out: `OrderCanceled`, and everything kept per order dropped.
    fn finish_cancel(&mut self, order_id: i32, order_client_id: &str) -> Vec<Event> {
        let reason = self.held_cancels.remove(&order_id).map(|h| h.reason).unwrap_or_default();
        self.end_as_cancelled(order_id, order_client_id, reason)
    }

    /// The `OrderCanceled` of `order_client_id`, in `reason`, and everything kept for it dropped.
    fn end_as_cancelled(
        &mut self,
        order_id: i32,
        order_client_id: &str,
        reason: String,
    ) -> Vec<Event> {
        self.held_cancels.remove(&order_id);
        self.note_ended(order_id, order_client_id);
        self.ids.forget(order_id);
        self.unacked.remove(&order_id);
        self.accepted.remove(order_client_id);
        self.fill_state.remove(&order_id);
        self.retire(order_client_id);
        vec![Event::OrderCanceled(OrderCanceled {
            order_client_id: order_client_id.to_string(),
            reason: reason.into(),
            timestamp: 0,
        })]
    }

    /// Whether the venue should be asked, NOW, for the commission a held cancel is still waiting for: a cancel whose
    /// `wait_ms` has run out with a fill of its order unjoined, not asked yet. Call it just before
    /// [`EventMapper::release_held_cancels`] with the same clock and bound; on `true` the caller sends `reqExecutions`
    /// (the answer carries the commission reports, and the ordinary join emits the fill with the REAL fee), and the
    /// release then waits [`COMMISSION_REREQUEST_WAIT_MS`] more before it books the fee unknown.
    ///
    /// `false` while the connection LEARNS the venue (the request would end into the learning's `SnapshotEnd`, as for
    /// the stranded-fill sweep) or the stream is dead (nothing can answer): those cancels are released at the old
    /// bound, as before. One ask per held cancel.
    pub(crate) fn ask_for_held_commissions(&mut self, now_ms: i64, wait_ms: i64) -> bool {
        if self.ids.stream_dead()
            || matches!(self.learning, Learning::OpenOrders | Learning::Executions)
        {
            return false;
        }
        let mut ask = false;
        for (order_id, held) in self.held_cancels.iter_mut() {
            let Some(first) = held.first_seen_ms else { continue };
            if held.asked_ms.is_some() || now_ms.saturating_sub(first) < wait_ms {
                continue;
            }
            if self.pending.values().any(|pf| !pf.recovered && pf.order_id == *order_id) {
                held.asked_ms = Some(now_ms);
                ask = true;
            }
        }
        ask
    }

    /// Let the cancels that have waited long enough for a commission go out ANYWAY: the commission is not
    /// coming (IB sends it right behind its execution). Called by the exec loop on its sweep cadence with a
    /// monotonic `now_ms` (the mapper has no clock; a held cancel is stamped by the first pass that sees it,
    /// so the wait is `(wait_ms - sweep period, wait_ms]`; a cancel the venue was asked about
    /// ([`EventMapper::ask_for_held_commissions`]) goes out [`COMMISSION_REREQUEST_WAIT_MS`] after the ask instead).
    ///
    /// ⚠ The fills still unjoined are booked WITH AN UNKNOWN FEE: `FillEvent.commission` is a bare `f64`
    /// with no way to say "unknown", so it is `0.0` with no fee currency, and an `error!` names every
    /// execution so the operator can put the fee right (the same convention bybit's fast wire follows).
    /// The position and the order's fills are right; only the fee is short. A commission that arrives
    /// afterwards is reported (`on_commission_report`), not dropped in silence.
    pub(crate) fn release_held_cancels(&mut self, now_ms: i64, wait_ms: i64) -> Vec<Event> {
        let mut due: Vec<i32> = Vec::new();
        for (order_id, held) in self.held_cancels.iter_mut() {
            let first = *held.first_seen_ms.get_or_insert(now_ms);
            // Once the venue was asked for the commission, the bound is the answer's (`COMMISSION_REREQUEST_WAIT_MS`).
            let waited = match held.asked_ms {
                Some(asked) => now_ms.saturating_sub(asked) >= COMMISSION_REREQUEST_WAIT_MS,
                None => now_ms.saturating_sub(first) >= wait_ms,
            };
            if waited {
                due.push(*order_id);
            }
        }
        due.sort_unstable();
        let mut out = Vec::new();
        for order_id in due {
            let Some(order_client_id) =
                self.held_cancels.get(&order_id).map(|h| h.order_client_id.clone())
            else {
                continue;
            };
            let mut stale: Vec<String> = self
                .pending
                .iter()
                .filter(|(_, pf)| !pf.recovered && pf.order_id == order_id)
                .map(|(id, _)| id.clone())
                .collect();
            stale.sort();
            for exec_id in stale {
                let Some(pf) = self.pending.remove(&exec_id) else { continue };
                if pf.untaught {
                    // An order nobody taught (`is_untaught`): the fill is not emitted at all, so there is no fee to
                    // put right either; the execution is remembered as seen, like an emitted one.
                    Self::log_untaught_fill(
                        &pf.order_client_id,
                        &exec_id,
                        pf.side,
                        pf.shares,
                        pf.price,
                        &pf.symbol,
                    );
                    self.note_emitted(exec_id);
                    continue;
                }
                tracing::error!(
                    exec_id = %exec_id,
                    order_client_id = %pf.order_client_id,
                    qty = pf.shares,
                    price = pf.price,
                    waited_ms = wait_ms,
                    "ibkr: the order was CANCELED and the commission of this execution did not arrive in \
                     time: the fill is booked with an UNKNOWN fee (0.0) - the position is right, the fee is \
                     short until a late commissionReport arrives and corrects it (a fee adjustment)"
                );
                let fill = FillEvent {
                    trade_id: pf.trade_id.clone(),
                    order_client_id: pf.order_client_id.clone(),
                    venue: ustr_lite("ibkr"),
                    symbol: ustr_lite(&pf.symbol),
                    side: pf.side,
                    last_qty: pf.shares,
                    last_price: pf.price,
                    commission: 0.0,
                    commission_asset: ustr_lite(""),
                    liquidity_side: Default::default(),
                    timestamp: pf.ts,
                    mark_price: None,
                    position_side: Default::default(),
                };
                // The fee the engine is told is UNKNOWN (0.0, no currency): the real commission, when it
                // arrives, goes out as a fee adjustment carrying all of it.
                self.note_fee_book(
                    &exec_id,
                    FeeBook {
                        trade_id: pf.trade_id.clone(),
                        order_client_id: pf.order_client_id.clone(),
                        symbol: pf.symbol.clone(),
                        px: pf.price,
                        ts: pf.ts,
                        fill_commission: 0.0,
                        fill_asset: String::new(),
                        told_extra: 0.0,
                        told_asset: String::new(),
                        seq: 0,
                    },
                );
                out.extend(terminal_events(pf.order_client_id, fill, pf.ts, false));
                self.note_emitted(exec_id);
            }
            out.extend(self.finish_cancel(order_id, &order_client_id));
        }
        out
    }

    /// Remember what the engine was told the fee of `exec_id`'s fill is ([`FeeBook`]). Bounded like `emitted`,
    /// oldest first: an evicted execution's late report is a no-op again, which is what it was before decision 0238.
    fn note_fee_book(&mut self, exec_id: &str, mut book: FeeBook) {
        if self.fee_book.len() >= MAX_EMITTED_EXECUTION_IDS && !self.fee_book.contains_key(exec_id)
        {
            let oldest = self.fee_book.iter().min_by_key(|(_, b)| b.seq).map(|(id, _)| id.clone());
            if let Some(oldest) = oldest {
                self.fee_book.remove(&oldest);
            }
        }
        book.seq = self.fee_book_sequence;
        self.fee_book_sequence += 1;
        self.fee_book.insert(exec_id.to_string(), book);
    }

    pub(crate) fn on_error(&mut self, code: i32, order_id: i32, msg: &str) -> Vec<Event> {
        match classify_code(code) {
            // Data-vs-Notice: advisory / connectivity / suppress → no order event (the transport
            // layer handles connectivity flag toggling; here we only avoid faking a terminal).
            CodeClass::ConnectivityLost | CodeClass::ConnectivityRestored => vec![],
            // IB's words on an order of ours that end nothing: said once, so an order IB answers with nothing else shows
            // what IB said (paper 2026-11-09: a `PEG MKT`, a `REL`, a `PEG BEST` and an activated `TRAIL` each drew one
            // such notice and no status, and nothing logged it).
            CodeClass::Warning | CodeClass::Suppress | CodeClass::Other => {
                if order_id != 0 {
                    tracing::info!(
                        code,
                        order_id,
                        order_client_id = self
                            .ids
                            .order_client_id_of(order_id)
                            .or_else(|| self.ended_here(order_id))
                            .unwrap_or(""),
                        msg,
                        "ibkr: IB's notice on an order (not a refusal: it ends nothing)"
                    );
                }
                vec![]
            }
            CodeClass::OrderRejection => {
                // An order-rejection-class error naming an order whose AMEND is in flight refuses the amend; the
                // order itself keeps working at its old terms and must not be terminalized (`execution::amend`).
                if order_id != 0
                    && let Some(event) = self.amends.refuse_for_order(order_id, msg)
                {
                    return vec![event];
                }
                // Direct resolution: IB's own notice names the order (ibapi 5's `Notice::request_id`, which the
                // socket pump passes on), and so do the SYNCHRONOUS submit failures (execution/mod.rs, the cpapi place).
                if let Some(order_client_id) = self.resolve(order_id, "") {
                    return self.refused(order_id, order_client_id, code, msg);
                }
                // Id-less path (no-order-vanishes fix): a notice with no order id (ibapi 4.2.0 dropped it; 5.2.0
                // copies only order-bound notices onto the stream, so this is now the backstop) arrives here as
                // `on_error(code, 0, msg)`, and is attributed to a still-unacked order. When EXACTLY ONE order is
                // unacked the rejection is unambiguously that order's → route it so the intent never silently
                // vanishes.
                //
                // ...but NOT while an amend is in flight: TWS answers a refused amend (error 201) the same
                // id-less way, so the notice may be the amend's and the unacked order innocent, and a
                // terminal `OrderRejected` on a live order is the one wrong answer that cannot be taken
                // back. With an amend open the notice is the amend's only when nothing else could own it
                // (no unacked order, exactly one amend open: a non-terminal `OrderModifyRejected`);
                // otherwise it is left unattributed - the amend ends unconfirmed at its bound, and an
                // unacked order is the submit-ack watchdog's.
                if order_id == 0 && self.amends.any_open() {
                    if self.unacked.is_empty()
                        && let Some(order_client_id) = self.amends.sole_open()
                        && let Some(event) = self.amends.refuse_unattributed(&order_client_id, msg)
                    {
                        return vec![event];
                    }
                } else if order_id == 0 && self.unacked.len() == 1 {
                    let oid = *self.unacked.iter().next().expect("len checked == 1");
                    if let Some(order_client_id) = self.resolve(oid, "") {
                        return self.refused(oid, order_client_id, code, msg);
                    }
                }
                // An id of an order that already ended here: IB's words came behind its end.
                if let Some((_, order_client_id)) =
                    self.ended.iter().rev().find(|(id, _)| order_id != 0 && *id == order_id)
                {
                    Self::log_refusal_after_end(order_id, order_client_id, code, msg);
                    return vec![];
                }
                // Zero or many unacked ⇒ we cannot attribute an id-less rejection without risking a
                // sibling order's state; an id that resolves to nothing names an order this connection does
                // not hold (never ours, or ended too long ago). Surface loudly, with IB's words; the core
                // submit-ack watchdog is the no-vanish backstop for an unacked order.
                tracing::warn!(
                    code,
                    order_id,
                    unacked = self.unacked.len(),
                    msg,
                    "unroutable IBKR order-rejection notice; relying on submit-ack watchdog"
                );
                vec![]
            }
        }
    }

    /// IB refused `order_client_id`'s order (`msg` is its words): logged once, here. Never acknowledged, it is the order's
    /// `OrderRejected`. ACKNOWLEDGED (a resting order IB refused after taking it, an iceberg's 201 0.3 s after its accept
    /// on paper 2026-11-09) the code alone ends NOTHING: the order may still rest at IB, and a local end of a live venue
    /// order is an orphan. Its end is IB's status (`on_order_status`), as an `OrderCanceled` (the engine's FSM drops a
    /// reject after `Accepted`) carrying these words, kept until that status comes ([`EventMapper::refusals`]). IB's end
    /// already in and held for these words (it came right after the accept, [`EventMapper::fresh_ends`]): it goes out now,
    /// carrying them. Held only for a fill's commission: the words are logged, never attached to it.
    fn refused(
        &mut self,
        order_id: i32,
        order_client_id: String,
        code: i32,
        msg: &str,
    ) -> Vec<Event> {
        // IB ended the order right after taking it and its words are the reason (`fresh_ends`): the end carries them.
        if self.fresh_ends.remove(&order_id).is_some() {
            tracing::warn!(
                order_client_id = %order_client_id,
                order_id,
                code,
                msg,
                "ibkr: IB refused the order it had just ended on its own: the end carries IB's words"
            );
            return self.end_over(order_id, order_client_id, msg.to_string());
        }
        if self.held_cancels.contains_key(&order_id) {
            Self::log_refusal_after_end(order_id, &order_client_id, code, msg);
            return vec![];
        }
        let resting = self.accepted.contains(&order_client_id);
        tracing::warn!(order_client_id = %order_client_id, order_id, code, msg, resting, "ibkr: IB refused the order");
        if !resting {
            return self.reject(order_id, order_client_id, msg);
        }
        self.refusals
            .insert(order_id, OrderRefusal { words: msg.to_string(), first_seen_ms: None });
        vec![]
    }

    /// IB's refusal of an order whose end it had already sent: logged once naming the order, never attached to that end.
    fn log_refusal_after_end(order_id: i32, order_client_id: &str, code: i32, msg: &str) {
        tracing::warn!(
            order_client_id,
            order_id,
            code,
            msg,
            "ibkr: IB refused this order AFTER it said the order ended: logged only, the end stands as sent"
        );
    }

    /// Emit `OrderRejected` for `order_client_id` and drop all per-order state (id map, unacked, accepted).
    fn reject(&mut self, order_id: i32, order_client_id: String, msg: &str) -> Vec<Event> {
        self.note_ended(order_id, &order_client_id);
        self.ids.forget(order_id);
        self.unacked.remove(&order_id);
        self.accepted.remove(&order_client_id);
        self.retire(&order_client_id);
        vec![Event::OrderRejected(OrderRejected {
            cause: None,
            order_client_id,
            reason: msg.to_string().into(),
            timestamp: 0,
        })]
    }

    /// Expose the registry for reconnect resync (Task 10).
    pub(crate) fn ids_mut(&mut self) -> &mut OrderVenueIdAllocator {
        &mut self.ids
    }

    /// Read-only registry access — the readiness/stream-death queries `execution::run_execution` makes on
    /// every command, which have no business taking a `&mut`.
    pub(crate) fn ids(&self) -> &OrderVenueIdAllocator {
        &self.ids
    }

    /// Every order client id this mapper still holds live per-order state for: submitted-but-unacked orders
    /// plus accepted ones. Used by `execution::run_execution`'s stream-death arm to NAME the orders whose
    /// venue state became unknowable, since nothing can be synthesized for them truthfully.
    pub(crate) fn live_order_client_ids(&self) -> Vec<&str> {
        let mut out: Vec<&str> = self
            .unacked
            .iter()
            .filter_map(|oid| self.ids.order_client_id_of(*oid))
            .chain(self.accepted.iter().map(String::as_str))
            .collect();
        out.sort_unstable();
        out.dedup();
        out
    }

    /// Re-seed the order client id⇄orderId map from one row of an open-order snapshot delivered after
    /// reconnect. IBKR does NOT replay open orders/executions on its own — `orderRef` IS the order client id
    /// (that's how it round-trips through the venue), so this `bind`s `order_id → order_ref` into
    /// the `OrderVenueIdAllocator` exactly like a fresh submit would.
    ///
    /// Two cases, distinguished by the `unacked` set ("core submitted this but has NOT yet seen its
    /// `OrderAccepted`"):
    ///
    /// - **`order_id` was in `unacked`** — the order was mid-flight when the socket blipped: the
    ///   core submitted it, IB accepted it server-side, but the confirming
    ///   `OrderStatus(Submitted/PreSubmitted)` was lost before it arrived. The core is still waiting
    ///   for the accept, so we MUST emit `OrderAccepted` (via the idempotent `ensure_accepted` idiom,
    ///   exactly like the exec-before-open synthesis in `on_execution_details`) to unstick it — otherwise
    ///   the `OrderRecord` FSM never leaves `Submitted` and the order's eventual terminal is dropped
    ///   as an invalid transition → the order silently vanishes.
    /// - **`order_id` was NOT in `unacked`** — an EXTERNAL order (opened in a previous
    ///   session/process this instance never submitted), or one already accepted before the blip
    ///   (already in `accepted`). Mark it accepted SILENTLY and emit nothing: re-emitting would be a
    ///   spurious duplicate, or an accept for an order client id the core never submitted.
    pub(crate) fn rebind_open_order(&mut self, order_id: i32, order_ref: &str) -> Vec<Event> {
        self.ids.bind(order_id, order_ref);
        if self.unacked.remove(&order_id) {
            // Mid-flight: its accept was lost in the blip — synthesize it (idempotent).
            self.ensure_accepted(order_ref, order_id).into_iter().collect()
        } else {
            // External / already-accepted: mark accepted so a later status emits no duplicate.
            self.accepted.insert(order_ref.to_string());
            vec![]
        }
    }
}

/// What the exec loop does next after a snapshot request ended ([`EventMapper::snapshot_ended`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum LearnStep {
    /// Not the request the mapper is waiting for (the stranded-fill sweep asks for executions too).
    Ignore,
    /// The open orders are known: ask for the day's executions now.
    RequestExecutions,
    /// Both ended complete: the bridge knows the venue.
    Done,
    /// A request failed or timed out: the bridge does not know the venue.
    Failed,
    /// The open orders ENDED, but what they said cannot be trusted: rows exist, none is this client's, and some
    /// carry an `orderRef` (an order of ours that the decoder did not attribute to us looks exactly like that).
    /// The connection is NOT learned; the exec loop says so loudly. See [`LearnStats::suspicious`].
    Suspicious,
}

/// What the open-order snapshot of a fresh connection contained, for the operator's log and for the one
/// judgement the bridge makes about it ([`LearnStats::suspicious`]).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct LearnStats {
    /// Rows the venue returned.
    pub rows: usize,
    /// Rows that are this client's.
    pub ours: usize,
    /// Rows that are NOT this client's but carry an `orderRef` (someone else's tagged order, a hand-typed note
    /// in TWS, or ours that the decoder did not attribute to us). Informational: a foreign row whose client id
    /// or account DID decode is just somebody else's order.
    pub foreign_with_ref: usize,
    /// Rows that are not this client's, carry a bridge-shaped `orderRef` AND whose client id and account did not
    /// decode at all (client id 0, empty account): the shape a blind decoder gives every row, ours included.
    /// This is what [`LearnStats::suspicious`] counts.
    pub undecoded_with_ref: usize,
    /// The order client ids this connection rebound (a working or resting order of ours).
    pub bound: Vec<String>,
}

impl LearnStats {
    /// Rows exist, none is ours, and some are UNDECODED and tagged (client id 0, no account, a bridge-shaped
    /// `orderRef`). A decoder that left the client id and account empty gives every row exactly that shape, ours
    /// included, and a "learned" connection over such rows would be blind: it must not claim to have learned.
    /// Rows whose client id or account DID decode are somebody else's orders and not suspicious, whatever their
    /// `orderRef` says: a hand-typed note in TWS, another API client's tag (review of W14e, finding 2: one such
    /// row used to block all trading).
    pub(crate) fn suspicious(&self) -> bool {
        self.rows > 0 && self.ours == 0 && self.undecoded_with_ref > 0
    }
}

impl EventMapper {
    /// A fresh connection starts LEARNING the venue: the exec loop is about to ask for the open orders
    /// (and then the executions). Rows of the open-order snapshot arriving from here are rebound as the
    /// orders of a PREVIOUS connection of this client ([`EventMapper::on_venue_order`]).
    pub(crate) fn begin_learning(&mut self) {
        self.stats = LearnStats::default();
        self.learning = Learning::OpenOrders;
        self.orders_unknown = true;
        self.executions_owed = false;
    }

    pub(crate) fn learning(&self) -> Learning {
        self.learning
    }

    /// Whether the executions replay of the learning is to be held back (the host has not handed the client
    /// over yet). The exec loop sets it at the head of every turn.
    pub(crate) fn hold_executions(&mut self, hold: bool) {
        self.hold_executions = hold;
    }

    /// The open orders ended and the executions replay is due: `true` = ask for it now, `false` = it is held
    /// back (the host has not handed the client over) and [`EventMapper::take_owed_executions`] will give it
    /// out when the hold is lifted.
    pub(crate) fn executions_due(&mut self) -> bool {
        if self.hold_executions {
            self.executions_owed = true;
            false
        } else {
            true
        }
    }

    /// Whether a held-back executions replay is still owed (and the learning is still waiting for it).
    pub(crate) fn executions_owed(&self) -> bool {
        self.executions_owed && self.learning == Learning::Executions
    }

    /// Take the owed executions replay: `true` once, when it is owed and the hold is lifted.
    pub(crate) fn take_owed_executions(&mut self) -> bool {
        if self.executions_owed() && !self.hold_executions {
            self.executions_owed = false;
            true
        } else {
            false
        }
    }

    /// What the open-order snapshot contained so far.
    pub(crate) fn learn_stats(&self) -> &LearnStats {
        &self.stats
    }

    /// Count one row of the open-order snapshot while the connection is learning: `ours` = it passed the
    /// ownership check, `has_ref` = it carries an `orderRef`, `undecoded_tagged` = its client id and account
    /// did not decode (0 and empty) and its `orderRef` has the shape of an order client id.
    pub(crate) fn note_row(&mut self, ours: bool, has_ref: bool, undecoded_tagged: bool) {
        if self.learning != Learning::OpenOrders {
            return;
        }
        self.stats.rows += 1;
        if ours {
            self.stats.ours += 1;
        } else {
            if has_ref {
                self.stats.foreign_with_ref += 1;
            }
            if undecoded_tagged {
                self.stats.undecoded_with_ref += 1;
            }
        }
    }

    /// At the end of the learning: an order the venue says was PARTLY filled before today (a GTC) has fills
    /// the day's executions replay cannot contain, so its `total` is reduced by the part of the venue's
    /// filled quantity that the replay did NOT account for. The fill that completes the order is then final
    /// instead of never. Where the replay already holds all of it, nothing changes (adding `venue_filled`
    /// to the sum would count the replayed fills twice and finalise early).
    fn settle_learned_totals(&mut self) {
        // An order with a fill still waiting for its commission is settled LATER, when that fill joins
        // (`join_fill`): its `filled` lacks the fill now, so the part the venue says it filled would be
        // taken off the total and the fill counted again at the join.
        let ready: Vec<i32> = self
            .fill_state
            .iter()
            .filter(|(id, st)| st.venue_filled > 0.0 && !self.has_unjoined(**id))
            .map(|(id, _)| *id)
            .collect();
        for order_id in ready {
            self.settle_order(order_id);
        }
    }

    /// [`EventMapper::settle_learned_totals`] for one order. The total never goes below what has been
    /// filled so far.
    fn settle_order(&mut self, order_id: i32) {
        let Some(st) = self.fill_state.get_mut(&order_id) else { return };
        if st.venue_filled > 0.0 {
            let missing = (st.venue_filled - st.filled).max(0.0);
            if missing > 0.0 {
                st.total = (st.total - missing).max(st.filled);
            }
            st.venue_filled = 0.0;
        }
    }

    /// Give learning up as FAILED (the exec loop's backstop for a request nothing ever ended).
    pub(crate) fn abandon_learning(&mut self) {
        if matches!(self.learning, Learning::OpenOrders | Learning::Executions) {
            self.learning = Learning::Failed;
        }
    }

    /// A snapshot request ended, `complete` or not (failed, or timed out). Only the request the mapper is
    /// waiting for moves it; anything else (the sweep's own executions request) is ignored.
    pub(crate) fn snapshot_ended(
        &mut self,
        snapshot: crate::transport::SnapshotRequest,
        complete: bool,
    ) -> LearnStep {
        use crate::transport::SnapshotRequest;
        match (self.learning, snapshot) {
            (Learning::OpenOrders, SnapshotRequest::OpenOrders) if complete => {
                if self.stats.suspicious() {
                    self.learning = Learning::Failed;
                    return LearnStep::Suspicious;
                }
                self.learning = Learning::Executions;
                self.orders_unknown = false;
                LearnStep::RequestExecutions
            }
            (Learning::Executions, SnapshotRequest::Executions) if complete => {
                self.settle_learned_totals();
                self.learning = Learning::Done;
                LearnStep::Done
            }
            (Learning::OpenOrders, SnapshotRequest::OpenOrders)
            | (Learning::Executions, SnapshotRequest::Executions) => {
                self.learning = Learning::Failed;
                LearnStep::Failed
            }
            _ => LearnStep::Ignore,
        }
    }

    /// Remember that `exec_id` matched no local order. Bounded like `emitted`, oldest first.
    fn note_unroutable(&mut self, exec_id: String) {
        if self.unroutable.len() >= MAX_EMITTED_EXECUTION_IDS
            && !self.unroutable.contains_key(&exec_id)
        {
            let oldest =
                self.unroutable.iter().min_by_key(|(_, seq)| **seq).map(|(id, _)| id.clone());
            if let Some(oldest) = oldest {
                self.unroutable.remove(&oldest);
            }
        }
        let seq = self.unroutable_sequence;
        self.unroutable_sequence += 1;
        self.unroutable.insert(exec_id, seq);
    }

    /// One row of the VENUE's open-order snapshot, already established to be an order of THIS client
    /// (`OwnOrders`). Three cases:
    ///
    /// * the row is unusable (no `orderRef`, no positive id or quantity, or the order already
    ///   finished): nothing is bound, nothing is emitted;
    /// * the connection is LEARNING (a fresh connection's snapshot): the row is an order a previous
    ///   connection of this client placed, and it is REBOUND here: the order client id⇄id map learns it (so a cancel
    ///   reaches IB under the right id and its status and executions resolve), its fill tracking is
    ///   seeded (`total` from the row, `filled` 0: the executions replay that follows adds the fills, so
    ///   the first replayed partial is not mistaken for the last), and an `OrderAccepted` is emitted for it
    ///   when it is live at the venue. The accept is idempotent in the engine's FSM (a duplicate on an
    ///   order that is already accepted is dropped) and it is what lifts an order whose accept the outage
    ///   swallowed out of `Submitted` (review of W14b, finding 5);
    /// * otherwise (an `openOrder` update during normal operation): the long-standing
    ///   [`EventMapper::rebind_open_order`].
    pub(crate) fn on_venue_order(&mut self, row: &IbkrOpenOrder) -> Vec<Event> {
        if row.order_id <= 0 || row.order_ref.is_empty() {
            tracing::debug!(
                order_id = row.order_id,
                "ibkr: open-order row without an id or an orderRef — not ours to bind"
            );
            return vec![];
        }
        let kind = OrderStatusKind::from_ibkr(&row.status);
        if kind.is_terminal() {
            tracing::debug!(
                order_id = row.order_id,
                order_client_id = %row.order_ref,
                status = %row.status,
                "ibkr: open-order row of a finished order — not rebound"
            );
            return vec![];
        }
        if self.learning != Learning::OpenOrders {
            // Outside the learning window an `openOrder` update is only meaningful for an order this
            // connection ALREADY holds (it placed it, or learned it): the long-standing silent rebind.
            // An unbound row of ours (the learning failed or was abandoned, and TWS still pushes
            // updates) is NOT bound here: it would carry no fill tracking, its first execution would be
            // classified final and every later one dropped (review 18, R2). Nothing is bound without the
            // total that fill tracking needs and the conflict check; the order stays unknown, and a cancel
            // of it is refused loudly.
            if self.ids.order_id_of(&row.order_ref) == Some(row.order_id) {
                let mut out = self.rebind_open_order(row.order_id, &row.order_ref);
                // ...and an `openOrder` row of an order whose amend is in flight is the venue's answer to it.
                let confirmed = self.amends.on_row(&row.order_ref, row);
                // A CONFIRMED amend moves the quantity the order completes at; any other update leaves the fill
                // tracking alone (it is not the venue's word on OUR request).
                if let Some(Event::OrderModified(m)) = &confirmed
                    && let Some(qty) = m.new_qty
                {
                    self.follow_venue_qty(row.order_id, qty);
                }
                out.extend(confirmed);
                return out;
            }
            // IB's late row of an order that ENDED on this connection (paper 2026-11-09: the iceberg IB refused re-sent its
            // row a millisecond after its end went out): nothing to bind, nothing to warn about.
            if self.ended_here(row.order_id) == Some(row.order_ref.as_str()) {
                tracing::debug!(
                    order_id = row.order_id,
                    order_client_id = %row.order_ref,
                    "ibkr: open-order row of an order that ended on this connection - not rebound"
                );
                return vec![];
            }
            tracing::warn!(
                order_id = row.order_id,
                order_client_id = %row.order_ref,
                "ibkr: open-order row of our own for an order this connection does not hold, outside the
                 learning window — not bound (cancel it in TWS if it is meant to be managed)"
            );
            return vec![];
        }
        if !(row.total_qty.is_finite() && row.total_qty > 0.0) {
            tracing::warn!(
                order_id = row.order_id,
                order_client_id = %row.order_ref,
                total_qty = row.total_qty,
                "ibkr: open-order row without a usable quantity — not rebound (its fills could not be classified)"
            );
            return vec![];
        }
        match self.ids.bind_rebound(row.order_id, &row.order_ref) {
            Rebound::Conflict => {
                tracing::warn!(
                    order_id = row.order_id,
                    order_client_id = %row.order_ref,
                    "ibkr: open-order row contradicts a binding this connection holds — not rebound"
                );
                return vec![];
            }
            // A binding this connection did not hold: an order a PREVIOUS connection placed. Whether the host
            // teaches its symbol decides whether its fills are emitted (`is_untaught`).
            Rebound::Bound => {
                self.rebound.insert(row.order_ref.clone());
            }
            // The id was already this order client id's here: an order this connection placed itself.
            Rebound::Known => {}
        }
        self.fill_state.entry(row.order_id).or_insert(FillState {
            total: row.total_qty,
            qty: row.total_qty,
            filled: 0.0,
            venue_filled: row.filled_qty.max(0.0),
        });
        self.stats.bound.push(row.order_ref.clone());
        self.unacked.remove(&row.order_id);
        if kind.is_active() {
            self.ensure_accepted(&row.order_ref, row.order_id).into_iter().collect()
        } else {
            // Rests at the venue in a state that is not "working" (a pending cancel, ...): bound so its
            // status resolves, announced as accepted only when it is working.
            self.accepted.insert(row.order_ref.clone());
            vec![]
        }
    }
}

/// `FillEvent.venue`/`symbol`/`commission_asset` are `Ustr` (interned). Bounded cardinality (one
/// venue, per-session symbols) so interning is sound — see vike_model::events interning contract.
fn ustr_lite(s: &str) -> ustr::Ustr {
    ustr::ustr(s)
}

#[path = "event_mapper_tests.rs"]
#[cfg(test)]
mod event_mapper_tests;

#[path = "event_mapper_property_tests.rs"]
#[cfg(test)]
mod event_mapper_property_tests;
