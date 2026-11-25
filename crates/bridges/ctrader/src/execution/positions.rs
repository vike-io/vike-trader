//! Pure position-tracking + reduce-routing logic for the cTrader exec path. No I/O, no actor
//! state — just the decode of a wire `ProtoOAPosition` into a bridge-local [`PositionTracked`] and
//! the FIFO reduce planner [`plan_reduce`] that decides whether a submit REDUCES an open position
//! (route to `ProtoOAClosePositionReq`, per position id) or OPENS one (route to
//! `ProtoOANewOrderReq`, unchanged). Kept separate from `conn`/`exec` so the routing law is
//! fixture-testable without a socket. Ports nothing — cTrader Open API
//! (https://help.ctrader.com/open-api/).
//!
//! Volumes throughout are cTrader **centi-units** (1/100 of a base unit — `volume = units × 100`),
//! the SAME scale `feed::decode::order_to_new_order`/`instrument::symbols::VolumeGrid` use.

use std::collections::HashMap;

use vike_bridge_core::exit::ExitKind;
use vike_bridge_core::oldest_first::take_oldest_first;
use vike_model::events::Event;
use vike_model::orders::client_id::{OrderClientIdReading, parse_order_client_id};

use crate::execution::exit_legs::{ExitLegs, leg_order_client_id};
use crate::instrument::symbols::VolumeGrid;
use crate::transport::proto::{
    ProtoOaOrder, ProtoOaOrderStatus, ProtoOaOrderType, ProtoOaPosition, ProtoOaPositionStatus,
    ProtoOaTradeSide,
};

/// One open venue position the bridge tracks, keyed elsewhere by its numeric `positionId`. Learned
/// from `ProtoOAExecutionEvent.position` on every exec event (and re-seeded from a reconcile). On a
/// HEDGING account several same-symbol positions coexist (long AND short); on a NETTING account at
/// most one per symbol — the reduce planner is agnostic to which, since it works off the actual
/// tracked set.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PositionTracked {
    /// The symbol id the position is in (matched against a submit's resolved symbol id).
    pub symbol_id: i64,
    /// Signed direction: `+1` long (BUY), `-1` short (SELL).
    pub side: i32,
    /// Open volume magnitude in centi-units (always ≥ 0; the sign lives in [`Self::side`]).
    pub volume: i64,
    /// `tradeData.openTimestamp` (epoch ms) — the FIFO key (oldest closed first). Absent → `0`,
    /// which sorts oldest, so an unknown-age position is closed before dated ones (conservative:
    /// flatten the position we know least about first).
    pub open_timestamp: i64,
}

/// Decode a wire `ProtoOAPosition` into `(position_id, PositionTracked)` when it is a live OPEN
/// position, else `None`. A CLOSED/CREATED/ERROR-status position (or one with zero volume) is not
/// tracked — the caller REMOVES it from the map instead. `CREATED` is the empty placeholder cTrader
/// makes for a pending order; it holds no real exposure.
pub fn tracked_position(p: &ProtoOaPosition) -> Option<(i64, PositionTracked)> {
    if !is_open(p.position_status) || p.trade_data.volume <= 0 {
        return None;
    }
    let side = if p.trade_data.trade_side == ProtoOaTradeSide::Sell as i32 { -1 } else { 1 };
    Some((
        p.position_id,
        PositionTracked {
            symbol_id: p.trade_data.symbol_id,
            side,
            volume: p.trade_data.volume,
            open_timestamp: p.trade_data.open_timestamp.unwrap_or(0),
        },
    ))
}

/// Whether a wire `positionStatus` is the live OPEN state (tracked) vs. CLOSED/CREATED/ERROR
/// (untracked — the position carries no exposure this bridge should close against).
pub fn is_open(position_status: i32) -> bool {
    matches!(
        ProtoOaPositionStatus::try_from(position_status),
        Ok(ProtoOaPositionStatus::PositionStatusOpen)
    )
}

/// Fold a reconcile answer's `ProtoOAPosition` rows into the tracked OPEN set, **counting the rows
/// this build could not classify at all**.
///
/// ⚠ **The count is the whole point, and it is why this exists beside [`tracked_position`] rather
/// than as a `filter_map` over it.** [`tracked_position`] answers `Option`, which collapses two very
/// different rejections into one: *"the venue says this carries no exposure"* (CLOSED, or CREATED —
/// the empty placeholder for a pending order) and *"this build cannot tell whether it carries
/// exposure"* (an ERROR-status position, a `positionStatus` value the vendored proto has no variant
/// for, or an OPEN row whose `volume` is not positive). The first is knowledge. The second is a hole
/// — and a hole read as an absence is what turns `PositionsById::is_fetched` into a manufactured
/// `PositionEvidence::Flat`, i.e. a refused exit under `halt_admit = "verify"`. Callers give the
/// tracked rows to [`PositionsById::replace_all`] only when `unreadable == 0`, and to
/// [`PositionsById::replace_unverified`] otherwise.
///
/// The tracked half is byte-identical to the `filter_map` it replaces, so ROUTING is unchanged: this
/// only changes whether the resulting book is allowed to call itself evidence.
pub fn reconcile_rows(rows: &[ProtoOaPosition]) -> (Vec<(i64, PositionTracked)>, usize) {
    let (open, unreadable) = open_rows(rows);
    (open.into_iter().filter_map(tracked_position).collect(), unreadable)
}

/// **The ONE reading of a reconcile answer's `ProtoOAPosition` rows** (A15: the bridge used to read one row three
/// ways): the rows that carry exposure, exactly those [`tracked_position`] tracks (OPEN, positive volume), and the
/// count of rows this build could not classify ([`reconcile_rows`]' hole: neither that nor a CLOSED or CREATED row).
/// The exec book is seeded from it ([`reconcile_rows`]), and the reconcile client's position report and flatten
/// list read the same rows (`crate::reconcile`'s `position_reports` and `parse_raw_positions`), so a row is
/// exposure to all three or to none.
pub fn open_rows<'a>(
    rows: impl IntoIterator<Item = &'a ProtoOaPosition>,
) -> (Vec<&'a ProtoOaPosition>, usize) {
    let mut open = Vec::new();
    let mut unreadable = 0usize;
    for p in rows {
        if tracked_position(p).is_some() {
            open.push(p);
        } else if !row_is_known_to_carry_no_exposure(p) {
            unreadable += 1;
        }
    }
    (open, unreadable)
}

/// `true` when the venue TOLD us this row holds no exposure, as opposed to this build failing to
/// read it. CLOSED and CREATED are the two states that mean exactly that; every other rejection —
/// ERROR, an unknown status value, or an OPEN row with a non-positive volume — is a hole, and
/// [`reconcile_rows`] counts it.
///
/// ⚠ Public because the SAME split is owed on the live-event path, not only on the reconcile one.
/// `crates/bridges/ctrader/src/transport/connection/inbound.rs`'s `update_position_map` sees a `position` ref on every
/// execution event and, when [`tracked_position`] answers `None`, must decide the identical
/// question: a CLOSED position is knowledge (drop the entry, the book stays authoritative), an
/// ERROR one is a hole (drop the entry AND stop calling the book evidence). Without this the
/// completeness discipline stopped at the reconcile and the book silently decayed forwards.
pub fn row_is_known_to_carry_no_exposure(p: &ProtoOaPosition) -> bool {
    matches!(
        ProtoOaPositionStatus::try_from(p.position_status),
        Ok(ProtoOaPositionStatus::PositionStatusClosed
            | ProtoOaPositionStatus::PositionStatusCreated)
    )
}

/// The routing decision for one submit against the currently-tracked positions.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ClosePlan {
    /// Not a reduce — route through the normal `ProtoOANewOrderReq` path (opens/adds exposure, or
    /// flips a NETTING position; UNCHANGED behavior).
    Open,
    /// A reduce — close these positions FIFO (oldest first) via one `ProtoOAClosePositionReq` each,
    /// as `(position_id, volume_centi)` legs whose volumes sum to the reduce total.
    Close(Vec<(i64, i64)>),
}

/// Decide whether a submit REDUCES exposure (→ close-by-position-id, FIFO) or OPENS it (→ new
/// order). `order_side` is the submit's signed direction (`+1`/`-1`), `requested_centi` its volume
/// in centi-units, `reduce_only` its flag. `open` is every tracked position **already filtered to
/// the submit's symbol** (the caller filters by symbol id).
///
/// ⚠ **A `Close` is an IMMEDIATE market close, so the caller asks it for a MARKET submit only**
/// (`crates/bridges/ctrader/src/execution/mod.rs`'s `submit`). A resting stop or limit answered `Close`
/// would close the position at once instead of resting.
///
/// Rule (mode-agnostic — it reads the ACTUAL tracked positions, not the account's netting mode):
///   * The reducible positions are those OPPOSITE to `order_side` (a SELL closes LONGs, a BUY
///     closes SHORTs), taken FIFO by `open_timestamp` (tie-broken by `position_id` for determinism).
///   * `available` = their total volume. If it is `0` (no opposing exposure — a flat book or a
///     same-side add), the submit OPENS → [`ClosePlan::Open`].
///   * A `reduce_only` submit closes `min(requested, available)` — capped, never opening (excess is
///     dropped, standard reduce-only semantics).
///   * A plain submit with `requested <= available` closes exactly `requested` (a pure
///     reduce/flatten). THIS is the hedging-flatten fix: an opposite market order now nets the
///     position flat instead of stacking a hedge.
///   * A plain submit with `requested > available` is a FLIP (reduce past flat into the other
///     direction). To PRESERVE netting-account flip behavior — and to keep one core order mapped to
///     one coherent venue lifecycle (never a mixed close+open under one order client id) — the WHOLE submit
///     routes as a normal `ProtoOANewOrderReq` ([`ClosePlan::Open`], unchanged). On a netting
///     account the venue flips correctly; a flip on a hedging account remains the operator's manual
///     concern (the reported bug was flatten/reduce, which IS fixed).
pub fn plan_reduce(
    order_side: i32,
    requested_centi: i64,
    reduce_only: bool,
    open: &[(i64, PositionTracked)],
) -> ClosePlan {
    if order_side == 0 || requested_centi <= 0 {
        return ClosePlan::Open; // a zero-side / zero-volume submit is the new-order path's reject
    }
    // The positions facing the opposite way, FIFO, each for min(remaining, its volume), capped at what they hold.
    let legs: Vec<(i64, i64)> = take_oldest_first(
        open,
        |(_, p)| p.side == -order_side && p.volume > 0,
        opened_before,
        |(_, p)| p.volume,
        requested_centi,
    )
    .into_iter()
    .map(|(&(position_id, _), take)| (position_id, take))
    .collect();
    if legs.is_empty() {
        return ClosePlan::Open; // no opposing exposure
    }
    // Less than asked faces the order: a plain submit is a FLIP → the normal new-order path (preserve netting
    // behavior); a reduce_only one closes what is there.
    let taken: i64 = legs.iter().map(|&(_, take)| take).sum();
    if !reduce_only && taken < requested_centi {
        return ClosePlan::Open;
    }
    ClosePlan::Close(legs)
}

/// FIFO by the venue's open time, a deterministic tie-break on the position id: the order a reduce
/// ([`plan_reduce`]) and a bound exit ([`PositionsById::bind_exit_among`], [`PositionsById::split_exit_among`]) take
/// positions in.
fn opened_before(a: &(i64, PositionTracked), b: &(i64, PositionTracked)) -> std::cmp::Ordering {
    a.1.open_timestamp.cmp(&b.1.open_timestamp).then(a.0.cmp(&b.0))
}

/// Snap a partial-close centi-volume to the symbol's `step_volume` grid (nearest step, at least one
/// step), capping at `position_volume`. A full-position close (`raw >= position_volume`) uses the
/// exact position volume — always a valid venue quantity. `step <= 0` (unknown grid) passes `raw`
/// through unrounded. Never returns `0` for a positive request (a sub-step reduce still closes one
/// step, so the intent is honored rather than silently dropped).
pub fn snap_close_volume(raw: i64, position_volume: i64, step: i64) -> i64 {
    if raw >= position_volume {
        return position_volume;
    }
    if step <= 0 {
        return raw.min(position_volume);
    }
    let steps = ((raw as f64) / step as f64).round() as i64;
    let snapped = (steps.max(1)) * step;
    snapped.min(position_volume)
}

/// Per-order-client-id aggregation state for in-flight closes, so the actor can present N FIFO
/// `ProtoOAClosePositionReq` legs (potentially across several positions) as ONE coherent core order
/// lifecycle: `OrderAccepted` once, then `OrderPartiallyFilled` per leg until the LAST fill, which
/// becomes `OrderFilled`. Keyed off the position id each close event references (`deal.positionId`/
/// `order.positionId`), since the closing order carries no `clientOrderId` of ours.
#[derive(Debug, Default)]
pub struct CloseTracker {
    /// positionId → the order client id of the submit that is closing it.
    by_position: HashMap<i64, String>,
    /// order client id → remaining centi-volume still to fill before the order is terminal.
    remaining: HashMap<String, i64>,
    /// order client id → cumulative centi-volume actually filled so far — the "partial progress" flag a
    /// failure (leg reject / wire error / write failure) reads to pick a LEGAL terminal: an order client id that
    /// has filled some volume is at FSM `PartiallyFilled`, from which `OrderRejected` is illegal, so
    /// it must terminalize as `OrderCanceled` instead (see [`Self::resolve_failure`]).
    filled: HashMap<String, i64>,
    /// order client ids that have already emitted their single `OrderAccepted`.
    accepted: std::collections::HashSet<String>,
    /// The closing deals whose realized swap was already booked, oldest first (A26): a funding event has
    /// no id and `Account::apply_funding` is not idempotent, so the bridge remembers the deal ids
    /// ([`Self::first_swap_of_deal`]). Bounded by [`SWAP_BOOKED_CAP`], the oldest forgotten first (the one "ids seen
    /// lately" set, `vike_utility::recent_set::RecentSet`, keyed by the deal id's digits).
    swap_booked: vike_utility::recent_set::RecentSet,
    /// The order client ids with a `ClosePosition` leg WRITTEN to the current socket and not yet terminal, with when the first
    /// leg went: what a socket death strands ([`Self::take_written`]), because the venue's answer went with the socket.
    written: HashMap<String, i64>,
}

/// How many closing deals [`CloseTracker::first_swap_of_deal`] remembers. A closing deal with a swap is a
/// position held overnight, so this is a leak guard; the oldest deal leaves first.
const SWAP_BOOKED_CAP: usize = 4096;

/// A close with a leg on a socket that died before the venue answered it ([`CloseTracker::take_written`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StrandedClose {
    pub order_client_id: String,
    /// The positions its legs were closing.
    pub positions: Vec<i64>,
    /// Whether an earlier leg's fill was heard: the FSM is then at `PartiallyFilled`, which a reject cannot end
    /// (its terminal is `OrderCanceled`, [`CloseFailure::Canceled`]).
    pub progressed: bool,
    /// When its first leg was written (epoch ms).
    pub written_ms: i64,
}

/// How a close order client id must be terminalized when a leg FAILS (venue reject, wire `ERROR_RES`, or a
/// write failure), so the order always reaches a LEGAL terminal state — never a dropped one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CloseFailure {
    /// Earlier legs already FILLED (FSM at `PartiallyFilled`): terminalize as `OrderCanceled`, the
    /// close ending with what its legs filled, as FXCM's partial close ends (owner ruling 3,
    /// 2026-11-11). An `OrderRejected` from `PartiallyFilled` is an illegal transition and would be
    /// DROPPED, stranding the order client id. The already-filled money/position is correct (folded via the legs'
    /// bare `Event::Fill`); this is purely the order's terminal.
    Canceled,
    /// Nothing filled yet (still `Submitted`/`Accepted`): a clean `OrderRejected`.
    Rejected,
}

/// What the actor should emit for one close-correlated execution event, decided by [`CloseTracker`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CloseEmit {
    /// Emit the single `OrderAccepted` for this order client id (first close event seen), and nothing else.
    AcceptOnly,
    /// A non-final fill: emit `OrderPartiallyFilled` (also `OrderAccepted` first if not yet sent).
    Partial { accept_first: bool },
    /// The final fill (cumulative reduce satisfied): emit `OrderFilled` (also `OrderAccepted` first
    /// if not yet sent), then the tracker forgets this order client id.
    Final { accept_first: bool },
}

impl CloseTracker {
    /// Register a planned close: `order_client_id` will close `legs` (`(position_id, volume_centi)`), whose
    /// volumes sum to the reduce total the resulting fills must reach before the order is terminal.
    pub fn register(&mut self, order_client_id: &str, legs: &[(i64, i64)]) {
        let total: i64 = legs.iter().fold(0i64, |a, (_, v)| a.saturating_add(*v));
        self.remaining.insert(order_client_id.to_string(), total);
        for (position_id, _) in legs {
            self.by_position.insert(*position_id, order_client_id.to_string());
        }
    }

    /// `true` the FIRST time `deal_id` is asked about, `false` for every repeat while it is remembered: the
    /// guard that books a closing deal's realized swap once (`crate::feed::decode::deal_swap_funding`).
    pub fn first_swap_of_deal(&mut self, deal_id: i64) -> bool {
        let id = deal_id.to_string();
        if self.swap_booked.contains(&id) {
            return false;
        }
        self.swap_booked.insert(&id, SWAP_BOOKED_CAP);
        true
    }

    /// The order client id closing `position_id`, if a close is in flight for it.
    pub fn order_client_id_for(&self, position_id: i64) -> Option<&str> {
        self.by_position.get(&position_id).map(String::as_str)
    }

    /// Whether a close is IN FLIGHT for `position_id` — the routing exclusion (a new reduce must
    /// NOT plan/register against a position already being closed under another order client id, or it would
    /// OVERWRITE that order client id's `by_position` entry and strand it).
    pub fn is_closing(&self, position_id: i64) -> bool {
        self.by_position.contains_key(&position_id)
    }

    /// Whether `order_client_id` is a close order client id this tracker still owns (registered and not yet
    /// forgotten) — so a wire-level failure (`on_error_res`, write failure) can tell a close order client id
    /// from an ordinary order and route it through [`Self::resolve_failure`].
    pub fn is_tracked(&self, order_client_id: &str) -> bool {
        self.remaining.contains_key(order_client_id) || self.accepted.contains(order_client_id)
    }

    /// Fold one close-correlated execution event for `order_client_id`. `fill_volume` is the deal's filled
    /// centi-volume (`0` for an accept-only event with no deal). Returns the [`CloseEmit`] decision
    /// and, on `Final`, prunes the order client id (and its position entries) from the tracker.
    pub fn on_event(&mut self, order_client_id: &str, fill_volume: i64) -> CloseEmit {
        let accept_first = self.accepted.insert(order_client_id.to_string()); // true iff newly inserted
        if fill_volume <= 0 {
            // No deal on this event (e.g. a bare ORDER_ACCEPTED for the closing order).
            return if accept_first {
                CloseEmit::AcceptOnly
            } else {
                CloseEmit::Partial { accept_first: false }
            };
        }
        let filled = self.filled.entry(order_client_id.to_string()).or_insert(0);
        *filled = filled.saturating_add(fill_volume);
        let remaining = self.remaining.entry(order_client_id.to_string()).or_insert(0);
        *remaining = remaining.saturating_sub(fill_volume);
        if *remaining <= 0 {
            self.forget(order_client_id);
            CloseEmit::Final { accept_first }
        } else {
            CloseEmit::Partial { accept_first }
        }
    }

    /// Whether `order_client_id` has already filled some volume — the NON-mutating read behind the
    /// terminal-choice a failure makes. Exposed so a CAPTURE-time failure event (see
    /// `transport::connection::write_failure_event`, built BEFORE the write is even attempted) can pick
    /// Filled-vs-Rejected WITHOUT forgetting the order client id — the forget happens only if the write
    /// actually fails.
    pub fn has_progress(&self, order_client_id: &str) -> bool {
        self.filled.get(order_client_id).copied().unwrap_or(0) > 0
    }

    /// Terminalize `order_client_id` on a FAILURE (leg reject / wire error / write failure) and FORGET it,
    /// picking the LEGAL terminal by whether any volume already filled: [`CloseFailure::Canceled`]
    /// if so (`OrderRejected` would be an illegal drop from `PartiallyFilled`), else
    /// [`CloseFailure::Rejected`]. Idempotent — an order client id the tracker no longer owns resolves as
    /// `Rejected` (nothing filled under our watch).
    pub fn resolve_failure(&mut self, order_client_id: &str) -> CloseFailure {
        let progressed = self.filled.get(order_client_id).copied().unwrap_or(0) > 0;
        self.forget(order_client_id);
        if progressed { CloseFailure::Canceled } else { CloseFailure::Rejected }
    }

    /// Drop all state for `order_client_id` (its remaining + filled counters, its accepted flag, every position
    /// entry pointing at it). Called on the final fill, and by [`Self::resolve_failure`].
    pub fn forget(&mut self, order_client_id: &str) {
        self.remaining.remove(order_client_id);
        self.filled.remove(order_client_id);
        self.accepted.remove(order_client_id);
        self.written.remove(order_client_id);
        self.by_position.retain(|_, c| c != order_client_id);
    }

    /// A `ClosePosition` leg of `order_client_id` reached the socket at `now_ms`. An order client id already forgotten stays forgotten.
    pub fn mark_written(&mut self, order_client_id: &str, now_ms: i64) {
        if self.remaining.contains_key(order_client_id) {
            self.written.entry(order_client_id.to_string()).or_insert(now_ms);
        }
    }

    /// **The socket died: every close with a leg on it is forgotten, and returned.** Its answer went with the socket
    /// and none will come on a new one, so left here its position stayed [`Self::is_closing`] for the life of the
    /// process: every later market reduce refused "already closing in flight", `close_all` skipping it in silence. A
    /// POSITION THAT COULD NOT BE CLOSED, until a gatewayhub restart. A close still queued, not written, is kept: the
    /// reconnect's replay writes it.
    pub fn take_written(&mut self) -> Vec<StrandedClose> {
        let order_client_ids = std::mem::take(&mut self.written);
        order_client_ids
            .into_iter()
            .map(|(order_client_id, written_ms)| {
                let positions = self
                    .by_position
                    .iter()
                    .filter(|(_, c)| **c == order_client_id)
                    .map(|(id, _)| *id);
                let stranded = StrandedClose {
                    positions: positions.collect(),
                    progressed: self.has_progress(&order_client_id),
                    written_ms,
                    order_client_id,
                };
                self.forget(&stranded.order_client_id);
                stranded
            })
            .collect()
    }
}

/// The account's OPEN positions **and whether the venue has ever told us what they are**.
///
/// ⚠ This type exists because a bare `HashMap` cannot distinguish the two states that matter most:
/// an empty map is the SAME value for *"we have never asked"* and *"we asked, and you are flat"*.
/// That ambiguity is not academic — it already cost this crate a feature. [`plan_reduce`] against a
/// never-populated map sees no opposing exposure and OPENS a hedge, which is exactly why
/// `crates/bridges/ctrader/src/execution/mod.rs`'s `close_all` had to exist ("12 stacked hedged positions a
/// fresh connect doesn't know"). And it is the trap a position-VERIFIED halt admit walks into: read
/// the empty map as "flat", and an operator who restarts the process cannot close anything, in the
/// one situation where being able to close is the whole point.
///
/// So [`Self::fetched_at_ms`] is the third state, and it is `None` until an authoritative
/// `ProtoOAReconcileReq` answer has been folded in. It goes back to `None` the moment the socket
/// drops ([`Self::invalidate`]): the entries stay (they are still the best routing guess, so the
/// reduce planner is unchanged) but they stop being EVIDENCE, because a position opened or closed
/// during a dead window is invisible to us.
///
/// ## Why freshness is CONTINUITY, not wall-clock age
///
/// There is deliberately no "evidence older than N seconds is stale" rule. cTrader pushes
/// `EXECUTION_EVENT`s for the account UNCONDITIONALLY once authorized — not subscription-gated, and
/// not only for orders we placed — so a socket that has been up continuously since its fetch has a
/// book that has been maintained continuously, whatever the clock says. A wall-clock bound would
/// therefore disarm the verification on a HEALTHY long-lived session (FX runs Sunday to Friday
/// without a reconnect) while adding nothing on an unhealthy one, where the disconnect already
/// invalidates.
///
/// ## ⚠ Evidence is COVERAGE, not decodability — [`Self::unauthoritative_for`] is the rule
///
/// A refusal under `halt_admit = "verify"` rests on this book answering *"there is nothing here to
/// reduce"*, and that answer is only ever manufactured from an ABSENCE — `opposing_available`
/// summing an empty slice to `0`. So the question that decides a refusal is not *"was some past
/// answer decodable"*, it is **"did the venue positively tell us about this symbol at all"**.
///
/// [`Self::unauthoritative_for`] demands BOTH halves, and they answer different questions:
///
/// * **Provenance** — [`Self::fetched_at_ms`] is `Some`: an authoritative `RECONCILE_RES`, for THIS
///   account, every row of which this build could CLASSIFY ([`reconcile_rows`] does that counting;
///   [`Self::replace_unverified`] is the arm for an answer that failed it), and no socket death
///   since ([`Self::invalidate`]).
/// * **Coverage** — the book holds at least one tracked position IN THE SYMBOL being asked about.
///   Without this the book claims to speak for instruments nothing ever mentioned to it.
///
/// ⚠ **The provenance half ALONE was a live trap, and it was measured, not theorised.** A
/// `RECONCILE_RES` that simply OMITS a position the venue is holding is well-formed and fully
/// classifiable — a row that was never SENT is uncountable, so `unreadable == 0`, `replace_all`
/// marked the book authoritative, and a `reduce_only` exit from that very position was REFUSED
/// under an engaged halt (`crates/bridges/ctrader/tests/offline/execution_halt.rs`'s
/// `verify_admits_the_exit_from_a_position_the_reconcile_answer_omitted` measured exactly that:
/// `["Submitted", "Rejected"]`, nothing on the wire). Counting undecodable rows can never catch an
/// absent one; only requiring POSITIVE evidence can.
///
/// ⚠ **What that costs, stated rather than hidden.** A genuinely FLAT account legitimately reports
/// no rows, so under this rule an all-flat book has coverage of nothing and `verify` ADMITS a
/// mis-tagged `reduce_only` entry there — the same answer every other venue gives. `verify` stops
/// the SECOND such order in a symbol, not the first: once the venue has positively reported a
/// position, an order claiming to reduce the side that has none is refused, and that is the
/// exposure-ADDING shape a kill switch most needs to stop. Refusing the first one as well would
/// mean reading an absence as a fact, which is precisely the trap above.
#[derive(Debug, Default)]
pub struct PositionsById {
    /// positionId → the live OPEN position.
    open: HashMap<i64, PositionTracked>,
    /// When an authoritative reconcile last REPLACED this set wholesale (epoch ms), or `None` for
    /// "never fetched / no longer trustworthy". See the type doc.
    fetched_at_ms: Option<i64>,
    /// order client id → the resting exit that order client id is BOUND to a position with (HEDGED accounts only; see
    /// [`BoundExit`]). Empty on a netted account and until the first bound exit.
    bound: HashMap<String, BoundExit>,
    /// The resting exits the connect-time look found bound to a position that is NOT open any more
    /// ([`OrphanedExit`]), until each ends. Informational: nothing here cancels anything.
    orphaned_at_connect: Vec<OrphanedExit>,
    /// The exits that SPAN positions and rest as one bound order per position (`crate::execution::exit_legs`): each leg is a
    /// row of `bound` under its own order client id, and this table folds their events into the one exit the engine placed.
    exit_legs: ExitLegs,
    /// order client id → when its `NewOrder` was written, until an event ends the order ([`Self::note_order_ended`]): what a
    /// reconnect asks the venue about when its pending list does not hold it ([`Self::take_written_orders_not_in`]).
    written_orders: HashMap<String, i64>,
    /// The order client ids this bridge KNOWS as ours though they do not read as a v2 id: placed through this bridge, or taught
    /// to it by a restore ([`Self::know_order_client_id`], [`Self::knows_order_client_id`]). Oldest first, at most [`KNOWN_ORDER_CLIENT_IDS_CAP`].
    known: vike_utility::recent_set::RecentSet,
    /// [`Self::is_settling`].
    settling: bool,
}

/// How many order client ids [`PositionsById::know_order_client_id`] remembers. Only ids that do not read as v2 are kept (a v2 id is ours by
/// its spelling), so this bounds an operator's own ids and those minted before v2, not a strategy's order rate.
/// shortcut: a count, oldest forgotten first, so a deal of an id older than the last 16,384 such reads foreign; upgrade
/// to a bound tied to the reconcile's fill lookback if v1 traffic ever outgrows it.
pub const KNOWN_ORDER_CLIENT_IDS_CAP: usize = 16_384;

/// Whether `id` reads as one of our v2 ids (`vike_model`'s `parse_order_client_id`).
pub(crate) fn is_v2_order_client_id(id: &str) -> bool {
    matches!(parse_order_client_id(id), Some(OrderClientIdReading::V2(_)))
}

/// One leg of an exit that spans positions: the order it rests as, the position it is bound to and its volume.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExitLeg {
    /// The leg's own order client id (`crate::execution::exit_legs::leg_order_client_id`).
    pub order_client_id: String,
    pub position_id: i64,
    /// Centi-units, a multiple of the symbol's step and no more than the room the position had.
    pub volume: i64,
}

/// A resting STOP or LIMIT that is bound to a position the venue no longer holds open, found at connect
/// (`crates/bridges/ctrader/src/transport/connection/reconcile.rs`'s `flag_orphaned_exits`): the position closed while the bridge was
/// down (or in the cTrader UI), and the venue does NOT cancel a bound order when its position closes another way.
/// ⚠ **If the market reaches it, it OPENS a new position under the reused position id** (measured on the hedged
/// demo account, 2026-10-26). The bridge NEVER cancels it by itself (an order it did not see placed or closed is
/// left to its owner: the operator cancels it by hand, `recon cancel` on a demo account or the cTrader UI); it
/// SAYS so, loudly, once at connect, and keeps the list here for as long as the order rests.
#[derive(Debug, Clone, PartialEq)]
pub struct OrphanedExit {
    /// The venue's numeric order id: what a cancel by hand names.
    pub order_id: i64,
    /// The order client id the order carries (`clientOrderId`, else the `label`), when it has one.
    pub order_client_id: Option<String>,
    /// The position the order names and the venue no longer holds open.
    pub position_id: i64,
    pub symbol_id: i64,
    /// `+1` a BUY, `-1` a SELL.
    pub side: i32,
    pub kind: ExitKind,
    /// The order's volume in centi-units.
    pub volume: i64,
    /// Its stop or limit price, as the venue states it.
    pub price: Option<f64>,
}

impl OrphanedExit {
    /// The exit `order` is, when it is a resting STOP, STOP_LIMIT or LIMIT that names a position; `None` for any
    /// other order. A stop-limit is a STOP here, at its stop price, as it binds (`ExecutionClientCtrader::submit`).
    pub fn of_order(order: &ProtoOaOrder) -> Option<Self> {
        let position_id = order.position_id?;
        let (kind, price) = match ProtoOaOrderType::try_from(order.order_type) {
            Ok(ProtoOaOrderType::Stop | ProtoOaOrderType::StopLimit) => {
                (ExitKind::Stop, order.stop_price)
            }
            Ok(ProtoOaOrderType::Limit) => (ExitKind::Limit, order.limit_price),
            _ => return None,
        };
        Some(OrphanedExit {
            order_id: order.order_id,
            order_client_id: order
                .client_order_id
                .clone()
                .or_else(|| order.trade_data.label.clone())
                .filter(|c| !c.is_empty()),
            position_id,
            symbol_id: order.trade_data.symbol_id,
            side: if order.trade_data.trade_side == ProtoOaTradeSide::Sell as i32 { -1 } else { 1 },
            kind,
            volume: order.trade_data.volume,
            price,
        })
    }
}

/// The pending STOPs, STOP_LIMITs and LIMITs of a reconcile answer that name a position the answer does NOT list as open:
/// either a PLAIN pending order (the venue gives it a new, empty position of its own) or a bound exit that outlived
/// its position, and the answer cannot say which. Only for a book that was read IN FULL (`book.is_fetched()`): an
/// unreadable row could be the position a "missing" one is really bound to. An order the bridge already holds a
/// bound-exit row for is not a candidate (it was adopted on an open position).
pub fn orphan_candidates<'a>(
    orders: &'a [ProtoOaOrder],
    book: &PositionsById,
) -> Vec<&'a ProtoOaOrder> {
    if !book.is_fetched() {
        return Vec::new();
    }
    orders
        .iter()
        .filter(|o| {
            OrphanedExit::of_order(o).is_some_and(|exit| {
                book.position(exit.position_id).is_none()
                    && !exit
                        .order_client_id
                        .as_deref()
                        .is_some_and(|c| book.bound_exit(c).is_some())
            })
        })
        .collect()
}

/// Whether the orders the venue lists for ONE position (`ProtoOAOrderListByPositionIdRes`) PROVE that the
/// position existed and is gone: an order of it OTHER than `pending_order_id` that FILLED. A plain pending order's
/// empty position never had a fill, so its list is the pending order alone; a bound exit's position was opened by
/// a filled entry and closed by a filled closing order. `false` is "not proven", never "proven plain".
pub fn proves_a_closed_position(listed: &[ProtoOaOrder], pending_order_id: i64) -> bool {
    listed.iter().any(|o| {
        o.order_id != pending_order_id
            && matches!(
                ProtoOaOrderStatus::try_from(o.order_status),
                Ok(ProtoOaOrderStatus::OrderStatusFilled)
            )
    })
}

/// One resting exit bound to an OPEN position (`ProtoOaNewOrderReq::position_id`).
///
/// ⚠ **Why the bridge keeps a table of these at all.** The venue does NOT cancel a bound order when its
/// position closes another way, and a stale bound stop that reaches the market OPENS a new position
/// under the reused position id (measured on the hedged demo account, 2026-10-26). The table is what
/// lets the bridge cancel them itself: `orphaned` is set when the position is seen gone, and the actor
/// writes the cancel (`cancel_sent` keeps the engine's own OCO-sibling cancel from racing it).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BoundExit {
    /// The position the exit is bound to.
    pub position_id: i64,
    pub kind: ExitKind,
    /// The exit's volume in centi-units (already capped to the room the position had).
    pub volume: i64,
    /// The volume an AMEND of this exit has asked for and the venue has not yet confirmed (`ORDER_REPLACED`) or
    /// refused: the cover count follows the REQUEST, not the confirmation, and counts the larger of the two sizes
    /// while the amend is in flight, so another exit binding in that instant cannot see room the amend is about to
    /// take.
    pub asked_volume: Option<i64>,
    /// The position is gone (or was never there): the exit must be cancelled.
    pub orphaned: bool,
    /// The bridge has written the cancel for an orphaned exit; the venue's `OrderCancelled` ends it.
    pub cancel_sent: bool,
    /// A cancel of this exit (the engine's) is on its way: it no longer counts toward its position's
    /// cover, so a stop cancelled and at once re-armed (a trailing stop) binds. A refused cancel puts it back.
    pub cancelling: bool,
    /// The order was WRITTEN to the venue (the actor's `NewOrder` write succeeded, or the reconcile found it
    /// resting). A row that is not `sent` is still queued or in flight inside this process and is left alone
    /// by a reconnect; one that is `sent` and not pending in the reconcile answer after a reconnect is gone
    /// (never accepted, filled, cancelled) and loses its row.
    pub sent: bool,
}

/// Why no position could take a `reduce_only` exit.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExitRefusal {
    /// No free position faces the order (none open, or all of them closing in flight).
    NothingToBind,
    /// Every free opposing position already has exits of this kind covering its whole volume.
    Covered,
    /// The exit is larger than the room any ONE position has, and more than one position has room: one
    /// exit binds to one position, so it would leave the others uncovered while the engine believes it
    /// covers them all.
    Spans,
}

impl PositionsById {
    /// Learn (or refresh) one position from an execution event.
    pub fn upsert(&mut self, position_id: i64, tracked: PositionTracked) {
        self.open.insert(position_id, tracked);
    }

    /// Forget a position the venue reports CLOSED / zero-volume.
    pub fn remove(&mut self, position_id: i64) {
        self.open.remove(&position_id);
    }

    /// The tracked OPEN position `position_id`, if any.
    pub fn position(&self, position_id: i64) -> Option<PositionTracked> {
        self.open.get(&position_id).copied()
    }

    /// Record `order_client_id` as an exit bound to `position_id` (this client placed it, or a reconcile found it
    /// resting on that open position). Replaces an earlier row of the same order client id.
    pub fn bind_exit(
        &mut self,
        order_client_id: &str,
        position_id: i64,
        kind: ExitKind,
        volume: i64,
    ) {
        self.bound.insert(
            order_client_id.to_string(),
            BoundExit {
                position_id,
                kind,
                volume,
                asked_volume: None,
                orphaned: false,
                cancel_sent: false,
                cancelling: false,
                sent: false,
            },
        );
    }

    /// Drop a bound exit's row (it reached a terminal state, or never reached the venue).
    pub fn unbind_exit(&mut self, order_client_id: &str) {
        self.bound.remove(order_client_id);
    }

    /// The bound-exit row of `order_client_id`, if it is one.
    pub fn bound_exit(&self, order_client_id: &str) -> Option<BoundExit> {
        self.bound.get(order_client_id).copied()
    }

    /// **Choose the position a new `reduce_only` resting exit binds to, and record the binding.** `free`
    /// is the symbol's open positions NOT already closing in flight. The rule: the OLDEST (FIFO, like
    /// [`plan_reduce`]) position facing the order (a SELL faces longs) whose ROOM, its volume less the live
    /// exits OF THE SAME KIND already bound to it, holds the whole exit. So a bracket's stop and take-profit
    /// (different kinds) both bind the SAME position, a second independent stop goes to the next position
    /// or is refused ([`ExitRefusal::Covered`]), and two exits of one kind can never exceed the position
    /// together. When NO position's room holds the whole exit: with exactly one position having any room
    /// the exit is capped to it (`reduce_only` never closes more than the position holds), with more than
    /// one it is refused ([`ExitRefusal::Spans`]) rather than silently covering one of them. Returns
    /// `(position_id, volume_centi)`.
    pub fn bind_exit_among(
        &mut self,
        order_client_id: &str,
        order_side: i32,
        kind: ExitKind,
        requested_centi: i64,
        free: &[(i64, PositionTracked)],
    ) -> Result<(i64, i64), ExitRefusal> {
        let mut facing: Vec<(i64, PositionTracked)> = free
            .iter()
            .filter(|(_, p)| order_side != 0 && p.side == -order_side && p.volume > 0)
            .copied()
            .collect();
        if facing.is_empty() || requested_centi <= 0 {
            return Err(ExitRefusal::NothingToBind);
        }
        facing.sort_by(opened_before);
        // The room of each position that still has any, oldest first. The snapshot may be a moment
        // old: judge against the book itself.
        let rooms: Vec<(i64, i64)> = facing
            .iter()
            .filter_map(|&(position_id, _)| {
                let room = self.room_of(position_id, kind, None)?;
                (room > 0).then_some((position_id, room))
            })
            .collect();
        let (position_id, volume) = match rooms.iter().find(|&&(_, room)| room >= requested_centi) {
            Some(&(position_id, _)) => (position_id, requested_centi),
            None => match rooms[..] {
                [] => return Err(ExitRefusal::Covered),
                [(position_id, room)] => (position_id, room),
                _ => return Err(ExitRefusal::Spans),
            },
        };
        self.bind_exit(order_client_id, position_id, kind, volume);
        Ok((position_id, volume))
    }

    /// **Plan and bind an exit that SPANS positions**: the legs of `parent` over the positions facing the order,
    /// oldest first, each taking the lesser of the room that position has left for `kind` and what the exit still
    /// needs, snapped DOWN to the symbol's step and kept only when it reaches the minimum volume (a leg is an order
    /// of its own). Called only after [`Self::bind_exit_among`] answered [`ExitRefusal::Spans`] (more than one position has
    /// room and none holds the whole exit), so a plan of fewer than two legs is the same refusal: one exit still
    /// cannot be bound to a position that cannot hold it. When the rooms together are smaller than the exit the legs
    /// cover what the rooms allow (`reduce_only` never closes more than the positions hold: the caller says so).
    /// Each leg becomes a bound-exit row under its own order client id and the group is registered so the legs' events fold into
    /// `parent`'s ([`crate::execution::exit_legs`]).
    pub fn split_exit_among(
        &mut self,
        parent: &str,
        order_side: i32,
        kind: ExitKind,
        requested_centi: i64,
        free: &[(i64, PositionTracked)],
        grid: VolumeGrid,
    ) -> Result<Vec<ExitLeg>, ExitRefusal> {
        let mut facing: Vec<(i64, PositionTracked)> = free
            .iter()
            .filter(|(_, p)| order_side != 0 && p.side == -order_side && p.volume > 0)
            .copied()
            .collect();
        if facing.is_empty() || requested_centi <= 0 {
            return Err(ExitRefusal::NothingToBind);
        }
        facing.sort_by(opened_before);
        let mut remaining = requested_centi;
        let mut plan: Vec<(i64, i64)> = Vec::new();
        for &(position_id, _) in &facing {
            if remaining <= 0 {
                break;
            }
            let mut take = self.room_of(position_id, kind, None).unwrap_or(0).min(remaining);
            if grid.step_volume > 0 {
                take -= take % grid.step_volume;
            }
            if take <= 0 || take < grid.min_volume {
                continue;
            }
            plan.push((position_id, take));
            remaining -= take;
        }
        if plan.len() < 2 {
            return Err(ExitRefusal::Spans);
        }
        let legs: Vec<ExitLeg> = plan
            .into_iter()
            .enumerate()
            .map(|(n, (position_id, volume))| ExitLeg {
                order_client_id: leg_order_client_id(parent, n + 1),
                position_id,
                volume,
            })
            .collect();
        for leg in &legs {
            self.bind_exit(&leg.order_client_id, leg.position_id, kind, leg.volume);
        }
        let order_client_ids: Vec<(String, i64)> =
            legs.iter().map(|l| (l.order_client_id.clone(), l.volume)).collect();
        self.exit_legs.register(parent, &order_client_ids);
        Ok(legs)
    }

    /// Forget a split exit whose submit never reached the venue: its group and every leg's row.
    pub fn forget_split_exit(&mut self, parent: &str) {
        for leg in self.exit_legs.forget(parent) {
            self.bound.remove(&leg);
        }
    }

    /// A reconnect dropped the rows of the orders that no longer rest ([`Self::unbind_exit`]): the legs of split
    /// exits that have no row any more are gone, no event will come for them, and they stop holding their exit open.
    pub fn retire_exit_legs_without_rows(&mut self) {
        let bound = &self.bound;
        self.exit_legs.retire_legs_where(|leg| !bound.contains_key(leg));
    }

    /// The legs of the exit `parent` that still rest, or `None` when `parent` is no split exit.
    pub fn live_exit_legs(&self, parent: &str) -> Option<Vec<String>> {
        self.exit_legs.live_legs(parent)
    }

    /// The exit whose leg `order_client_id` is, while its split exit lives (`crate::execution::exit_legs`): what a reconcile fill of a leg
    /// reports as its order client id (`crate::reconcile::deal_order_client_ids`).
    pub fn exit_leg_parent(&self, order_client_id: &str) -> Option<&str> {
        self.exit_legs.parent_of(order_client_id)
    }

    /// `order_client_id` is OURS: the engine asked this bridge to place it (`crate::execution::ExecutionClientCtrader`'s `submit`), or a restore
    /// taught it (`adopt_restored`). What lets a label that is no v2 id (an operator's own "MyStop", one minted before
    /// v2) read as ours (`crate::reconcile`'s `label_order_client_id`). A v2 id is not kept: its spelling says it already.
    pub fn know_order_client_id(&mut self, order_client_id: &str) {
        if order_client_id.is_empty() || is_v2_order_client_id(order_client_id) {
            return;
        }
        self.known.insert(order_client_id, KNOWN_ORDER_CLIENT_IDS_CAP);
    }

    /// Whether [`Self::know_order_client_id`] holds `order_client_id`.
    pub fn knows_order_client_id(&self, order_client_id: &str) -> bool {
        self.known.contains(order_client_id)
    }

    /// Fold a canonical event through the split-exit table: an event of a leg becomes its parent's (or nothing),
    /// everything else comes back untouched. The one call every site that hands an event to the core makes
    /// (`crate::transport::connection`'s `emit_event`).
    pub fn fold_exit_leg_event(&mut self, event: Event) -> Vec<Event> {
        self.exit_legs.fold(event)
    }

    /// The room `kind` has left on the open position `position_id`: its volume less the live exits of
    /// that kind bound to it (an orphaned or cancelling exit no longer counts), and less nothing for
    /// `except`, an exit being amended whose own volume is the thing in question. `None` for a position
    /// not in the book.
    fn room_of(&self, position_id: i64, kind: ExitKind, except: Option<&str>) -> Option<i64> {
        let live = self.open.get(&position_id)?;
        let covered: i64 = self
            .bound
            .iter()
            .filter(|(order_client_id, b)| {
                b.position_id == position_id
                    && b.kind == kind
                    && !b.orphaned
                    && !b.cancelling
                    && Some(order_client_id.as_str()) != except
            })
            .fold(0i64, |a, (_, b)| a.saturating_add(b.volume.max(b.asked_volume.unwrap_or(0))));
        Some(live.volume.saturating_sub(covered))
    }

    /// An AMEND of the exit `order_client_id` to `volume` has been REQUESTED: from now until the venue answers it
    /// ([`Self::set_exit_volume`] on `ORDER_REPLACED`, [`Self::exit_amend_failed`] on a refusal) the exit counts at
    /// the larger of its size and the asked one.
    pub fn request_exit_volume(&mut self, order_client_id: &str, volume: i64) {
        if let Some(b) = self.bound.get_mut(order_client_id) {
            b.asked_volume = Some(volume);
        }
    }

    /// The amend of `order_client_id` was refused or never reached the venue: the exit keeps its size and stops counting the
    /// asked one.
    pub fn exit_amend_failed(&mut self, order_client_id: &str) {
        if let Some(b) = self.bound.get_mut(order_client_id) {
            b.asked_volume = None;
        }
    }

    /// The largest volume the bound exit `order_client_id` may be amended to: its position's volume less every OTHER
    /// live exit of its kind. `None` when `order_client_id` is not a bound exit whose position is in the book.
    pub fn exit_room(&self, order_client_id: &str) -> Option<i64> {
        let b = self.bound.get(order_client_id)?;
        self.room_of(b.position_id, b.kind, Some(order_client_id))
    }

    /// The engine asked to cancel `order_client_id`: stop counting it toward its position's cover (a cancel and an
    /// immediate re-arm must bind). [`Self::cancel_failed`] puts it back.
    pub fn mark_cancelling(&mut self, order_client_id: &str) {
        if let Some(b) = self.bound.get_mut(order_client_id) {
            b.cancelling = true;
        }
    }

    /// A cancel command of `order_client_id` never reached the venue (the write failed): forget that it was written,
    /// both the bridge's own (`cancel_sent`) and the engine's (`cancelling`), so the cancel is owed again.
    pub fn cancel_unsent(&mut self, order_client_id: &str) {
        if let Some(b) = self.bound.get_mut(order_client_id) {
            b.cancel_sent = false;
            b.cancelling = false;
        }
    }

    /// Record the volume a bound exit REALLY has: the venue's confirmed one after an amend
    /// (`ORDER_REPLACED`), or the one actually sent once the mapper has put it on the grid. It is the
    /// truth and is recorded as it is, even if it is larger than the room (an amend made by hand in the UI,
    /// say): the cap is enforced where an amend is REQUESTED (`ExecutionClientCtrader::modify`, via
    /// [`Self::exit_room`]), and counting a bigger exit as smaller would only hide the over-cover.
    pub fn set_exit_volume(&mut self, order_client_id: &str, volume: i64) {
        if let Some(b) = self.bound.get_mut(order_client_id) {
            b.volume = volume;
            b.asked_volume = None; // the venue has answered whatever amend asked for another size
        }
        self.exit_legs.set_volume(order_client_id, volume); // a split exit's leg: the size it backs
    }

    /// Mark every exit bound to `position_id` (except `except`, the order whose own event this is) as
    /// ORPHANED: its position is gone, so it must be cancelled.
    pub fn orphan_exits_on(&mut self, position_id: i64, except: Option<&str>) {
        for (order_client_id, b) in &mut self.bound {
            if b.position_id == position_id && Some(order_client_id.as_str()) != except {
                b.orphaned = true;
            }
        }
    }

    /// Mark one bound exit ORPHANED (its position is not in the book).
    pub fn orphan_exit(&mut self, order_client_id: &str) {
        if let Some(b) = self.bound.get_mut(order_client_id) {
            b.orphaned = true;
        }
    }

    /// Every orphaned exit the bridge has not yet written a cancel for.
    pub fn exits_to_cancel(&self) -> Vec<String> {
        self.bound
            .iter()
            .filter(|(_, b)| b.orphaned && !b.cancel_sent)
            .map(|(order_client_id, _)| order_client_id.clone())
            .collect()
    }

    /// The bridge wrote the cancel of an orphaned exit.
    pub fn mark_cancel_sent(&mut self, order_client_id: &str) {
        if let Some(b) = self.bound.get_mut(order_client_id) {
            b.cancel_sent = true;
        }
    }

    /// Whether the bridge owes, or has written, its own cancel of `order_client_id` (an orphaned exit), so the
    /// engine's cancel of the same order (an OCO sibling racing the bridge's) is not written a second time.
    pub fn cancel_in_flight(&self, order_client_id: &str) -> bool {
        self.bound.get(order_client_id).is_some_and(|b| b.orphaned)
    }

    /// The venue REFUSED a cancel of `order_client_id` (the bridge's own or the engine's): give up on the bridge's (no
    /// retry loop), let the engine's through, and count the exit toward its position's cover again, since
    /// it still rests.
    pub fn cancel_failed(&mut self, order_client_id: &str) {
        if let Some(b) = self.bound.get_mut(order_client_id) {
            b.orphaned = false;
            b.cancel_sent = false;
            b.cancelling = false;
        }
    }

    /// After an AUTHORITATIVE replace: every bound exit whose position the venue no longer lists.
    fn orphan_exits_without_a_position(&mut self) {
        for b in self.bound.values_mut() {
            if !self.open.contains_key(&b.position_id) {
                b.orphaned = true;
            }
        }
    }

    /// Replace the whole set from a reconcile answer this build read IN FULL, and mark it FETCHED
    /// at `now_ms`. The reconcile lists only OPEN positions, so replacing wholesale is correct —
    /// anything closed while we were not listening is absent and thus dropped.
    ///
    /// ⚠ **The caller owes "in full", and [`reconcile_rows`] is what measures it.** This method
    /// makes the book AUTHORITATIVE, which is what lets a downstream `opposing_available` total of
    /// `0` become `PositionEvidence::Flat` — a REFUSAL under `halt_admit = "verify"`. An answer with
    /// even one row this build could not classify must go to [`Self::replace_unverified`] instead:
    /// "we could not read that row" and "there is nothing there" are the same absence in the map,
    /// and only one of them may refuse an operator's exit.
    pub fn replace_all(
        &mut self,
        positions: impl IntoIterator<Item = (i64, PositionTracked)>,
        now_ms: i64,
    ) {
        self.replace_entries(positions);
        self.fetched_at_ms = Some(now_ms);
        // An answer read in full (this account's, every row classified: the only way here) IS knowledge
        // that a position is gone: its bound exits are orphans. (`replace_unverified` and `invalidate` do not
        // do this: an absence there is not evidence.) ⚠ This does NOT demand per-symbol COVERAGE the way
        // [`Self::unauthoritative_for`] does, on purpose: coverage needs a position still listed in the
        // symbol, and the case this exists for is the position that closed during the gap, leaving the
        // account flat, where an exit left resting OPENS a position under the reused id. The cost of the
        // other error (a venue that omits a really-open position from a well-formed answer, seen only in
        // a scripted fake) is a protective exit cancelled; the log names it and the node's `POSITION
        // WITHOUT STOP` line shows it.
        self.orphan_exits_without_a_position();
    }

    /// [`Self::replace_all`] for an answer that could NOT be read in full: the entries are taken
    /// (they are still the best routing guess available, and the reconcile is authoritative about
    /// the rows it DID express) while the book stays NOT authoritative.
    ///
    /// The asymmetry is the point, and it mirrors [`Self::invalidate`]: routing may run on the best
    /// guess, evidence may not. A book rebuilt from a partially-unreadable answer answers `Unknown`
    /// at the halt boundary, which ADMITS — the same verdict a never-fetched book gets, for the same
    /// reason.
    pub fn replace_unverified(
        &mut self,
        positions: impl IntoIterator<Item = (i64, PositionTracked)>,
    ) {
        self.replace_entries(positions);
        self.fetched_at_ms = None;
    }

    /// The entry half both replace paths share, so they cannot disagree about what "replace" means.
    fn replace_entries(&mut self, positions: impl IntoIterator<Item = (i64, PositionTracked)>) {
        self.open.clear();
        self.open.extend(positions);
    }

    /// Stop treating this book as EVIDENCE — the socket died, so anything could have happened.
    /// Deliberately keeps the entries: the reduce planner's routing is unchanged by this call, and
    /// dropping them would turn a reconnect into a hedge-opening window. It touches no bound-exit flag: it is
    /// also called on a LIVE socket (an unclassifiable position ref, an answer for another account), where a
    /// cancel already written is still on its way; [`Self::socket_died`] is the dead-socket half.
    pub fn invalidate(&mut self) {
        self.fetched_at_ms = None;
    }

    /// The socket DIED (the actor's reconnect): a cancel written on it may never have arrived, so an
    /// orphaned exit is owed its cancel again (the reconnect either re-sends it or finds the order gone),
    /// and an exit the engine was cancelling counts toward its position's cover again until the venue says
    /// it is gone. Called together with [`Self::invalidate`], and only there. It also starts the SETTLE ([`Self::is_settling`]).
    pub fn socket_died(&mut self) {
        for b in self.bound.values_mut() {
            b.cancel_sent = false;
            b.cancelling = false;
        }
        self.settling = true;
    }

    /// **A dead socket's reconnect has not yet handed on what the venue's lists say of what it left unanswered**:
    /// from [`Self::socket_died`] until [`Self::settled`]. A reconcile pass meanwhile sets the venue against a book that
    /// has not heard it: a close that filled unheard reads as a missed fill, a drifted position and an orphaned order,
    /// and the missed fill stays held after the settle books it (`crate::refusals::CtraderReconcileRefusal::Settling`).
    pub fn is_settling(&self) -> bool {
        self.settling
    }

    /// The reconnect's reconcile is over, whatever it could read: its events are on their way to the core.
    pub fn settled(&mut self) {
        self.settling = false;
    }

    /// The actor wrote `order_client_id` to the venue (or a reconcile found it resting): a reconnect now judges it
    /// against the venue's pending orders.
    pub fn mark_sent(&mut self, order_client_id: &str) {
        if let Some(b) = self.bound.get_mut(order_client_id) {
            b.sent = true;
        }
    }

    /// The actor wrote the `NewOrder` of `order_client_id` to a socket at `now_ms`.
    pub fn note_order_written(&mut self, order_client_id: &str, now_ms: i64) {
        self.written_orders.insert(order_client_id.to_string(), now_ms);
    }

    /// `event` is on its way to the core: an order it ends is nothing a reconnect need ask about.
    pub fn note_order_ended(&mut self, event: &Event) {
        if let Some(order_client_id) = event.ended_order() {
            self.written_orders.remove(order_client_id);
        }
    }

    /// After a reconnect's reconcile answer: the written orders still unanswered that its pending list (`present`)
    /// does not hold, each with when it was written, and forgotten here: asked about once.
    pub fn take_written_orders_not_in(
        &mut self,
        present: &std::collections::HashSet<String>,
    ) -> HashMap<String, i64> {
        let (kept, gone) =
            std::mem::take(&mut self.written_orders).into_iter().partition::<HashMap<_, _>, _>(
                |(order_client_id, _)| present.contains(order_client_id),
            );
        self.written_orders = kept;
        gone
    }

    /// After a reconnect's reconcile answer: drop every row that was WRITTEN to the venue and is not among
    /// its pending orders (`present`): the order was never accepted (the socket died after the write), or it
    /// filled, was cancelled or expired during the gap. Left, it would keep counting toward its position's
    /// cover (a re-armed stop refused as already covered, the position naked) and could never be cancelled
    /// (no venue order id). Rows not yet written (queued behind the reconnect) are kept.
    pub fn forget_sent_exits_not_in(&mut self, present: &std::collections::HashSet<String>) {
        self.bound.retain(|order_client_id, b| !b.sent || present.contains(order_client_id));
    }

    /// Remember the exits the connect-time look found orphaned ([`OrphanedExit`]), replacing an earlier list.
    pub fn record_orphaned_exits(&mut self, exits: Vec<OrphanedExit>) {
        self.orphaned_at_connect = exits;
    }

    /// The orphaned exits found at connect that have not ended since: what an operator has to cancel by hand.
    pub fn orphaned_exits(&self) -> &[OrphanedExit] {
        &self.orphaned_at_connect
    }

    /// The order `order_id` ended (filled, cancelled by hand, expired): it is no orphan to warn about any more.
    pub fn forget_orphaned_exit(&mut self, order_id: i64) {
        self.orphaned_at_connect.retain(|e| e.order_id != order_id);
    }

    /// When this book was last authoritatively fetched, or `None` for never/invalidated.
    pub fn fetched_at_ms(&self) -> Option<i64> {
        self.fetched_at_ms
    }

    /// `true` once an authoritative fetch has landed and the socket has not dropped since. ⚠ This
    /// is the PROVENANCE half only — it says a past answer was trustworthy, not that this book
    /// currently covers any particular instrument. [`Self::unauthoritative_for`] is what a refusal
    /// must consult; this accessor exists for the provenance tests and for logging.
    pub fn is_fetched(&self) -> bool {
        self.fetched_at_ms.is_some()
    }

    /// Why this book may NOT be read as EVIDENCE about `symbol_id`, or `None` when it may.
    ///
    /// The `Some` payload is the exact sentence that becomes `vike_execution::halt::PositionEvidence`'s
    /// `Unknown` reason (which ADMITS, and is logged), so the two states are told apart in the log
    /// of an incident rather than collapsed into one "unknown".
    ///
    /// ⚠ **The second arm — COVERAGE — is the one that stops a halt trapping an operator.** Only a
    /// symbol the venue positively reported a position in may have an absence read as a fact about
    /// it. See the type doc for the measured trap the provenance arm alone let through: an
    /// omitted-but-real position is indistinguishable from a flat account by any row count, and
    /// refusing on it costs the operator their exit under an engaged kill switch.
    ///
    /// ⚠ It is deliberately PER SYMBOL and not "the book is non-empty anywhere". cTrader's
    /// `ProtoOAReconcileRes.position` is account-wide, so a non-empty answer is formally a
    /// statement about every symbol — but resting a refusal on that means one instrument's row
    /// licensing a refusal about a different instrument, which is an absence again with extra
    /// steps. The strongest claim this type can honestly make is the one it makes: **a refusal
    /// requires a POSITIVE report in the very symbol being refused.**
    pub fn unauthoritative_for(&self, symbol_id: i64) -> Option<&'static str> {
        if self.fetched_at_ms.is_none() {
            return Some(
                "the position book is not authoritative: never fetched, invalidated by a \
                 reconnect, answered for another account, or rebuilt from a reconcile answer this \
                 build could not read in full",
            );
        }
        if !self.open.values().any(|p| p.symbol_id == symbol_id) {
            return Some(
                "the venue has reported no position in this symbol at all — an absence, which is \
                 not the same fact as a flat account",
            );
        }
        None
    }

    /// Every tracked open position in `symbol_id`, as the `(position_id, PositionTracked)` pairs
    /// [`plan_reduce`] reads.
    pub fn for_symbol(&self, symbol_id: i64) -> Vec<(i64, PositionTracked)> {
        self.open
            .iter()
            .filter(|(_, p)| p.symbol_id == symbol_id)
            .map(|(id, p)| (*id, *p))
            .collect()
    }

    /// How many open positions are tracked (any symbol) — for tests and logging.
    pub fn len(&self) -> usize {
        self.open.len()
    }

    /// `true` when no position is tracked. ⚠ Says NOTHING about whether the account is flat: pair
    /// it with [`Self::is_fetched`], which is the whole reason this type exists.
    pub fn is_empty(&self) -> bool {
        self.open.is_empty()
    }
}

/// Total opposing-side open volume (centi-units) a submit of `order_side` could reduce against
/// `open` — the positions facing the OTHER way (a SELL opposes LONGs, a BUY opposes SHORTs). Used to
/// tell "genuinely no exposure to reduce" (→ open a new order) from "all opposing exposure is
/// already being closed" (→ reject, never open a hedge) after the in-flight exclusion.
pub fn opposing_available(order_side: i32, open: &[(i64, PositionTracked)]) -> i64 {
    if order_side == 0 {
        return 0;
    }
    open.iter()
        .filter(|(_, p)| p.side == -order_side)
        .fold(0i64, |a, (_, p)| a.saturating_add(p.volume))
}

#[path = "positions_tests.rs"]
#[cfg(test)]
mod positions_tests;
