//! The event-correctness heart: IB callback reports → canonical vike `Event`s, with the four traps
//! the ibapi + Nautilus studies surfaced baked in:
//!   1. Exec-before-open synthesis — a fill arriving before ACCEPTED synthesizes `OrderAccepted`.
//!   2. `execution_id` commission join — execDetails + commissionReport are two async messages;
//!      buffer by exec_id and emit the fill only once its commission joins (never pair by arrival).
//!      The join is **symmetric**: IBKR delivers the two halves in EITHER order, so whichever lands
//!      first is parked (`pending` for the execution, `commissions` for the commission) and the
//!      second one completes the pair.
//!      `crates/bridges/vike-ibkr/vendor/ibapi/src/orders/mod.rs`'s `CommissionReport` is the
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
//!      (`crates/bridges/vike-ibkr/vendor/ibapi/src/orders/sync.rs`'s `executions` — *"Along with
//!      the ExecutionData, the CommissionReport will also be returned"*), so
//!      [`EventMapper::sweep_pending`] reports a stranded execution to the exec loop, which
//!      re-requests them and lets the ordinary join emit the fill with the REAL commission. See
//!      that method for the bound, the retry budget, and the residual it does NOT close.
//!
//! Pure + fixture-tested; owns an `IdRegistry`. Consumed by the exec loop (Task 8,
//! `exec::run_exec` folds transport inbound through it).
//!
//! **Clockless by construction.** Nothing here reads a clock. `sweep_pending` takes `now_ms` as a
//! PARAMETER and uses only DIFFERENCES of it, so every test passes explicit millis and stays
//! deterministic — the same event-driven-not-clock-driven discipline the parked-commission expiry
//! in [`EventMapper::on_exec_details`] already follows.

use std::collections::HashMap;

use crate::error::{CodeClass, classify_code};
use crate::id_registry::IdRegistry;
use crate::order::OrderStatusKind;
use vike_model::events::{
    Event, FillEvent, OrderAccepted, OrderCanceled, OrderFilled, OrderPartiallyFilled,
    OrderRejected, TradeId,
};

pub struct IbOrderStatus {
    pub order_id: i32,
    pub order_ref: String,
    pub status: String,
    pub filled: f64,
    pub avg_fill_price: f64,
}

pub struct IbExecDetails {
    pub order_id: i32,
    pub order_ref: String,
    pub exec_id: String,
    pub symbol: String,
    pub side_buy: bool,
    pub shares: f64,
    pub price: f64,
    pub ts: i64,
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
pub const PENDING_COMMISSION_GRACE_MS: i64 = 15_000;

/// How many venue re-requests ONE stranded execution gets before it is escalated to the operator.
/// Each attempt restarts that entry's clock, so the ladder is ~15s / 30s / 45s before the
/// `error!`. Bounded because a re-request that has already failed three times is not failing for a
/// reason a fourth will fix — it means IBKR has no commissionReport for that execution, which is an
/// operator problem, not a retry problem.
pub const MAX_PENDING_RECOVERY_ATTEMPTS: u32 = 3;

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
pub struct StrandedFill {
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
pub struct PendingSweep {
    /// Executions past the grace window with retry budget left: the caller should ask the venue to
    /// re-deliver the day's executions. ONE account-wide request covers every id in here.
    pub recover: Vec<String>,
    /// Executions whose budget is spent, reported ONCE each.
    pub stranded: Vec<StrandedFill>,
}

impl PendingSweep {
    /// Nothing to do this pass — the steady state.
    pub fn is_empty(&self) -> bool {
        self.recover.is_empty() && self.stranded.is_empty()
    }
}

/// Cumulative-vs-total fill tracking for one order, keyed by `order_id` — see
/// `on_commission_report`'s partial-vs-final decision.
#[derive(Default)]
struct FillState {
    total: f64,
    filled: f64,
}

pub struct EventMapper {
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
    /// exec_id → commission awaiting its execution: the reverse index the vendored ibapi docs
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
}

impl EventMapper {
    pub fn new(ids: IdRegistry) -> Self {
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
        }
    }

    /// Record that `order_id` was submitted (called by the exec loop right after `bind`, before
    /// `place_order`). It stays "unacked" until the order goes active or terminal — this is the
    /// population an id-less async order-rejection is attributed to (see `on_error`). Also seeds
    /// `fill_state` with the requested `total_qty` so `on_commission_report` can tell a partial
    /// fill from the one that completes the order.
    pub fn on_submit(&mut self, order_id: i32, total_qty: f64) {
        self.unacked.insert(order_id);
        self.fill_state.insert(order_id, FillState { total: total_qty, filled: 0.0 });
    }

    fn resolve(&self, order_id: i32, order_ref: &str) -> Option<String> {
        self.ids.resolve(order_id, order_ref)
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

    pub fn on_order_status(&mut self, s: IbOrderStatus) -> Vec<Event> {
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
            // Flush fills still awaiting commission for this order as partials (commission
            // best-effort 0.0), dedup their exec_id so the late real commission is a no-op — no
            // fill dropped, no post-terminal fill. MUST happen before the OrderCanceled push so
            // the FSM sees a valid [...PartiallyFilled, Canceled] order.
            //
            // `commissions` is deliberately NOT swept here: a parked commission is keyed only by
            // exec_id and its execution never arrived, so there is nothing to attribute it to this
            // order_id with. It expires on its own terms — when its execDetails finally arrives
            // and resolves to no local order (this cancel forgot the id), `on_exec_details`
            // releases it with a `warn!`.
            let stale: Vec<String> = self
                .pending
                .iter()
                .filter(|(_, pf)| pf.order_id == s.order_id)
                .map(|(id, _)| id.clone())
                .collect();
            for exec_id in stale {
                if let Some(pf) = self.pending.remove(&exec_id) {
                    let fill = FillEvent {
                        // Validated at ingress and carried on the entry — see `PendingFill`.
                        trade_id: pf.trade_id.clone(),
                        client_order_id: pf.coid.clone(),
                        venue: ustr_lite("ibkr"),
                        symbol: ustr_lite(&pf.symbol),
                        side: pf.side,
                        last_qty: pf.shares,
                        last_px: pf.price,
                        commission: 0.0,
                        commission_asset: ustr_lite("USD"),
                        liquidity_side: Default::default(),
                        ts: pf.ts,
                        mark_price: None,
                        position_side: Default::default(),
                    };
                    out.push(Event::OrderPartiallyFilled(OrderPartiallyFilled {
                        client_order_id: pf.coid,
                        fill,
                        ts: pf.ts,
                    }));
                    self.note_emitted(exec_id);
                }
            }
            out.push(Event::OrderCanceled(OrderCanceled {
                client_order_id: coid.clone(),
                reason: String::new().into(),
                ts: 0,
            }));
            self.ids.forget(s.order_id);
            self.unacked.remove(&s.order_id);
            self.accepted.remove(&coid);
            self.fill_state.remove(&s.order_id);
        }
        // Filled status is folded from execDetails+commission (below), not here (avg-price-only
        // status can't carry the exec_id vike's FillEvent needs).
        out
    }

    pub fn on_exec_details(&mut self, e: IbExecDetails) -> Vec<Event> {
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
        let Some(coid) = self.resolve(e.order_id, &e.order_ref) else {
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
        };
        let mut out = vec![];
        // Exec-before-open: a fill before ACCEPTED synthesizes the accept to backfill the mapping.
        if let Some(ev) = self.ensure_accepted(&coid, e.order_id) {
            out.push(ev);
        }
        let p = PendingFill {
            order_id: e.order_id,
            trade_id,
            coid,
            symbol: e.symbol,
            side: if e.side_buy { 1 } else { -1 },
            shares: e.shares,
            price: e.price,
            ts: e.ts,
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
            out.push(self.join_fill(&e.exec_id, p, pc.commission, &pc.currency));
            return out;
        }
        // Exec-first: buffer the fill by exec_id; it emits when its commission joins.
        self.pending.insert(e.exec_id, p);
        out
    }

    pub fn on_commission_report(&mut self, c: IbCommissionReport) -> Vec<Event> {
        if self.emitted.contains_key(&c.exec_id) {
            // A commission for an exec whose fill already emitted: a late real report for a
            // cancel-flushed partial, an IB re-send, or a `reqExecutions` replay. No-op — and
            // crucially NOT parked, so a replay cannot leak one `commissions` entry per
            // already-emitted fill.
            return vec![];
        }
        let Some(p) = self.pending.remove(&c.exec_id) else {
            // NOT a drop any more. This arm used to `return vec![]`, on the assumption that a
            // commission can only precede its execution when IB re-sends. It cannot: IBKR
            // delivers the two halves in either order by design (module doc trap 2, quoting
            // `crates/bridges/vike-ibkr/vendor/ibapi/src/orders/mod.rs`'s `CommissionReport`).
            // Discarding the commission ALSO stranded the execution that arrived afterwards — it
            // sat in `pending` forever, reachable by no sweep (the only one is `on_order_status`'s
            // cancel arm, and a filled order is not cancelled; reconnect re-requests OPEN orders
            // only, and a filled order is not open). So the fill never emitted: position and
            // realized PnL silently disagreed with the venue. Park the half we have;
            // `on_exec_details` completes it.
            self.park_commission(c);
            return vec![];
        };
        vec![self.join_fill(&c.exec_id, p, c.commission, &c.currency)]
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
    pub fn sweep_pending(&mut self, now_ms: i64, grace_ms: i64) -> PendingSweep {
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
    ) -> Event {
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
        let final_fill = match self.fill_state.get_mut(&order_id) {
            Some(st) => {
                st.filled += p.shares;
                st.filled + EPS >= st.total
            }
            None => true, // unknown total → terminal (external/rebound; preserves prior behaviour)
        };
        if final_fill {
            self.ids.forget(order_id);
            self.accepted.remove(&p.coid);
            self.unacked.remove(&order_id);
            self.fill_state.remove(&order_id);
            Event::OrderFilled(OrderFilled { client_order_id: p.coid.clone(), fill, ts: p.ts })
        } else {
            Event::OrderPartiallyFilled(OrderPartiallyFilled {
                client_order_id: p.coid.clone(),
                fill,
                ts: p.ts,
            })
        }
    }

    pub fn on_error(&mut self, code: i32, order_id: i32, msg: &str) -> Vec<Event> {
        match classify_code(code) {
            // Data-vs-Notice: advisory / connectivity / suppress → no order event (the transport
            // layer handles connectivity flag toggling; here we only avoid faking a terminal).
            CodeClass::Warning
            | CodeClass::ConnectivityLost
            | CodeClass::ConnectivityRestored
            | CodeClass::Suppress
            | CodeClass::Other => vec![],
            CodeClass::OrderRejection => {
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
                if order_id == 0 && self.unacked.len() == 1 {
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
        vec![Event::OrderRejected(OrderRejected {
            client_order_id: coid,
            reason: msg.to_string().into(),
            ts: 0,
        })]
    }

    /// Expose the registry for reconnect resync (Task 10).
    pub fn ids_mut(&mut self) -> &mut IdRegistry {
        &mut self.ids
    }

    /// Read-only registry access — the readiness/stream-death queries `exec::run_exec` makes on
    /// every command, which have no business taking a `&mut`.
    pub fn ids(&self) -> &IdRegistry {
        &self.ids
    }

    /// Every coid this mapper still holds live per-order state for: submitted-but-unacked orders
    /// plus accepted ones. Used by `exec::run_exec`'s stream-death arm to NAME the orders whose
    /// venue state became unknowable, since nothing can be synthesized for them truthfully.
    pub fn live_client_order_ids(&self) -> Vec<&str> {
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
    pub fn rebind_open_order(&mut self, order_id: i32, order_ref: &str) -> Vec<Event> {
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

/// `FillEvent.venue`/`symbol`/`commission_asset` are `Ustr` (interned). Bounded cardinality (one
/// venue, per-session symbols) so interning is sound — see vike_model::events interning contract.
fn ustr_lite(s: &str) -> ustr::Ustr {
    ustr::ustr(s)
}

#[path = "event_mapper_tests.rs"]
#[cfg(test)]
mod event_mapper_tests;
