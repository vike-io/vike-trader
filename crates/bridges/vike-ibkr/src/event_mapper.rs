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

use crate::error::{classify_code, CodeClass};
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
        if kind.is_active() {
            if let Some(ev) = self.ensure_accepted(&coid, s.order_id) {
                out.push(ev);
            }
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::id_registry::IdRegistry;
    use vike_model::events::Event;

    fn mapper_with(coid: &str, order_id: i32) -> EventMapper {
        let mut ids = IdRegistry::default();
        ids.on_next_valid_id(order_id);
        let alloc = ids.next_order_id();
        assert_eq!(alloc, order_id);
        ids.bind(order_id, coid);
        EventMapper::new(ids)
    }

    /// An execDetails with an EMPTY `execId` is refused at ingress: no events, and nothing buffered,
    /// so its commissionReport cannot later join and mint a fill either.
    ///
    /// This gates the `TradeId::new` guard in `on_exec_details`, not the type. Reverting it to a
    /// permissive id turns both asserted emptinesses into fills — and the hazard is worse than one
    /// un-dedupable fill: `emitted`/`pending`/`commissions` are ALL keyed on the exec id, so several
    /// id-less executions would share one `""` slot, overwriting each other's halves and making a
    /// `reqExecutions` replay (which returns the whole day) re-emit or swallow at random.
    #[test]
    fn exec_details_without_an_exec_id_is_refused_at_ingress() {
        let mut m = mapper_with("coid-A", 101);
        let evs = m.on_exec_details(IbExecDetails {
            order_id: 101,
            order_ref: "coid-A".into(),
            exec_id: String::new(),
            symbol: "AAPL.SMART.USD".into(),
            side_buy: true,
            shares: 10.0,
            price: 190.0,
            ts: 5,
        });
        assert!(evs.is_empty(), "an id-less execution emits nothing, not even the synth accept");

        // ...and nothing was buffered, so the matching commission cannot mint a fill afterwards.
        let joined = m.on_commission_report(IbCommissionReport {
            exec_id: String::new(),
            commission: 1.0,
            currency: "USD".into(),
        });
        assert!(joined.is_empty(), "no pending half exists to join: {joined:?}");
    }

    #[test]
    fn fill_before_accept_synthesizes_accepted_then_filled() {
        // A fresh mapper whose id is bound but never saw an ACCEPTED status: an execDetails must
        // synthesize OrderAccepted, then (after the commission joins) OrderFilled.
        let mut m = mapper_with("coid-A", 101);
        let evs = m.on_exec_details(IbExecDetails {
            order_id: 101,
            order_ref: "coid-A".into(),
            exec_id: "e1".into(),
            symbol: "AAPL.SMART.USD".into(),
            side_buy: true,
            shares: 10.0,
            price: 190.0,
            ts: 5,
        });
        // Accepted is emitted immediately; the fill waits for the commission join.
        assert!(matches!(evs.as_slice(), [Event::OrderAccepted(_)]));
        let evs2 = m.on_commission_report(IbCommissionReport {
            exec_id: "e1".into(),
            commission: 1.0,
            currency: "USD".into(),
        });
        match evs2.as_slice() {
            [Event::OrderFilled(f)] => {
                assert_eq!(f.client_order_id, "coid-A");
                assert_eq!(f.fill.last_qty, 10.0);
                assert_eq!(f.fill.commission, 1.0);
                assert_eq!(f.fill.trade_id, "e1");
            }
            other => panic!("expected OrderFilled, got {other:?}"),
        }
    }

    #[test]
    fn exec_details_buffered_until_commission_joins_by_exec_id() {
        let mut m = mapper_with("coid-A", 101);
        // Pre-accept via a Submitted status so no synth is needed.
        let _ = m.on_order_status(IbOrderStatus {
            order_id: 101,
            order_ref: "coid-A".into(),
            status: "Submitted".into(),
            filled: 0.0,
            avg_fill_price: 0.0,
        });
        let e = m.on_exec_details(IbExecDetails {
            order_id: 101,
            order_ref: "coid-A".into(),
            exec_id: "e9".into(),
            symbol: "AAPL.SMART.USD".into(),
            side_buy: false,
            shares: 4.0,
            price: 191.0,
            ts: 7,
        });
        assert!(e.is_empty(), "fill must NOT emit before its commission joins");
        let joined = m.on_commission_report(IbCommissionReport {
            exec_id: "e9".into(),
            commission: 0.5,
            currency: "USD".into(),
        });
        assert!(matches!(joined.as_slice(), [Event::OrderFilled(_)]));
    }

    #[test]
    fn advisory_code_emits_nothing() {
        let mut m = mapper_with("coid-A", 101);
        assert!(m.on_error(2104, 101, "Market data farm connection is OK").is_empty());
        assert!(m.on_error(2137, 101, "advisory").is_empty());
    }

    #[test]
    fn order_rejection_code_emits_reject_and_forgets() {
        let mut m = mapper_with("coid-A", 101);
        let evs = m.on_error(201, 101, "Order rejected - reason: insufficient buying power");
        assert!(matches!(evs.as_slice(), [Event::OrderRejected(_)]));
    }

    #[test]
    fn idless_async_rejection_routes_to_sole_unacked_order() {
        // ibapi's global order_update_stream drops the order id from order-rejection Notices, so an
        // async hard rejection (delivered AFTER submit_order returned Ok) arrives as
        // on_error(code, 0, msg). With exactly one order still unacked, it must resolve to that
        // order and emit OrderRejected — the no-order-vanishes contract.
        let mut m = mapper_with("coid-A", 101);
        m.on_submit(101, 1.0);
        let evs = m.on_error(201, 0, "Order rejected - insufficient buying power");
        match evs.as_slice() {
            [Event::OrderRejected(r)] => assert_eq!(r.client_order_id, "coid-A"),
            other => panic!("expected OrderRejected, got {other:?}"),
        }
        // Idempotent: after the reject the order is forgotten (zero unacked) → a duplicate id-less
        // notice synthesizes no second terminal.
        assert!(m.on_error(201, 0, "duplicate").is_empty());
    }

    #[test]
    fn idless_async_rejection_ambiguous_when_multiple_unacked() {
        // Two orders in flight: an id-less rejection cannot be safely attributed, so NO terminal is
        // synthesized here (the core submit-ack watchdog is the backstop). Never corrupt a sibling.
        let mut ids = IdRegistry::default();
        ids.on_next_valid_id(101);
        let a = ids.next_order_id();
        ids.bind(a, "coid-A");
        let b = ids.next_order_id();
        ids.bind(b, "coid-B");
        let mut m = EventMapper::new(ids);
        m.on_submit(a, 1.0);
        m.on_submit(b, 1.0);
        assert!(m.on_error(201, 0, "ambiguous").is_empty());
    }

    #[test]
    fn idless_async_rejection_ignored_after_order_acked() {
        // Once the sole in-flight order goes active it leaves the unacked set, so a stray id-less
        // rejection no longer misattributes to it (an accepted order rejects via a resolvable
        // OrderStatus, not this path).
        let mut m = mapper_with("coid-A", 101);
        m.on_submit(101, 1.0);
        let _ = m.on_order_status(IbOrderStatus {
            order_id: 101,
            order_ref: "coid-A".into(),
            status: "Submitted".into(),
            filled: 0.0,
            avg_fill_price: 0.0,
        });
        assert!(m.on_error(201, 0, "late").is_empty());
    }

    #[test]
    fn rebind_of_unacked_order_emits_accepted_once() {
        // Mid-flight order: core submitted it (unacked), IB accepted server-side, but the confirming
        // OrderStatus was lost in a socket blip. On reconnect IB replays it as an OpenOrder →
        // rebind_open_order MUST emit exactly one OrderAccepted to unstick the FSM, and a later
        // Submitted status must NOT re-emit a second (idempotent).
        let mut m = mapper_with("coid-A", 101);
        m.on_submit(101, 1.0);
        let evs = m.rebind_open_order(101, "coid-A");
        match evs.as_slice() {
            [Event::OrderAccepted(a)] => {
                assert_eq!(a.client_order_id, "coid-A");
                assert_eq!(a.venue_order_id.as_deref(), Some("101"));
            }
            other => panic!("expected exactly one OrderAccepted, got {other:?}"),
        }
        // A subsequent active status for the same order does NOT emit a duplicate accept.
        let dup = m.on_order_status(IbOrderStatus {
            order_id: 101,
            order_ref: "coid-A".into(),
            status: "Submitted".into(),
            filled: 0.0,
            avg_fill_price: 0.0,
        });
        assert!(dup.is_empty(), "second accept must not be emitted (idempotent)");
    }

    #[test]
    fn rebind_of_external_order_emits_nothing_and_stays_resolvable() {
        // An EXTERNAL order (never submitted this process → not in unacked, e.g. opened in a prior
        // session): rebind emits NOTHING, but the id map is seeded so a later Cancelled still
        // resolves to its coid and emits exactly one OrderCanceled.
        let mut ids = IdRegistry::default();
        ids.on_next_valid_id(55);
        let mut m = EventMapper::new(ids);
        let evs = m.rebind_open_order(55, "coid-ext");
        assert!(evs.is_empty(), "external rebind must emit nothing");
        let cancel = m.on_order_status(IbOrderStatus {
            order_id: 55,
            order_ref: "coid-ext".into(),
            status: "Cancelled".into(),
            filled: 0.0,
            avg_fill_price: 0.0,
        });
        match cancel.as_slice() {
            [Event::OrderCanceled(c)] => assert_eq!(c.client_order_id, "coid-ext"),
            other => panic!("expected exactly one OrderCanceled, got {other:?}"),
        }
    }

    #[test]
    fn terminal_status_cleans_the_map() {
        let mut m = mapper_with("coid-A", 101);
        let evs = m.on_order_status(IbOrderStatus {
            order_id: 101,
            order_ref: "coid-A".into(),
            status: "Cancelled".into(),
            filled: 0.0,
            avg_fill_price: 0.0,
        });
        assert!(matches!(evs.as_slice(), [Event::OrderCanceled(_)]));
        // After a terminal, the id is forgotten → a late duplicate resolves to nothing.
        let dup = m.on_order_status(IbOrderStatus {
            order_id: 101,
            order_ref: "".into(),
            status: "Cancelled".into(),
            filled: 0.0,
            avg_fill_price: 0.0,
        });
        assert!(dup.is_empty());
    }

    fn mapper_submitted(coid: &str, order_id: i32, qty: f64) -> EventMapper {
        let mut m = mapper_with(coid, order_id);
        m.on_submit(order_id, qty);
        // ack it so fills don't need exec-before-open synthesis noise in these tests
        let _ = m.on_order_status(IbOrderStatus {
            order_id,
            order_ref: coid.into(),
            status: "Submitted".into(),
            filled: 0.0,
            avg_fill_price: 0.0,
        });
        m
    }

    fn commission(m: &mut EventMapper, exec_id: &str) -> Vec<Event> {
        m.on_commission_report(IbCommissionReport {
            exec_id: exec_id.into(),
            commission: 0.0,
            currency: "USD".into(),
        })
    }
    fn exec(
        m: &mut EventMapper,
        order_id: i32,
        coid: &str,
        exec_id: &str,
        shares: f64,
    ) -> Vec<Event> {
        m.on_exec_details(IbExecDetails {
            order_id,
            order_ref: coid.into(),
            exec_id: exec_id.into(),
            symbol: "AAPL.SMART.USD".into(),
            side_buy: true,
            shares,
            price: 190.0,
            ts: 1,
        })
    }

    #[test]
    fn multi_fill_emits_partial_then_final_and_forgets() {
        let mut m = mapper_submitted("coid-A", 101, 10.0);
        let _ = exec(&mut m, 101, "coid-A", "e1", 4.0);
        assert!(matches!(commission(&mut m, "e1").as_slice(), [Event::OrderPartiallyFilled(_)]));
        let _ = exec(&mut m, 101, "coid-A", "e2", 6.0);
        assert!(matches!(commission(&mut m, "e2").as_slice(), [Event::OrderFilled(_)]));
        // forgotten: a stray later exec for order 101 resolves to nothing
        assert!(exec(&mut m, 101, "", "e3", 1.0).is_empty());
    }

    #[test]
    fn single_full_fill_is_terminal_and_forgets() {
        let mut m = mapper_submitted("coid-A", 101, 5.0);
        let _ = exec(&mut m, 101, "coid-A", "e1", 5.0);
        assert!(matches!(commission(&mut m, "e1").as_slice(), [Event::OrderFilled(_)]));
        assert!(m
            .on_order_status(IbOrderStatus {
                order_id: 101,
                order_ref: "".into(),
                status: "Filled".into(),
                filled: 5.0,
                avg_fill_price: 190.0,
            })
            .is_empty());
    }

    #[test]
    fn unknown_total_fill_falls_back_to_terminal_filled() {
        // No on_submit(total) → external/unknown → keep current OrderFilled-terminal behaviour.
        // (order_id 101, not the plan text's illustrative 55: mapper_with asserts the allocated id
        // round-trips, and IdRegistry::next_order_id floors every allocation at 101.)
        let mut m = mapper_with("coid-X", 101);
        let _ = m.on_order_status(IbOrderStatus {
            order_id: 101,
            order_ref: "coid-X".into(),
            status: "Submitted".into(),
            filled: 0.0,
            avg_fill_price: 0.0,
        });
        let _ = exec(&mut m, 101, "coid-X", "e1", 3.0);
        assert!(matches!(commission(&mut m, "e1").as_slice(), [Event::OrderFilled(_)]));
    }

    #[test]
    fn partial_then_cancel_is_valid_and_forgets() {
        let mut m = mapper_submitted("coid-A", 101, 10.0);
        let _ = exec(&mut m, 101, "coid-A", "e1", 4.0);
        assert!(matches!(commission(&mut m, "e1").as_slice(), [Event::OrderPartiallyFilled(_)]));
        let c = m.on_order_status(IbOrderStatus {
            order_id: 101,
            order_ref: "coid-A".into(),
            status: "Cancelled".into(),
            filled: 4.0,
            avg_fill_price: 190.0,
        });
        assert!(matches!(c.as_slice(), [Event::OrderCanceled(_)]));
        // forgotten
        assert!(m
            .on_order_status(IbOrderStatus {
                order_id: 101,
                order_ref: "".into(),
                status: "Cancelled".into(),
                filled: 4.0,
                avg_fill_price: 0.0,
            })
            .is_empty());
    }

    #[test]
    fn cancel_with_commission_in_flight_flushes_partial_then_cancels_and_dedups() {
        let mut m = mapper_submitted("coid-A", 101, 10.0);
        let _ = exec(&mut m, 101, "coid-A", "e1", 4.0); // execDetails, NO commission yet
        let out = m.on_order_status(IbOrderStatus {
            order_id: 101,
            order_ref: "coid-A".into(),
            status: "Cancelled".into(),
            filled: 4.0,
            avg_fill_price: 190.0,
        });
        // flush emits the partial (commission 0) BEFORE the cancel
        match out.as_slice() {
            [Event::OrderPartiallyFilled(p), Event::OrderCanceled(c)] => {
                assert_eq!(p.client_order_id, "coid-A");
                assert_eq!(p.fill.commission, 0.0);
                assert_eq!(c.client_order_id, "coid-A");
            }
            other => panic!("expected [PartiallyFilled, Canceled], got {other:?}"),
        }
        // the late real commission for e1 is a no-op (its fill already emitted → no double emit)
        assert!(commission(&mut m, "e1").is_empty());
        // …and it is NOT parked either: `emitted` is checked before the park, so a dedup'd
        // commission cannot linger in the reverse index.
        assert!(m.commissions.is_empty());
    }

    // -----------------------------------------------------------------------------------------
    // Commission-before-exec: the arrival order IBKR explicitly permits
    // (`crates/bridges/vike-ibkr/vendor/ibapi/src/orders/mod.rs`'s `CommissionReport`). Before the
    // reverse index existed, the commission was discarded on arrival AND its execution then
    // stranded in `pending` forever, so the fill was never emitted at all.
    // -----------------------------------------------------------------------------------------

    #[test]
    fn commission_before_exec_emits_exactly_one_fill_instead_of_dropping_it() {
        let mut m = mapper_submitted("coid-A", 101, 10.0);
        let parked = m.on_commission_report(IbCommissionReport {
            exec_id: "e1".into(),
            commission: 1.25,
            currency: "USD".into(),
        });
        assert!(
            parked.is_empty(),
            "the commission alone emits nothing — it is parked, not dropped"
        );
        let evs = m.on_exec_details(IbExecDetails {
            order_id: 101,
            order_ref: "coid-A".into(),
            exec_id: "e1".into(),
            symbol: "AAPL.SMART.USD".into(),
            side_buy: true,
            shares: 10.0,
            price: 190.0,
            ts: 5,
        });
        match evs.as_slice() {
            [Event::OrderFilled(f)] => {
                assert_eq!(f.client_order_id, "coid-A");
                assert_eq!(f.fill.trade_id, "e1");
                assert_eq!(f.fill.last_qty, 10.0);
                assert_eq!(f.fill.last_px, 190.0);
                // the parked half's values are carried onto the fill, not defaulted away
                assert_eq!(f.fill.commission, 1.25);
                assert_eq!(f.fill.commission_asset, "USD");
                assert_eq!(f.fill.ts, 5);
            }
            other => panic!("expected exactly one OrderFilled, got {other:?}"),
        }
        assert!(m.commissions.is_empty(), "the park is consumed by the join");
        assert!(m.pending.is_empty(), "nothing strands in pending");
    }

    #[test]
    fn both_arrival_orders_produce_an_identical_fill() {
        // The join is by exec_id, never by arrival, so the two orderings must be indistinguishable
        // downstream — the property the vendored ibapi docs prescribe.
        let fill_of = |commission_first: bool| -> FillEvent {
            let mut m = mapper_submitted("coid-A", 101, 3.0);
            let c = IbCommissionReport {
                exec_id: "e1".into(),
                commission: 0.75,
                currency: "USD".into(),
            };
            let d = IbExecDetails {
                order_id: 101,
                order_ref: "coid-A".into(),
                exec_id: "e1".into(),
                symbol: "AAPL.SMART.USD".into(),
                side_buy: false,
                shares: 3.0,
                price: 188.5,
                ts: 42,
            };
            let evs = if commission_first {
                assert!(m.on_commission_report(c).is_empty());
                m.on_exec_details(d)
            } else {
                assert!(m.on_exec_details(d).is_empty());
                m.on_commission_report(c)
            };
            match evs.as_slice() {
                [Event::OrderFilled(f)] => f.fill.clone(),
                other => panic!("expected exactly one OrderFilled, got {other:?}"),
            }
        };
        assert_eq!(fill_of(true), fill_of(false));
    }

    #[test]
    fn commission_before_exec_still_synthesizes_the_accept_first() {
        // Exec-before-open AND commission-before-exec at once: the accept must still lead, so the
        // FSM sees a valid [Accepted, Filled] pair from the single call.
        let mut m = mapper_with("coid-A", 101);
        assert!(commission(&mut m, "e1").is_empty());
        let evs = exec(&mut m, 101, "coid-A", "e1", 10.0);
        match evs.as_slice() {
            [Event::OrderAccepted(a), Event::OrderFilled(f)] => {
                assert_eq!(a.client_order_id, "coid-A");
                assert_eq!(f.client_order_id, "coid-A");
            }
            other => panic!("expected [OrderAccepted, OrderFilled], got {other:?}"),
        }
    }

    #[test]
    fn commission_first_partials_account_identically_to_exec_first() {
        // Both legs reversed: `fill_state`'s partial-vs-final accounting must be unchanged, since
        // it advances in the shared `join_fill` rather than in either arrival path.
        let mut m = mapper_submitted("coid-A", 101, 10.0);
        assert!(commission(&mut m, "e1").is_empty());
        assert!(matches!(
            exec(&mut m, 101, "coid-A", "e1", 4.0).as_slice(),
            [Event::OrderPartiallyFilled(_)]
        ));
        assert!(commission(&mut m, "e2").is_empty());
        assert!(matches!(
            exec(&mut m, 101, "coid-A", "e2", 6.0).as_slice(),
            [Event::OrderFilled(_)]
        ));
        // forgotten on the final fill, exactly as in the exec-first ordering
        assert!(exec(&mut m, 101, "", "e3", 1.0).is_empty());
    }

    #[test]
    fn duplicate_commission_does_not_double_emit_a_fill() {
        let mut m = mapper_submitted("coid-A", 101, 5.0);
        let _ = exec(&mut m, 101, "coid-A", "e1", 5.0);
        assert!(matches!(commission(&mut m, "e1").as_slice(), [Event::OrderFilled(_)]));
        // IB re-sends the same commissionReport. The fill for e1 already emitted, so the duplicate
        // is DROPPED at the top of `on_commission_report` — emitting nothing and, unlike before the
        // `emitted` index existed, not PARKING either (a park here would leak one `commissions`
        // entry per re-delivered commission, which a `reqExecutions` replay produces by the dozen).
        assert!(commission(&mut m, "e1").is_empty());
        assert!(commission(&mut m, "e1").is_empty());
        assert!(m.commissions.is_empty(), "a commission for an emitted fill must not park");
    }

    #[test]
    fn parked_commission_is_released_once_its_exec_proves_unroutable() {
        // The DEFINITIVE expiry: the execution arrives but resolves to no local order, so the pair
        // can never join and the park must not be held for the process lifetime.
        let mut m = mapper_submitted("coid-A", 101, 5.0);
        assert!(commission(&mut m, "ext-1").is_empty());
        assert_eq!(m.commissions.len(), 1);
        let evs = exec(&mut m, 999, "", "ext-1", 3.0); // order 999 was never bound
        assert!(evs.is_empty(), "an unroutable execution still emits nothing");
        assert!(m.commissions.is_empty(), "the park is released, not held forever");
        assert!(m.pending.is_empty());
    }

    #[test]
    fn parked_commissions_are_capped_and_evict_the_oldest_first() {
        // The BACKSTOP expiry: the key is a venue-supplied exec_id, so the map is bounded.
        let mut m = mapper_submitted("coid-A", 101, 1.0);
        for i in 0..MAX_PARKED_COMMISSIONS {
            assert!(commission(&mut m, &format!("orphan-{i}")).is_empty());
        }
        assert_eq!(m.commissions.len(), MAX_PARKED_COMMISSIONS);
        // One more park evicts the OLDEST, never the newest (which is the one still mid-race).
        assert!(commission(&mut m, "newest").is_empty());
        assert_eq!(m.commissions.len(), MAX_PARKED_COMMISSIONS, "the cap holds");
        assert!(!m.commissions.contains_key("orphan-0"), "the oldest park is the one evicted");
        assert!(m.commissions.contains_key("orphan-1"));
        assert!(m.commissions.contains_key("newest"), "the newest park survives");
        // The surviving park still joins normally — the cap does not break the live pair.
        assert!(matches!(
            exec(&mut m, 101, "coid-A", "newest", 1.0).as_slice(),
            [Event::OrderFilled(_)]
        ));
    }

    // -----------------------------------------------------------------------------------------
    // The MIRROR leak (#949's documented residual): an execution whose commissionReport never
    // arrives, on an order that is never cancelled, was held forever and its fill NEVER emitted —
    // position and realized PnL silently short. Cured by RECOVERING the real commission from the
    // venue (`sweep_pending` → the exec loop's `reqExecutions`), never by inventing one.
    // -----------------------------------------------------------------------------------------

    #[test]
    fn a_fresh_pending_entry_is_not_swept_before_the_grace_window() {
        let mut m = mapper_submitted("coid-A", 101, 10.0);
        let _ = exec(&mut m, 101, "coid-A", "e1", 10.0);
        // The first sweep only STAMPS the entry (the mapper has no clock, so it cannot have been
        // stamped at arrival) — it can never fire on the same pass that discovers it.
        assert!(m.sweep_pending(1_000, 15_000).is_empty());
        assert!(m.sweep_pending(1_000 + 14_999, 15_000).is_empty(), "one ms short of the window");
        assert_eq!(m.pending.len(), 1, "the sweep removes nothing");
    }

    #[test]
    fn an_ordinary_pair_that_joins_promptly_is_never_swept() {
        let mut m = mapper_submitted("coid-A", 101, 10.0);
        let _ = exec(&mut m, 101, "coid-A", "e1", 10.0);
        assert!(matches!(commission(&mut m, "e1").as_slice(), [Event::OrderFilled(_)]));
        // Nothing buffered ⇒ no re-request is ever provoked on the happy path, at any age.
        assert!(m.sweep_pending(i64::MAX / 2, 15_000).is_empty());
    }

    #[test]
    fn a_stranded_execution_is_reported_for_recovery_then_escalated_once() {
        let mut m = mapper_submitted("coid-A", 101, 10.0);
        let _ = exec(&mut m, 101, "coid-A", "e1", 10.0);
        let mut now = 0;
        m.sweep_pending(now, 15_000); // stamp
                                      // Each attempt restarts the entry's clock, so the ladder is one request per grace window.
        for attempt in 1..=MAX_PENDING_RECOVERY_ATTEMPTS {
            now += 15_000;
            let s = m.sweep_pending(now, 15_000);
            assert_eq!(s.recover, vec!["e1".to_string()], "attempt {attempt}");
            assert!(s.stranded.is_empty(), "attempt {attempt} must not escalate yet");
        }
        // Budget spent → ONE stranded row carrying everything an operator needs to see what the
        // platform is short.
        now += 15_000;
        let s = m.sweep_pending(now, 15_000);
        assert!(s.recover.is_empty(), "no fourth request");
        assert_eq!(
            s.stranded,
            vec![StrandedFill {
                exec_id: "e1".into(),
                client_order_id: "coid-A".into(),
                order_id: 101,
                symbol: "AAPL.SMART.USD".into(),
                side: 1,
                shares: 10.0,
                price: 190.0,
            }]
        );
        // …and never again: the escalation is reported once, not once per second forever.
        now += 15_000;
        assert!(m.sweep_pending(now, 15_000).is_empty());
    }

    #[test]
    fn a_stranded_execution_is_never_evicted_so_a_late_commission_still_emits_its_fill() {
        // The #949 constraint this design is built around: evicting a `pending` entry would lose
        // THE FILL, so the sweep annotates and never removes. Even past escalation the entry is
        // still joinable — and it joins with the REAL commission, never a fabricated 0.0.
        let mut m = mapper_submitted("coid-A", 101, 10.0);
        let _ = exec(&mut m, 101, "coid-A", "e1", 10.0);
        let mut now = 0;
        for _ in 0..(MAX_PENDING_RECOVERY_ATTEMPTS + 3) {
            m.sweep_pending(now, 15_000);
            now += 15_000;
        }
        assert_eq!(m.pending.len(), 1, "escalation holds the entry, it does not drop it");
        match m
            .on_commission_report(IbCommissionReport {
                exec_id: "e1".into(),
                commission: 1.75,
                currency: "USD".into(),
            })
            .as_slice()
        {
            [Event::OrderFilled(f)] => {
                assert_eq!(f.fill.commission, 1.75, "the REAL commission, not a fabricated 0.0");
                assert_eq!(f.fill.last_qty, 10.0);
            }
            other => panic!("expected exactly one OrderFilled, got {other:?}"),
        }
        assert!(m.pending.is_empty());
    }

    #[test]
    fn the_venue_replay_of_a_stranded_pair_emits_the_fill_with_the_real_commission() {
        // The whole recovery round trip, as the exec loop drives it: the commission is lost, the
        // sweep asks for it, IBKR re-delivers BOTH halves, and the ordinary join emits the fill.
        let mut m = mapper_submitted("coid-A", 101, 10.0);
        assert!(exec(&mut m, 101, "coid-A", "e1", 10.0).is_empty(), "no fill without a commission");
        m.sweep_pending(0, 15_000);
        assert_eq!(m.sweep_pending(15_000, 15_000).recover, vec!["e1".to_string()]);
        // `reqExecutions` replays the execution AND its commission report (in either order — the
        // join is by exec_id, never by arrival).
        assert!(exec(&mut m, 101, "coid-A", "e1", 10.0).is_empty(), "replayed exec re-buffers");
        match m
            .on_commission_report(IbCommissionReport {
                exec_id: "e1".into(),
                commission: 2.5,
                currency: "USD".into(),
            })
            .as_slice()
        {
            [Event::OrderFilled(f)] => {
                assert_eq!(f.fill.trade_id, "e1");
                assert_eq!(f.fill.commission, 2.5);
            }
            other => panic!("expected exactly one OrderFilled, got {other:?}"),
        }
        assert!(m.pending.is_empty(), "nothing strands after the recovery");
    }

    // -----------------------------------------------------------------------------------------
    // Replay SAFETY: `reqExecutions` returns the whole day indiscriminately, so every re-delivery
    // path has to be inert for a fill that already emitted. These are the tests that keep the
    // recovery from being worse than the disease.
    // -----------------------------------------------------------------------------------------

    #[test]
    fn replaying_an_emitted_partial_does_not_advance_fill_state_and_lose_later_executions() {
        // THE regression guard. Order of 10: e1=4 emits a partial, e2=3 strands. The replay
        // re-delivers e1 as well. If the replayed e1 re-entered `pending` and re-joined,
        // `fill_state.filled` would reach 4+4+3 = 11 ≥ 10, so e2 would be classified FINAL — the
        // order forgotten while 3 shares are still working, and every later execution silently
        // dropped. With the `emitted` index, e2 is correctly still a PARTIAL.
        let mut m = mapper_submitted("coid-A", 101, 10.0);
        let _ = exec(&mut m, 101, "coid-A", "e1", 4.0);
        assert!(matches!(commission(&mut m, "e1").as_slice(), [Event::OrderPartiallyFilled(_)]));
        let _ = exec(&mut m, 101, "coid-A", "e2", 3.0); // commission lost → stranded

        // The replay: both halves of the already-emitted e1 come back. Both are no-ops.
        assert!(exec(&mut m, 101, "coid-A", "e1", 4.0).is_empty(), "replayed exec must not buffer");
        assert!(commission(&mut m, "e1").is_empty(), "replayed commission must not emit");
        assert!(m.commissions.is_empty(), "…and must not park either");
        assert_eq!(m.pending.len(), 1, "only the genuinely stranded e2 is buffered");

        // e2's replayed commission now joins — and is still a PARTIAL (4+3 = 7 < 10).
        assert!(matches!(commission(&mut m, "e2").as_slice(), [Event::OrderPartiallyFilled(_)]));
        // Proof the order was NOT forgotten: a third execution still resolves and completes it.
        let _ = exec(&mut m, 101, "coid-A", "e3", 3.0);
        assert!(matches!(commission(&mut m, "e3").as_slice(), [Event::OrderFilled(_)]));
    }

    #[test]
    fn replaying_a_fully_emitted_pair_emits_nothing_in_either_arrival_order() {
        for commission_first in [false, true] {
            let mut m = mapper_submitted("coid-A", 101, 5.0);
            let _ = exec(&mut m, 101, "coid-A", "e1", 5.0);
            assert!(matches!(commission(&mut m, "e1").as_slice(), [Event::OrderFilled(_)]));
            if commission_first {
                assert!(commission(&mut m, "e1").is_empty());
                assert!(exec(&mut m, 101, "coid-A", "e1", 5.0).is_empty());
            } else {
                assert!(exec(&mut m, 101, "coid-A", "e1", 5.0).is_empty());
                assert!(commission(&mut m, "e1").is_empty());
            }
            assert!(m.pending.is_empty(), "commission_first={commission_first}");
            assert!(m.commissions.is_empty(), "commission_first={commission_first}");
        }
    }

    #[test]
    fn emitted_index_is_capped_and_evicts_the_oldest_first() {
        // Bounded like `commissions`, for the same venue-supplied-key reason — but eviction here
        // is safe: the worst case is a duplicate fill event, which `vike_exec::ExecutionEngine`
        // dedups by `trade_id`. Cap the map by driving that many complete fills.
        let mut m = mapper_with("coid-A", 101);
        for i in 0..MAX_EMITTED_EXEC_IDS {
            // No `on_submit` ⇒ unknown total ⇒ each fill is terminal and forgets the order, so
            // rebind before the next one.
            m.ids_mut().bind(101, "coid-A");
            let id = format!("x{i}");
            let _ = exec(&mut m, 101, "coid-A", &id, 1.0);
            assert!(matches!(commission(&mut m, &id).as_slice(), [Event::OrderFilled(_)]));
        }
        assert_eq!(m.emitted.len(), MAX_EMITTED_EXEC_IDS);
        m.ids_mut().bind(101, "coid-A");
        let _ = exec(&mut m, 101, "coid-A", "newest", 1.0);
        assert!(matches!(commission(&mut m, "newest").as_slice(), [Event::OrderFilled(_)]));
        assert_eq!(m.emitted.len(), MAX_EMITTED_EXEC_IDS, "the cap holds");
        assert!(!m.emitted.contains_key("x0"), "the oldest record is the one evicted");
        assert!(m.emitted.contains_key("x1"));
        assert!(m.emitted.contains_key("newest"));
    }

    #[test]
    fn a_cancel_flushed_partial_is_recorded_as_emitted_and_survives_a_replay() {
        // The `flushed` set #949 introduced folded into `emitted`. It kept its original job (the
        // late REAL commission for a cancel-flushed partial is a no-op) and gained non-consumption,
        // so a `reqExecutions` replay of that same pair cannot resurrect it either.
        let mut m = mapper_submitted("coid-A", 101, 10.0);
        let _ = exec(&mut m, 101, "coid-A", "e1", 4.0); // execDetails, NO commission yet
        let out = m.on_order_status(IbOrderStatus {
            order_id: 101,
            order_ref: "coid-A".into(),
            status: "Cancelled".into(),
            filled: 4.0,
            avg_fill_price: 190.0,
        });
        assert!(matches!(
            out.as_slice(),
            [Event::OrderPartiallyFilled(_), Event::OrderCanceled(_)]
        ));
        // Late real commission: still a no-op, still not parked (the #949 assertions).
        assert!(commission(&mut m, "e1").is_empty());
        assert!(m.commissions.is_empty());
        // NEW: the whole pair replayed — also inert, where a consuming set would have re-armed.
        assert!(exec(&mut m, 101, "coid-A", "e1", 4.0).is_empty());
        assert!(commission(&mut m, "e1").is_empty());
        assert!(m.pending.is_empty());
        assert!(m.commissions.is_empty());
    }
}
