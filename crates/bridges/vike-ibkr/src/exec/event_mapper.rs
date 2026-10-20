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
//!   4. Typed terminal status drives coid⇄orderId map cleanup.
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
//!      rows of the open-order snapshot that are THIS client's rebind the coid⇄orderId map, seed the
//!      order's `fill_state` (`filled` 0, `total` from the row: the executions replay that follows adds
//!      the fills, so the first replayed partial is a partial) and announce a working order as accepted;
//!      an execution that resolves to no order is remembered as unroutable so its commission is dropped,
//!      not parked. Foreign rows never reach here (`OwnOrders`, checked by the exec loop).
//!   7. **A fill carries the symbol its ORDER was submitted under** ([`EventMapper::register_symbol`], [`EventMapper::fill_symbol`]),
//!      never the instrument IB echoes on the execution: the engine drops a bare `Fill` whose symbol it neither mounts nor
//!      registered the order under, in silence.
//!   8. **A venue cancel waits for the commissions of the order's unjoined fills** ([`EventMapper::release_held_cancels`]), so a fill
//!      goes out with its real fee instead of an invented `0.0`; past a bound the fee is booked unknown, loudly.
//!
//! Pure + fixture-tested; owns an `IdRegistry`. Consumed by the exec loop (Task 8,
//! `exec::run_exec` folds transport inbound through it).
//!
//! **Clockless by construction.** Nothing here reads a clock. `sweep_pending` takes `now_ms` as a
//! PARAMETER and uses only DIFFERENCES of it, so every test passes explicit millis and stays
//! deterministic — the same event-driven-not-clock-driven discipline the parked-commission expiry
//! in [`EventMapper::on_exec_details`] already follows.

use std::collections::{HashMap, VecDeque};

use crate::error::{CodeClass, classify_code};
use crate::exec::amend::{PendingAmend, PendingAmends};
use crate::exec::id_registry::{IdRegistry, Rebound};
use crate::order::OrderStatusKind;
use vike_bridge_core::terminal_events;
use vike_model::events::{Event, FillEvent, OrderAccepted, OrderCanceled, OrderRejected, TradeId};

#[derive(Default)]
pub struct IbOrderStatus {
    pub order_id: i32,
    pub order_ref: String,
    pub status: String,
    pub filled: f64,
    pub avg_fill_price: f64,
    /// The API client that placed the order, when the backend says (0 = it does not).
    pub client_id: i32,
}

/// One row of the VENUE's open-order snapshot (IB `openOrder`), as the socket backend decodes it. Unlike the
/// bare `IbInbound::OpenOrder` pair (the cpapi pump's, which has no more to say), it carries what is needed to
/// tell an order of THIS client from any other (`client_id`, `account`) and to seed the order's fill tracking
/// (`total_qty`).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct IbOpenOrder {
    pub order_id: i32,
    /// IB's `orderRef`: our coid when this client placed the order, EMPTY for an order made in TWS.
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
    /// Read only to confirm an amend ([`crate::exec::amend::PendingAmends`]).
    pub limit_price: Option<f64>,
    /// The order's auxiliary (stop trigger) price as the venue holds it (`None` = not said or not a stop).
    pub aux_price: Option<f64>,
}

#[derive(Default)]
pub struct IbExecDetails {
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
    /// that finished while the connection was down, and is recovered as a partial. See
    /// [`EventMapper::on_exec_details`].
    pub recoverable: bool,
}

pub struct IbCommissionReport {
    pub exec_id: String,
    pub commission: f64,
    pub currency: String,
}

/// A fill awaiting its commission (join by exec_id).
struct PendingFill {
    order_id: i32,
    /// The IB `execution_id` as the validated dedup key, resolved ONCE at ingress
    /// ([`EventMapper::on_exec_details`]) and carried so neither of the two `FillEvent` sites — the
    /// commission join and the cancel flush — has to re-validate it or can disagree about it.
    trade_id: TradeId,
    coid: String,
    symbol: String,
    side: i32,
    shares: f64,
    price: f64,
    ts: i64,
    /// A fill of an order that finished while the connection was down, recovered from the replay: always
    /// a PARTIAL (nothing says how the order ended) and it changes no order bookkeeping.
    recovered: bool,
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
/// [`EventMapper::park_commission`]; the definitive expiry is in [`EventMapper::on_exec_details`]).
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
/// re-emits — and `vike_exec::ExecutionEngine` carries an ALWAYS-ON `seen_trade_ids` /
/// `seen_fsm_trade_ids` dedup keyed on `FillEvent.trade_id` (which IS the IB `execution_id`)
/// precisely for reconnect replays, so the duplicate is dropped one layer up. The one genuine
/// residual is local: `fill_state.filled` would double-count that replay, which can only matter if
/// the SAME order is still live 4096 executions later.
const MAX_EMITTED_EXEC_IDS: usize = 4096;

/// One execution whose fill could not be emitted: its commissionReport never arrived and the venue
/// re-request budget is spent. Reported by [`EventMapper::sweep_pending`] so the exec loop can name
/// exactly which fill the platform is short — the entry itself is still held and still joinable.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct StrandedFill {
    pub exec_id: String,
    pub client_order_id: String,
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
    /// What the venue said was ALREADY filled when the order was learned (`IbOpenOrder::filled_qty`); `0`
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
    ids: IdRegistry,
    accepted: std::collections::HashSet<String>, // coids that have emitted OrderAccepted
    /// exec_id → fill awaiting its commission.
    ///
    /// **Deliberately UNCAPPED, and never evicted by the sweep** (module doc trap 5). The two maps
    /// are not symmetric: evicting a parked commission loses a fee figure, whereas evicting a
    /// `pending` entry would lose THE FILL ITSELF — the exact failure #949 removed. An entry that
    /// outlives its commissionReport is therefore recovered (`sweep_pending` → the exec loop asks
    /// IBKR to re-deliver the day's executions) or escalated, never dropped.
    ///
    /// Removal paths: the join (`on_commission_report` / `on_exec_details`) and
    /// `on_order_status`'s Cancelled/ApiCancelled flush. `sweep_pending` only ANNOTATES.
    pending: HashMap<String, PendingFill>,
    /// exec_id → commission awaiting its execution: the reverse index `ibapi` 3.2.1's docs
    /// prescribe (module doc trap 2). Empty in steady state — a commission that arrives after its
    /// execution joins and emits within the same call, so an entry surviving means that
    /// execution genuinely has not been delivered yet. Bounded by [`MAX_PARKED_COMMISSIONS`].
    commissions: HashMap<String, PendingCommission>,
    /// Monotonic counter stamped into each [`PendingCommission::seq`]. Only ever increments.
    commission_seq: u64,
    /// Order ids submitted but not yet acked (active status) or terminal. The routing population
    /// for the id-less async-rejection path — see `on_error`.
    unacked: std::collections::HashSet<i32>,
    /// Per-order cumulative-vs-total fill tracking, populated by `on_submit` and consulted (and
    /// forgotten on the final fill) by `on_commission_report`. Absent entry ⇒ unknown total
    /// (external/rebound order) ⇒ single-fill-terminal fallback.
    fill_state: HashMap<i32, FillState>,
    /// exec_id → park seq, for every exec_id whose fill has ALREADY been emitted — by the join
    /// (`join_fill`) or by the cancel-with-commission-in-flight flush. Consulted, WITHOUT removing,
    /// at the top of BOTH `on_exec_details` and `on_commission_report`, so a re-delivered half is a
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
    /// Bounded by [`MAX_EMITTED_EXEC_IDS`], oldest-first — see there for why evicting from THIS
    /// map is safe while evicting from `pending` is not.
    emitted: HashMap<String, u64>,
    /// Monotonic counter stamped into each `emitted` value; only ever increments. Separate from
    /// `commission_seq` so the two caps evict independently.
    emitted_seq: u64,
    /// Learning progress, see [`Learning`].
    learning: Learning,
    /// exec_id → park seq, for every execution that resolved to NO local order (the day's executions of
    /// orders that finished before this connection, replayed by `reqExecutions`). Its commission reaches
    /// `on_commission_report` afterwards and would PARK for ever (nothing can join it); this set drops it
    /// instead. Bounded like `emitted`.
    unroutable: HashMap<String, u64>,
    unroutable_seq: u64,
    /// What the open-order snapshot being learned contained.
    stats: LearnStats,
    /// coid → the canonical symbol the order was SUBMITTED under (`register_symbol`: the submit, or the host
    /// teaching a restored order). The symbol a fill carries comes from here, never from the instrument IB
    /// echoes on the execution: see [`EventMapper::fill_symbol`]. Removed when the order retires.
    symbols: HashMap<String, String>,
    /// The coids that reached a terminal on THIS connection, newest last (bounded by [`MAX_RECENT_FINISHED`]):
    /// the cancel of one of them is not the alarm a cancel of an unknown order is.
    finished: VecDeque<String>,
    /// A venue cancel that arrived while fills of the order still wait for their commission: the cancel (and
    /// the unjoined fills) are held back a bounded time so the fills carry their REAL commission. See
    /// [`EventMapper::release_held_cancels`].
    held_cancels: HashMap<i32, HeldCancel>,
    /// exec_id → park seq, for every fill that was booked with its fee UNKNOWN (a cancel held the unjoined
    /// fill for [`CANCEL_COMMISSION_WAIT_MS`] and the commission still did not come). A real commission that
    /// arrives later is reported (the operator reconciles the fee), not silently dropped. Bounded like `emitted`.
    fee_unknown: HashMap<String, u64>,
    fee_unknown_seq: u64,
    /// The open-order snapshot of this connection is NOT known: learning is running in its first phase, or it
    /// failed there. A submit is refused while this holds (an order id handed out now could be a resting
    /// order's). False when nothing was ever asked (cpapi, scripted tests) and once the open orders ended.
    orders_unknown: bool,
    /// The host has not yet handed the client over (`exec::Taught`): the executions replay of the learning is
    /// not asked for until it has, so the fills of the restored orders it replays carry the symbols the host
    /// teaches. Set by the exec loop every turn; false in every mapper that is not driven by it.
    hold_executions: bool,
    /// The open orders ended while [`EventMapper::hold_executions`] held and the replay is still to be asked for.
    executions_owed: bool,
    /// Amends written to the venue and not yet answered (`exec::amend`).
    amends: PendingAmends,
}

/// A cancel waiting for the commissions of the order's unjoined fills. See [`EventMapper::release_held_cancels`].
struct HeldCancel {
    coid: String,
    /// Stamped by the first release pass that saw it (the mapper has no clock), like `PendingFill::first_seen_ms`.
    first_seen_ms: Option<i64>,
}

/// The most coids [`EventMapper::finished`] remembers.
const MAX_RECENT_FINISHED: usize = 256;

/// The most submitted-symbol entries [`EventMapper::symbols`] holds: past it a new order is not registered and
/// its fills carry the echoed instrument (loudly). Entries leave with their order, so this is a leak guard.
const MAX_REGISTERED_SYMBOLS: usize = 4096;

/// How long a venue cancel is held back for the commissions of the order's unjoined fills before they are booked
/// with an UNKNOWN fee (booked as 0.0, named in an `error!`). IB sends a commission report right behind its
/// execution, so this is the "the report is not coming" bound, not a normal-path delay.
pub(crate) const CANCEL_COMMISSION_WAIT_MS: i64 = 5_000;

impl EventMapper {
    pub(crate) fn new(ids: IdRegistry) -> Self {
        EventMapper {
            ids,
            accepted: Default::default(),
            pending: Default::default(),
            commissions: Default::default(),
            commission_seq: 0,
            unacked: Default::default(),
            fill_state: Default::default(),
            emitted: Default::default(),
            emitted_seq: 0,
            learning: Learning::Idle,
            unroutable: Default::default(),
            unroutable_seq: 0,
            stats: LearnStats::default(),
            symbols: Default::default(),
            finished: Default::default(),
            held_cancels: Default::default(),
            fee_unknown: Default::default(),
            fee_unknown_seq: 0,
            orders_unknown: false,
            hold_executions: false,
            executions_owed: false,
            amends: PendingAmends::default(),
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

    /// A backend vouched for an amend of `coid` without an echo (`AmendOutcome::Acknowledged`): the quantity
    /// and price it asked for are what the venue holds now.
    pub(crate) fn amend_acknowledged(
        &mut self,
        coid: &str,
        order_id: i32,
        qty: f64,
        price: Option<f64>,
    ) {
        self.amends.record_confirmed(coid, qty, price);
        self.follow_venue_qty(order_id, qty);
    }

    fn resolve(&self, order_id: i32, order_ref: &str) -> Option<String> {
        self.ids.resolve(order_id, order_ref)
    }

    /// Remember the canonical symbol `coid` was submitted under (the exec loop at the submit, the host when it
    /// teaches a restored order). The fills of the order carry THIS symbol: the engine drops a bare `Fill`
    /// whose symbol it neither mounts nor registered the order under, and IB's own spelling of the instrument
    /// (the exec contract's symbol/exchange/currency) is not the canonical for a forex pair or a stock executed
    /// on a named exchange. See [`EventMapper::fill_symbol`].
    pub(crate) fn register_symbol(&mut self, coid: &str, symbol: &str) {
        if symbol.is_empty() {
            return;
        }
        if self.symbols.len() >= MAX_REGISTERED_SYMBOLS && !self.symbols.contains_key(coid) {
            tracing::warn!(
                coid,
                cap = MAX_REGISTERED_SYMBOLS,
                "ibkr: submitted-symbol table full - this order's fills will carry the instrument IB \
                 echoes, which an engine may not mount"
            );
            return;
        }
        self.symbols.insert(coid.to_string(), symbol.to_string());
    }

    /// The symbol a fill of `coid` carries: the one the order was submitted/registered under, else (an order
    /// nobody told this connection the symbol of: recovered from the replay, rebound from the venue's open
    /// orders and not taught by the host) the instrument IB echoed, with a `warn!`, because
    /// `vike_exec::ExecutionEngine` drops a fill whose symbol it does not mount in silence (no log, no counter).
    fn fill_symbol(&self, coid: &str, echoed: &str) -> String {
        match self.symbols.get(coid) {
            Some(registered) => {
                if registered != echoed {
                    tracing::warn!(
                        coid,
                        registered = %registered,
                        echoed,
                        "ibkr: the execution names a different instrument than the order was submitted \
                         under; the fill carries the SUBMITTED symbol (the engine drops a fill whose \
                         symbol it neither mounts nor registered the order under)"
                    );
                }
                registered.clone()
            }
            None => {
                tracing::warn!(
                    coid,
                    echoed,
                    "ibkr: no submitted symbol is known for this order (recovered or rebound after a \
                     rebuild): the fill carries the instrument IB echoed, and an engine that does not \
                     mount that exact symbol DROPS it silently"
                );
                echoed.to_string()
            }
        }
    }

    /// Remember that `coid` reached a terminal on this connection and drop what is kept per order.
    fn retire(&mut self, coid: &str) {
        self.symbols.remove(coid);
        self.amends.forget(coid);
        if self.finished.len() >= MAX_RECENT_FINISHED {
            self.finished.pop_front();
        }
        self.finished.push_back(coid.to_string());
    }

    /// Whether `coid` reached a terminal on THIS connection moments ago (the last
    /// [`MAX_RECENT_FINISHED`] terminals): the cancel of it is a race lost, not an unknown order.
    pub(crate) fn recently_finished(&self, coid: &str) -> bool {
        self.finished.iter().any(|c| c == coid)
    }

    /// The open-order snapshot of this connection is not known (see the field): a new order must be refused.
    pub(crate) fn orders_unknown(&self) -> bool {
        self.orders_unknown
    }

    /// Keep an amend that is about to be written; an older unanswered amend of the same order is closed with
    /// the advisory this returns (`exec::amend`).
    pub(crate) fn note_amend(&mut self, coid: &str, pending: PendingAmend) -> Option<Event> {
        self.amends.note(coid, pending)
    }

    /// Take the amend of `coid` back out: the write failed, or the backend vouched for it synchronously.
    pub(crate) fn take_amend(&mut self, coid: &str) -> Option<PendingAmend> {
        self.amends.take(coid)
    }

    /// Close the amends that waited `wait_ms` for an echo that did not come (the loop's upkeep).
    pub(crate) fn sweep_amends(&mut self, now_ms: i64, wait_ms: i64) -> Vec<Event> {
        self.amends.sweep(now_ms, wait_ms)
    }

    /// Emit OrderAccepted once per coid (idempotent) — used both by a real Submitted/PreSubmitted
    /// status AND by the exec-before-open synthesis path.
    fn ensure_accepted(&mut self, coid: &str, order_id: i32) -> Option<Event> {
        // Active/filling ⇒ acked: it leaves the unacked set so a later id-less rejection can no
        // longer be misattributed to it.
        self.unacked.remove(&order_id);
        if self.accepted.insert(coid.to_string()) {
            Some(Event::OrderAccepted(OrderAccepted {
                client_order_id: coid.to_string(),
                venue_order_id: Some(order_id.to_string().into()),
                ts: 0,
            }))
        } else {
            None
        }
    }

    pub(crate) fn on_order_status(&mut self, s: IbOrderStatus) -> Vec<Event> {
        let Some(coid) = self.resolve(s.order_id, &s.order_ref) else {
            return vec![]; // already terminal/forgotten, or unknown external order
        };
        let kind = OrderStatusKind::from_ib(&s.status);
        let mut out = vec![];
        if kind.is_active()
            && let Some(ev) = self.ensure_accepted(&coid, s.order_id)
        {
            out.push(ev);
        }
        if matches!(kind, OrderStatusKind::Cancelled | OrderStatusKind::ApiCancelled) {
            // Fills still awaiting their commission: the cancel is HELD back for them (a bounded time,
            // `release_held_cancels`) rather than flushing the fills with an invented fee. The real
            // commission follows the execution within milliseconds, and the fill carries it when it does;
            // the terminal then follows the fills, which is the order the engine's FSM needs
            // ([...PartiallyFilled, Canceled]). `commissions` is deliberately NOT swept here: a parked
            // commission is keyed only by exec_id and its execution never arrived, so there is nothing to
            // attribute it to this order_id with. It expires on its own terms - when its execDetails finally
            // arrives and resolves to no local order (this cancel forgot the id), `on_exec_details`
            // releases it with a `warn!`.
            if self.has_unjoined(s.order_id) {
                self.held_cancels
                    .entry(s.order_id)
                    .or_insert_with(|| HeldCancel { coid: coid.clone(), first_seen_ms: None });
                tracing::debug!(
                    coid = %coid,
                    order_id = s.order_id,
                    "ibkr: cancel held back until the commission of the order's fill(s) arrives"
                );
                return out;
            }
            out.extend(self.finish_cancel(s.order_id, &coid));
        }
        // Filled status is folded from execDetails+commission (below), not here (avg-price-only
        // status can't carry the exec_id vike's FillEvent needs).
        out
    }

    pub(crate) fn on_exec_details(&mut self, e: IbExecDetails) -> Vec<Event> {
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
        let (coid, recovered) = match self.resolve(e.order_id, &e.order_ref) {
            Some(coid) => (coid, false),
            // An execution of THIS client's order (the exec loop checked the client id) that this
            // connection knows nothing about, replayed while it learns the venue: the order FINISHED while
            // the connection was down, and this is a fill it missed. It is recovered by its orderRef (our
            // coid) as a PARTIAL (nothing here says how the order ended), changing no bookkeeping; the
            // engine's trade-id dedup drops what it already folded, and applies what it had not.
            None if e.recoverable && !e.order_ref.is_empty() => (e.order_ref.clone(), true),
            None => {
                // Unknown / already-forgotten order (an external fill, or one arriving after this
                // order's terminal): there is no coid to attribute it to, so nothing can be emitted.
                // This drop is PRE-EXISTING; it is only logged now, because it is also the
                // DEFINITIVE EXPIRY for a parked commission — the execution has now ARRIVED and is
                // unroutable, so its parked half can never join and is released here rather than held
                // for the process lifetime.
                //
                // Event-driven, not clock-driven, deliberately: this mapper is a pure, fixture-tested
                // fold with no clock, and `IbCommissionReport` carries no timestamp to age by. A
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
        // The order is tracked under the id THIS connection bound for the coid, not under the number the
        // execution names: an execution resolved by its `orderRef` can carry another number, and every
        // per-order structure (unacked, fill tracking, the cancel's flush) is keyed on the bound one.
        let order_id =
            if recovered { e.order_id } else { self.ids.order_id_of(&coid).unwrap_or(e.order_id) };
        if !recovered && let Some(ev) = self.ensure_accepted(&coid, order_id) {
            out.push(ev);
        }
        let symbol = self.fill_symbol(&coid, &e.symbol);
        let p = PendingFill {
            order_id,
            trade_id,
            coid,
            symbol,
            side: if e.side_buy { 1 } else { -1 },
            shares: e.shares,
            price: e.price,
            ts: e.ts,
            recovered,
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

    pub(crate) fn on_commission_report(&mut self, c: IbCommissionReport) -> Vec<Event> {
        if self.emitted.contains_key(&c.exec_id) {
            if self.fee_unknown.remove(&c.exec_id).is_some() {
                tracing::warn!(
                    exec_id = %c.exec_id,
                    commission = c.commission,
                    currency = %c.currency,
                    "ibkr: the REAL commission of a fill that was booked with an unknown fee (0.0) has \
                     arrived late - the account's fee for this execution is short by this amount"
                );
            }
            // A commission for an exec whose fill already emitted: a late real report for a
            // cancel-flushed partial, an IB re-send, or a `reqExecutions` replay. No-op — and
            // crucially NOT parked, so a replay cannot leak one `commissions` entry per
            // already-emitted fill.
            return vec![];
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
            // `on_exec_details` completes it.
            self.park_commission(c);
            return vec![];
        };
        self.join_fill(&c.exec_id, p, c.commission, &c.currency)
    }

    /// Park a commission whose execution has not arrived yet. Bounded by
    /// [`MAX_PARKED_COMMISSIONS`]: at the cap the OLDEST park is evicted and `warn!`ed.
    ///
    /// **Oldest-first is the correct direction here** (the mirror of #937's front-trim, for the
    /// opposite reason): the oldest park has had the longest opportunity for its execDetails to
    /// arrive and still has not, so it is the likeliest to be genuinely orphaned, while the
    /// newest parks are precisely the ones still mid-race. The `min_by_key` scan is O(n), but it
    /// runs ONLY at the cap.
    fn park_commission(&mut self, c: IbCommissionReport) {
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
        let seq = self.commission_seq;
        self.commission_seq += 1;
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
    /// a no-op. Bounded by [`MAX_EMITTED_EXEC_IDS`], evicting the OLDEST first — the oldest
    /// execution is the least likely to still be replayed, and (unlike a `pending` eviction) losing
    /// the record cannot lose a fill.
    fn note_emitted(&mut self, exec_id: String) {
        if self.emitted.len() >= MAX_EMITTED_EXEC_IDS && !self.emitted.contains_key(&exec_id) {
            // Resolve to an OWNED key first so the `iter()` borrow ends before the `remove`.
            let oldest = self.emitted.iter().min_by_key(|(_, seq)| **seq).map(|(id, _)| id.clone());
            if let Some(oldest) = oldest {
                self.emitted.remove(&oldest);
            }
        }
        let seq = self.emitted_seq;
        self.emitted_seq += 1;
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
                    client_order_id: p.coid.clone(),
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
            client_order_id: p.coid.clone(),
            venue: ustr_lite("ibkr"),
            symbol: ustr_lite(&p.symbol),
            side: p.side,
            last_qty: p.shares,
            last_px: p.price,
            commission,
            commission_asset: ustr_lite(currency),
            liquidity_side: Default::default(),
            ts: p.ts,
            mark_price: None,
            position_side: Default::default(),
        };
        const EPS: f64 = 1e-9;
        // Record the emit BEFORE the partial-vs-final decision: a re-delivered half of this pair
        // must be a no-op on both sides from here on (see the `emitted` field doc).
        self.note_emitted(exec_id.to_string());
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
            self.accepted.remove(&p.coid);
            self.unacked.remove(&order_id);
            self.fill_state.remove(&order_id);
            self.retire(&p.coid);
            // A cancel that waited for this fill's commission is moot: the order completed.
            self.held_cancels.remove(&order_id);
        }
        let held_coid = (!p.recovered && !final_fill)
            .then(|| self.held_cancels.get(&order_id).map(|h| h.coid.clone()))
            .flatten();
        // `[Fill, wrap]`, like every other bridge (`vike_bridge_core::terminal_events`): the bare
        // `Fill` is what reaches `Account::apply_fill` (position, realized PnL, commission), the
        // wrapper only advances the order's FSM. Both carry the same `trade_id` (the IB execution
        // id), the dedup key of both folds.
        let mut out = terminal_events(p.coid, fill, p.ts, final_fill);
        // The cancel held back for the commissions of this order's fills goes out behind the last of them.
        if let Some(coid) = held_coid
            && !self.has_unjoined(order_id)
        {
            out.extend(self.finish_cancel(order_id, &coid));
        }
        out
    }

    /// Whether a fill of `order_id` (an order of THIS connection, not a recovered one) still waits for its
    /// commission.
    fn has_unjoined(&self, order_id: i32) -> bool {
        self.pending.values().any(|pf| !pf.recovered && pf.order_id == order_id)
    }

    /// The venue's cancel of `coid`, all its fills out: `OrderCanceled`, and everything kept per order dropped.
    fn finish_cancel(&mut self, order_id: i32, coid: &str) -> Vec<Event> {
        self.held_cancels.remove(&order_id);
        self.ids.forget(order_id);
        self.unacked.remove(&order_id);
        self.accepted.remove(coid);
        self.fill_state.remove(&order_id);
        self.retire(coid);
        vec![Event::OrderCanceled(OrderCanceled {
            client_order_id: coid.to_string(),
            reason: String::new().into(),
            ts: 0,
        })]
    }

    /// Let the cancels that have waited long enough for a commission go out ANYWAY: the commission is not
    /// coming (IB sends it right behind its execution). Called by the exec loop on its sweep cadence with a
    /// monotonic `now_ms` (the mapper has no clock; a held cancel is stamped by the first pass that sees it,
    /// so the wait is `(wait_ms - sweep period, wait_ms]`).
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
            if now_ms.saturating_sub(first) >= wait_ms {
                due.push(*order_id);
            }
        }
        due.sort_unstable();
        let mut out = Vec::new();
        for order_id in due {
            let Some(coid) = self.held_cancels.get(&order_id).map(|h| h.coid.clone()) else {
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
                tracing::error!(
                    exec_id = %exec_id,
                    coid = %pf.coid,
                    qty = pf.shares,
                    price = pf.price,
                    waited_ms = wait_ms,
                    "ibkr: the order was CANCELED and the commission of this execution did not arrive in \
                     time: the fill is booked with an UNKNOWN fee (0.0) - the position is right, the fee is \
                     short; a late commissionReport will be logged, put the fee right by hand"
                );
                let fill = FillEvent {
                    trade_id: pf.trade_id.clone(),
                    client_order_id: pf.coid.clone(),
                    venue: ustr_lite("ibkr"),
                    symbol: ustr_lite(&pf.symbol),
                    side: pf.side,
                    last_qty: pf.shares,
                    last_px: pf.price,
                    commission: 0.0,
                    commission_asset: ustr_lite(""),
                    liquidity_side: Default::default(),
                    ts: pf.ts,
                    mark_price: None,
                    position_side: Default::default(),
                };
                out.extend(terminal_events(pf.coid, fill, pf.ts, false));
                self.note_emitted(exec_id.clone());
                self.note_fee_unknown(exec_id);
            }
            out.extend(self.finish_cancel(order_id, &coid));
        }
        out
    }

    /// Remember that `exec_id` was booked with an unknown fee. Bounded like `emitted`, oldest first.
    fn note_fee_unknown(&mut self, exec_id: String) {
        if self.fee_unknown.len() >= MAX_EMITTED_EXEC_IDS
            && !self.fee_unknown.contains_key(&exec_id)
        {
            let oldest =
                self.fee_unknown.iter().min_by_key(|(_, seq)| **seq).map(|(id, _)| id.clone());
            if let Some(oldest) = oldest {
                self.fee_unknown.remove(&oldest);
            }
        }
        let seq = self.fee_unknown_seq;
        self.fee_unknown_seq += 1;
        self.fee_unknown.insert(exec_id, seq);
    }

    pub(crate) fn on_error(&mut self, code: i32, order_id: i32, msg: &str) -> Vec<Event> {
        match classify_code(code) {
            // Data-vs-Notice: advisory / connectivity / suppress → no order event (the transport
            // layer handles connectivity flag toggling; here we only avoid faking a terminal).
            CodeClass::Warning
            | CodeClass::ConnectivityLost
            | CodeClass::ConnectivityRestored
            | CodeClass::Suppress
            | CodeClass::Other => vec![],
            CodeClass::OrderRejection => {
                // An order-rejection-class error naming an order whose AMEND is in flight refuses the amend; the
                // order itself keeps working at its old terms and must not be terminalized (`exec::amend`).
                if order_id != 0
                    && let Some(ev) = self.amends.refuse_for_order(order_id, msg)
                {
                    return vec![ev];
                }
                // Direct resolution: the SYNCHRONOUS submit-failure path (exec.rs) — and any future
                // backend that carries the id — deliver a real `order_id`, which resolves straight
                // to its coid.
                if let Some(coid) = self.resolve(order_id, "") {
                    return self.reject(order_id, coid, msg);
                }
                // Async id-less path (no-order-vanishes fix): ibapi's global `order_update_stream`
                // drops the order id from order-rejection Notices — a hard rejection TWS delivers
                // asynchronously (AFTER submit_order returned Ok) arrives here as
                // `on_error(code, 0, msg)`. The `Notice` type structurally cannot carry the id, so
                // we attribute it to a still-unacked order. When EXACTLY ONE order is unacked the
                // rejection is unambiguously that order's → route it so the intent never silently
                // vanishes (an accepted order's later rejection would instead arrive as a resolvable
                // OrderStatus, not this id-less path).
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
                        && let Some(coid) = self.amends.sole_open()
                        && let Some(ev) = self.amends.refuse_unattributed(&coid, msg)
                    {
                        return vec![ev];
                    }
                } else if order_id == 0 && self.unacked.len() == 1 {
                    let oid = *self.unacked.iter().next().expect("len checked == 1");
                    if let Some(coid) = self.resolve(oid, "") {
                        return self.reject(oid, coid, msg);
                    }
                }
                // Zero or many unacked ⇒ we cannot attribute an id-less rejection without risking a
                // sibling order's state. Surface loudly; the core submit-ack watchdog is the
                // no-vanish backstop for this residual (confirm behaviour in the Task 12 smoke).
                if order_id == 0 {
                    tracing::warn!(
                        code,
                        unacked = self.unacked.len(),
                        "unroutable IBKR order-rejection notice; relying on submit-ack watchdog"
                    );
                }
                vec![]
            }
        }
    }

    /// Emit `OrderRejected` for `coid` and drop all per-order state (id map, unacked, accepted).
    fn reject(&mut self, order_id: i32, coid: String, msg: &str) -> Vec<Event> {
        self.ids.forget(order_id);
        self.unacked.remove(&order_id);
        self.accepted.remove(&coid);
        self.retire(&coid);
        vec![Event::OrderRejected(OrderRejected {
            client_order_id: coid,
            reason: msg.to_string().into(),
            ts: 0,
        })]
    }

    /// Expose the registry for reconnect resync (Task 10).
    pub(crate) fn ids_mut(&mut self) -> &mut IdRegistry {
        &mut self.ids
    }

    /// Read-only registry access — the readiness/stream-death queries `exec::run_exec` makes on
    /// every command, which have no business taking a `&mut`.
    pub(crate) fn ids(&self) -> &IdRegistry {
        &self.ids
    }

    /// Every coid this mapper still holds live per-order state for: submitted-but-unacked orders
    /// plus accepted ones. Used by `exec::run_exec`'s stream-death arm to NAME the orders whose
    /// venue state became unknowable, since nothing can be synthesized for them truthfully.
    pub(crate) fn live_client_order_ids(&self) -> Vec<&str> {
        let mut out: Vec<&str> = self
            .unacked
            .iter()
            .filter_map(|oid| self.ids.coid_of(*oid))
            .chain(self.accepted.iter().map(String::as_str))
            .collect();
        out.sort_unstable();
        out.dedup();
        out
    }

    /// Re-seed the coid⇄orderId map from one row of an open-order snapshot delivered after
    /// reconnect. IBKR does NOT replay open orders/executions on its own — `orderRef` IS the coid
    /// (that's how it round-trips through the venue), so this `bind`s `order_id → order_ref` into
    /// the `IdRegistry` exactly like a fresh submit would.
    ///
    /// Two cases, distinguished by the `unacked` set ("core submitted this but has NOT yet seen its
    /// `OrderAccepted`"):
    ///
    /// - **`order_id` was in `unacked`** — the order was mid-flight when the socket blipped: the
    ///   core submitted it, IB accepted it server-side, but the confirming
    ///   `OrderStatus(Submitted/PreSubmitted)` was lost before it arrived. The core is still waiting
    ///   for the accept, so we MUST emit `OrderAccepted` (via the idempotent `ensure_accepted` idiom,
    ///   exactly like the exec-before-open synthesis in `on_exec_details`) to unstick it — otherwise
    ///   the `ManagedOrder` FSM never leaves `Submitted` and the order's eventual terminal is dropped
    ///   as an invalid transition → the order silently vanishes.
    /// - **`order_id` was NOT in `unacked`** — an EXTERNAL order (opened in a previous
    ///   session/process this instance never submitted), or one already accepted before the blip
    ///   (already in `accepted`). Mark it accepted SILENTLY and emit nothing: re-emitting would be a
    ///   spurious duplicate, or an accept for a coid the core never submitted.
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
    /// The coids this connection rebound (a working or resting order of ours).
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
    /// did not decode (0 and empty) and its `orderRef` has the shape of a coid.
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
        snapshot: crate::transport::Snapshot,
        complete: bool,
    ) -> LearnStep {
        use crate::transport::Snapshot;
        match (self.learning, snapshot) {
            (Learning::OpenOrders, Snapshot::OpenOrders) if complete => {
                if self.stats.suspicious() {
                    self.learning = Learning::Failed;
                    return LearnStep::Suspicious;
                }
                self.learning = Learning::Executions;
                self.orders_unknown = false;
                LearnStep::RequestExecutions
            }
            (Learning::Executions, Snapshot::Executions) if complete => {
                self.settle_learned_totals();
                self.learning = Learning::Done;
                LearnStep::Done
            }
            (Learning::OpenOrders, Snapshot::OpenOrders)
            | (Learning::Executions, Snapshot::Executions) => {
                self.learning = Learning::Failed;
                LearnStep::Failed
            }
            _ => LearnStep::Ignore,
        }
    }

    /// Remember that `exec_id` matched no local order. Bounded like `emitted`, oldest first.
    fn note_unroutable(&mut self, exec_id: String) {
        if self.unroutable.len() >= MAX_EMITTED_EXEC_IDS && !self.unroutable.contains_key(&exec_id)
        {
            let oldest =
                self.unroutable.iter().min_by_key(|(_, seq)| **seq).map(|(id, _)| id.clone());
            if let Some(oldest) = oldest {
                self.unroutable.remove(&oldest);
            }
        }
        let seq = self.unroutable_seq;
        self.unroutable_seq += 1;
        self.unroutable.insert(exec_id, seq);
    }

    /// One row of the VENUE's open-order snapshot, already established to be an order of THIS client
    /// (`OwnOrders`). Three cases:
    ///
    /// * the row is unusable (no `orderRef`, no positive id or quantity, or the order already
    ///   finished): nothing is bound, nothing is emitted;
    /// * the connection is LEARNING (a fresh connection's snapshot): the row is an order a previous
    ///   connection of this client placed, and it is REBOUND here: the coid⇄id map learns it (so a cancel
    ///   reaches IB under the right id and its status and executions resolve), its fill tracking is
    ///   seeded (`total` from the row, `filled` 0: the executions replay that follows adds the fills, so
    ///   the first replayed partial is not mistaken for the last), and an `OrderAccepted` is emitted for it
    ///   when it is live at the venue. The accept is idempotent in the engine's FSM (a duplicate on an
    ///   order that is already accepted is dropped) and it is what lifts an order whose accept the outage
    ///   swallowed out of `Submitted` (review of W14b, finding 5);
    /// * otherwise (an `openOrder` update during normal operation): the long-standing
    ///   [`EventMapper::rebind_open_order`].
    pub(crate) fn on_venue_order(&mut self, row: &IbOpenOrder) -> Vec<Event> {
        if row.order_id <= 0 || row.order_ref.is_empty() {
            tracing::debug!(
                order_id = row.order_id,
                "ibkr: open-order row without an id or an orderRef — not ours to bind"
            );
            return vec![];
        }
        let kind = OrderStatusKind::from_ib(&row.status);
        if kind.is_terminal() {
            tracing::debug!(
                order_id = row.order_id,
                coid = %row.order_ref,
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
            tracing::warn!(
                order_id = row.order_id,
                coid = %row.order_ref,
                "ibkr: open-order row of our own for an order this connection does not hold, outside the
                 learning window — not bound (cancel it in TWS if it is meant to be managed)"
            );
            return vec![];
        }
        if !(row.total_qty.is_finite() && row.total_qty > 0.0) {
            tracing::warn!(
                order_id = row.order_id,
                coid = %row.order_ref,
                total_qty = row.total_qty,
                "ibkr: open-order row without a usable quantity — not rebound (its fills could not be classified)"
            );
            return vec![];
        }
        if self.ids.bind_rebound(row.order_id, &row.order_ref) == Rebound::Conflict {
            tracing::warn!(
                order_id = row.order_id,
                coid = %row.order_ref,
                "ibkr: open-order row contradicts a binding this connection holds — not rebound"
            );
            return vec![];
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

#[path = "event_mapper_props.rs"]
#[cfg(test)]
mod event_mapper_props;
