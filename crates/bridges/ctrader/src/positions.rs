//! Pure position-tracking + reduce-routing logic for the cTrader exec path. No I/O, no actor
//! state — just the decode of a wire `ProtoOAPosition` into a bridge-local [`TrackedPosition`] and
//! the FIFO reduce planner [`plan_reduce`] that decides whether a submit REDUCES an open position
//! (route to `ProtoOAClosePositionReq`, per position id) or OPENS one (route to
//! `ProtoOANewOrderReq`, unchanged). Kept separate from `conn`/`exec` so the routing law is
//! fixture-testable without a socket. Ports nothing — cTrader Open API
//! (https://help.ctrader.com/open-api/).
//!
//! Volumes throughout are cTrader **centi-units** (1/100 of a base unit — `volume = units × 100`),
//! the SAME scale `event_mapper::order_to_new_order`/`symbols::VolumeGrid` use.

use std::collections::HashMap;

use crate::proto::{ProtoOaPosition, ProtoOaPositionStatus, ProtoOaTradeSide};

/// One open venue position the bridge tracks, keyed elsewhere by its numeric `positionId`. Learned
/// from `ProtoOAExecutionEvent.position` on every exec event (and re-seeded from a reconcile). On a
/// HEDGING account several same-symbol positions coexist (long AND short); on a NETTING account at
/// most one per symbol — the reduce planner is agnostic to which, since it works off the actual
/// tracked set.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TrackedPosition {
    /// The symbol id the position is in (matched against a submit's resolved symbol id).
    pub symbol_id: i64,
    /// Signed direction: `+1` long (BUY), `-1` short (SELL).
    pub side: i32,
    /// Open volume magnitude in centi-units (always ≥ 0; the sign lives in [`Self::side`]).
    pub volume: i64,
    /// `tradeData.openTimestamp` (epoch ms) — the FIFO key (oldest closed first). Absent → `0`,
    /// which sorts oldest, so an unknown-age position is closed before dated ones (conservative:
    /// flatten the position we know least about first).
    pub open_ts: i64,
}

/// Decode a wire `ProtoOAPosition` into `(position_id, TrackedPosition)` when it is a live OPEN
/// position, else `None`. A CLOSED/CREATED/ERROR-status position (or one with zero volume) is not
/// tracked — the caller REMOVES it from the map instead. `CREATED` is the empty placeholder cTrader
/// makes for a pending order; it holds no real exposure.
pub fn tracked_position(p: &ProtoOaPosition) -> Option<(i64, TrackedPosition)> {
    if !is_open(p.position_status) || p.trade_data.volume <= 0 {
        return None;
    }
    let side = if p.trade_data.trade_side == ProtoOaTradeSide::Sell as i32 { -1 } else { 1 };
    Some((
        p.position_id,
        TrackedPosition {
            symbol_id: p.trade_data.symbol_id,
            side,
            volume: p.trade_data.volume,
            open_ts: p.trade_data.open_timestamp.unwrap_or(0),
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
/// — and a hole read as an absence is what turns `PositionBook::is_fetched` into a manufactured
/// `PositionEvidence::Flat`, i.e. a refused exit under `halt_admit = "verify"`. Callers give the
/// tracked rows to [`PositionBook::replace_all`] only when `unreadable == 0`, and to
/// [`PositionBook::replace_unverified`] otherwise.
///
/// The tracked half is byte-identical to the `filter_map` it replaces, so ROUTING is unchanged: this
/// only changes whether the resulting book is allowed to call itself evidence.
pub fn reconcile_rows(rows: &[ProtoOaPosition]) -> (Vec<(i64, TrackedPosition)>, usize) {
    let mut tracked = Vec::with_capacity(rows.len());
    let mut unreadable = 0usize;
    for p in rows {
        match tracked_position(p) {
            Some(row) => tracked.push(row),
            None if row_is_known_to_carry_no_exposure(p) => {}
            None => unreadable += 1,
        }
    }
    (tracked, unreadable)
}

/// `true` when the venue TOLD us this row holds no exposure, as opposed to this build failing to
/// read it. CLOSED and CREATED are the two states that mean exactly that; every other rejection —
/// ERROR, an unknown status value, or an OPEN row with a non-positive volume — is a hole, and
/// [`reconcile_rows`] counts it.
///
/// ⚠ Public because the SAME split is owed on the live-event path, not only on the reconcile one.
/// `crates/bridges/ctrader/src/conn.rs`'s `update_position_map` sees a `position` ref on every
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
/// Rule (mode-agnostic — it reads the ACTUAL tracked positions, not the account's netting mode):
///   * The reducible positions are those OPPOSITE to `order_side` (a SELL closes LONGs, a BUY
///     closes SHORTs), taken FIFO by `open_ts` (tie-broken by `position_id` for determinism).
///   * `available` = their total volume. If it is `0` (no opposing exposure — a flat book or a
///     same-side add), the submit OPENS → [`ClosePlan::Open`].
///   * A `reduce_only` submit closes `min(requested, available)` — capped, never opening (excess is
///     dropped, standard reduce-only semantics).
///   * A plain submit with `requested <= available` closes exactly `requested` (a pure
///     reduce/flatten). THIS is the hedging-flatten fix: an opposite market order now nets the
///     position flat instead of stacking a hedge.
///   * A plain submit with `requested > available` is a FLIP (reduce past flat into the other
///     direction). To PRESERVE netting-account flip behavior — and to keep one core order mapped to
///     one coherent venue lifecycle (never a mixed close+open under one coid) — the WHOLE submit
///     routes as a normal `ProtoOANewOrderReq` ([`ClosePlan::Open`], unchanged). On a netting
///     account the venue flips correctly; a flip on a hedging account remains the operator's manual
///     concern (the reported bug was flatten/reduce, which IS fixed).
pub fn plan_reduce(
    order_side: i32,
    requested_centi: i64,
    reduce_only: bool,
    open: &[(i64, TrackedPosition)],
) -> ClosePlan {
    if order_side == 0 || requested_centi <= 0 {
        return ClosePlan::Open; // a zero-side / zero-volume submit is the new-order path's reject
    }
    // Positions this submit can close: the ones facing the opposite way.
    let mut opposing: Vec<(i64, TrackedPosition)> =
        open.iter().filter(|(_, p)| p.side == -order_side && p.volume > 0).copied().collect();
    if opposing.is_empty() {
        return ClosePlan::Open;
    }
    // FIFO: oldest open first; deterministic tie-break on position id.
    opposing.sort_by(|(ia, a), (ib, b)| a.open_ts.cmp(&b.open_ts).then(ia.cmp(ib)));
    let available: i64 = opposing.iter().map(|(_, p)| p.volume).sum();

    let close_total = if reduce_only {
        requested_centi.min(available)
    } else if requested_centi <= available {
        requested_centi
    } else {
        // Flip → leave to the normal new-order path (preserve netting behavior).
        return ClosePlan::Open;
    };
    if close_total <= 0 {
        return ClosePlan::Open;
    }

    // Build the FIFO legs, taking min(remaining, position volume) from each in turn.
    let mut remaining = close_total;
    let mut legs: Vec<(i64, i64)> = Vec::new();
    for (position_id, p) in opposing {
        if remaining <= 0 {
            break;
        }
        let take = remaining.min(p.volume);
        legs.push((position_id, take));
        remaining -= take;
    }
    ClosePlan::Close(legs)
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

/// Per-coid aggregation state for in-flight closes, so the actor can present N FIFO
/// `ProtoOAClosePositionReq` legs (potentially across several positions) as ONE coherent core order
/// lifecycle: `OrderAccepted` once, then `OrderPartiallyFilled` per leg until the LAST fill, which
/// becomes `OrderFilled`. Keyed off the position id each close event references (`deal.positionId`/
/// `order.positionId`), since the closing order carries no `clientOrderId` of ours.
#[derive(Debug, Default)]
pub struct CloseTracker {
    /// positionId → the coid of the submit that is closing it.
    by_position: HashMap<i64, String>,
    /// coid → remaining centi-volume still to fill before the order is terminal.
    remaining: HashMap<String, i64>,
    /// coid → cumulative centi-volume actually filled so far — the "partial progress" flag a
    /// failure (leg reject / wire error / write failure) reads to pick a LEGAL terminal: a coid that
    /// has filled some volume is at FSM `PartiallyFilled`, from which `OrderRejected` is illegal, so
    /// it must terminalize as `OrderFilled` instead (see [`Self::resolve_failure`]).
    filled: HashMap<String, i64>,
    /// coids that have already emitted their single `OrderAccepted`.
    accepted: std::collections::HashSet<String>,
}

/// How a close coid must be terminalized when a leg FAILS (venue reject, wire `ERROR_RES`, or a
/// write failure), so the order always reaches a LEGAL terminal state — never a dropped one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CloseFailure {
    /// Earlier legs already FILLED (FSM at `PartiallyFilled`): terminalize as `OrderFilled` — an
    /// `OrderRejected` from `PartiallyFilled` is an illegal transition and would be DROPPED,
    /// stranding the coid. The already-filled money/position is correct (folded via the legs' bare
    /// `Event::Fill`); this is purely a legal FSM terminal.
    TerminalFilled,
    /// Nothing filled yet (still `Submitted`/`Accepted`): a clean `OrderRejected`.
    Rejected,
}

/// What the actor should emit for one close-correlated execution event, decided by [`CloseTracker`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CloseEmit {
    /// Emit the single `OrderAccepted` for this coid (first close event seen), and nothing else.
    AcceptOnly,
    /// A non-final fill: emit `OrderPartiallyFilled` (also `OrderAccepted` first if not yet sent).
    Partial { accept_first: bool },
    /// The final fill (cumulative reduce satisfied): emit `OrderFilled` (also `OrderAccepted` first
    /// if not yet sent), then the tracker forgets this coid.
    Final { accept_first: bool },
}

impl CloseTracker {
    /// Register a planned close: `coid` will close `legs` (`(position_id, volume_centi)`), whose
    /// volumes sum to the reduce total the resulting fills must reach before the order is terminal.
    pub fn register(&mut self, coid: &str, legs: &[(i64, i64)]) {
        let total: i64 = legs.iter().map(|(_, v)| v).sum();
        self.remaining.insert(coid.to_string(), total);
        for (position_id, _) in legs {
            self.by_position.insert(*position_id, coid.to_string());
        }
    }

    /// The coid closing `position_id`, if a close is in flight for it.
    pub fn coid_for(&self, position_id: i64) -> Option<&str> {
        self.by_position.get(&position_id).map(String::as_str)
    }

    /// Whether a close is IN FLIGHT for `position_id` — the routing exclusion (a new reduce must
    /// NOT plan/register against a position already being closed under another coid, or it would
    /// OVERWRITE that coid's `by_position` entry and strand it).
    pub fn is_closing(&self, position_id: i64) -> bool {
        self.by_position.contains_key(&position_id)
    }

    /// Whether `coid` is a close coid this tracker still owns (registered and not yet
    /// forgotten) — so a wire-level failure (`on_error_res`, write failure) can tell a close coid
    /// from an ordinary order and route it through [`Self::resolve_failure`].
    pub fn is_tracked(&self, coid: &str) -> bool {
        self.remaining.contains_key(coid) || self.accepted.contains(coid)
    }

    /// Fold one close-correlated execution event for `coid`. `fill_volume` is the deal's filled
    /// centi-volume (`0` for an accept-only event with no deal). Returns the [`CloseEmit`] decision
    /// and, on `Final`, prunes the coid (and its position entries) from the tracker.
    pub fn on_event(&mut self, coid: &str, fill_volume: i64) -> CloseEmit {
        let accept_first = self.accepted.insert(coid.to_string()); // true iff newly inserted
        if fill_volume <= 0 {
            // No deal on this event (e.g. a bare ORDER_ACCEPTED for the closing order).
            return if accept_first {
                CloseEmit::AcceptOnly
            } else {
                CloseEmit::Partial { accept_first: false }
            };
        }
        *self.filled.entry(coid.to_string()).or_insert(0) += fill_volume;
        let remaining = self.remaining.entry(coid.to_string()).or_insert(0);
        *remaining -= fill_volume;
        if *remaining <= 0 {
            self.forget(coid);
            CloseEmit::Final { accept_first }
        } else {
            CloseEmit::Partial { accept_first }
        }
    }

    /// Whether `coid` has already filled some volume — the NON-mutating read behind the
    /// terminal-choice a failure makes. Exposed so a CAPTURE-time failure event (see
    /// `conn::write_failure_event`, built BEFORE the write is even attempted) can pick
    /// Filled-vs-Rejected WITHOUT forgetting the coid — the forget happens only if the write
    /// actually fails.
    pub fn has_progress(&self, coid: &str) -> bool {
        self.filled.get(coid).copied().unwrap_or(0) > 0
    }

    /// Terminalize `coid` on a FAILURE (leg reject / wire error / write failure) and FORGET it,
    /// picking the LEGAL terminal by whether any volume already filled: [`CloseFailure::TerminalFilled`]
    /// if so (`OrderRejected` would be an illegal drop from `PartiallyFilled`), else
    /// [`CloseFailure::Rejected`]. Idempotent — a coid the tracker no longer owns resolves as
    /// `Rejected` (nothing filled under our watch).
    pub fn resolve_failure(&mut self, coid: &str) -> CloseFailure {
        let progressed = self.filled.get(coid).copied().unwrap_or(0) > 0;
        self.forget(coid);
        if progressed {
            CloseFailure::TerminalFilled
        } else {
            CloseFailure::Rejected
        }
    }

    /// Drop all state for `coid` (its remaining + filled counters, its accepted flag, every position
    /// entry pointing at it). Called on the final fill, and by [`Self::resolve_failure`].
    pub fn forget(&mut self, coid: &str) {
        self.remaining.remove(coid);
        self.filled.remove(coid);
        self.accepted.remove(coid);
        self.by_position.retain(|_, c| c != coid);
    }
}

/// The account's OPEN positions **and whether the venue has ever told us what they are**.
///
/// ⚠ This type exists because a bare `HashMap` cannot distinguish the two states that matter most:
/// an empty map is the SAME value for *"we have never asked"* and *"we asked, and you are flat"*.
/// That ambiguity is not academic — it already cost this crate a feature. [`plan_reduce`] against a
/// never-populated map sees no opposing exposure and OPENS a hedge, which is exactly why
/// `crates/bridges/ctrader/src/exec.rs`'s `close_all` had to exist ("12 stacked hedged positions a
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
/// under an engaged halt (`crates/bridges/ctrader/tests/exec_halt.rs`'s
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
pub struct PositionBook {
    /// positionId → the live OPEN position.
    open: HashMap<i64, TrackedPosition>,
    /// When an authoritative reconcile last REPLACED this set wholesale (epoch ms), or `None` for
    /// "never fetched / no longer trustworthy". See the type doc.
    fetched_at_ms: Option<i64>,
}

impl PositionBook {
    /// Learn (or refresh) one position from an execution event.
    pub fn upsert(&mut self, position_id: i64, tracked: TrackedPosition) {
        self.open.insert(position_id, tracked);
    }

    /// Forget a position the venue reports CLOSED / zero-volume.
    pub fn remove(&mut self, position_id: i64) {
        self.open.remove(&position_id);
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
        positions: impl IntoIterator<Item = (i64, TrackedPosition)>,
        now_ms: i64,
    ) {
        self.replace_entries(positions);
        self.fetched_at_ms = Some(now_ms);
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
        positions: impl IntoIterator<Item = (i64, TrackedPosition)>,
    ) {
        self.replace_entries(positions);
        self.fetched_at_ms = None;
    }

    /// The entry half both replace paths share, so they cannot disagree about what "replace" means.
    fn replace_entries(&mut self, positions: impl IntoIterator<Item = (i64, TrackedPosition)>) {
        self.open.clear();
        self.open.extend(positions);
    }

    /// Stop treating this book as EVIDENCE — the socket died, so anything could have happened.
    /// Deliberately keeps the entries: the reduce planner's routing is unchanged by this call, and
    /// dropping them would turn a reconnect into a hedge-opening window.
    pub fn invalidate(&mut self) {
        self.fetched_at_ms = None;
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
    /// The `Some` payload is the exact sentence that becomes `vike_exec::halt::PositionEvidence`'s
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

    /// Every tracked open position in `symbol_id`, as the `(position_id, TrackedPosition)` pairs
    /// [`plan_reduce`] reads.
    pub fn for_symbol(&self, symbol_id: i64) -> Vec<(i64, TrackedPosition)> {
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
pub fn opposing_available(order_side: i32, open: &[(i64, TrackedPosition)]) -> i64 {
    if order_side == 0 {
        return 0;
    }
    open.iter().filter(|(_, p)| p.side == -order_side).map(|(_, p)| p.volume).sum()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pos(symbol_id: i64, side: i32, volume: i64, open_ts: i64) -> TrackedPosition {
        TrackedPosition { symbol_id, side, volume, open_ts }
    }

    #[test]
    fn no_opposing_position_opens() {
        // Flat book.
        assert_eq!(plan_reduce(-1, 100_000, false, &[]), ClosePlan::Open);
        // Same-side add (a BUY while already long): opens.
        let open = vec![(1, pos(1, 1, 100_000, 10))];
        assert_eq!(plan_reduce(1, 100_000, false, &open), ClosePlan::Open);
    }

    #[test]
    fn full_close_of_single_position() {
        let open = vec![(7, pos(1, 1, 100_000, 10))];
        assert_eq!(plan_reduce(-1, 100_000, false, &open), ClosePlan::Close(vec![(7, 100_000)]));
    }

    #[test]
    fn partial_close_of_single_position() {
        let open = vec![(7, pos(1, 1, 100_000, 10))];
        // Reduce 40_000 of a 100_000 long → one close leg for 40_000 (order fills fully for 40k).
        assert_eq!(plan_reduce(-1, 40_000, false, &open), ClosePlan::Close(vec![(7, 40_000)]));
    }

    #[test]
    fn fifo_close_across_two_same_symbol_positions_oldest_first() {
        // Two long positions: P2 is NEWER (open_ts 20), P1 OLDER (open_ts 10). Present out of order.
        let open = vec![(2, pos(1, 1, 40_000, 20)), (1, pos(1, 1, 60_000, 10))];
        // Flatten 100_000: close the OLDER (P1, 60k) first, then the newer (P2, 40k).
        assert_eq!(
            plan_reduce(-1, 100_000, false, &open),
            ClosePlan::Close(vec![(1, 60_000), (2, 40_000)])
        );
    }

    #[test]
    fn fifo_partial_spanning_two_positions() {
        let open = vec![(1, pos(1, 1, 60_000, 10)), (2, pos(1, 1, 40_000, 20))];
        // Reduce 80_000: close P1 fully (60k) then 20k of P2.
        assert_eq!(
            plan_reduce(-1, 80_000, false, &open),
            ClosePlan::Close(vec![(1, 60_000), (2, 20_000)])
        );
    }

    #[test]
    fn plain_over_reduce_is_a_flip_and_opens() {
        // Long 100_000, plain SELL 150_000 (a flip past flat) → normal new-order path (unchanged).
        let open = vec![(1, pos(1, 1, 100_000, 10))];
        assert_eq!(plan_reduce(-1, 150_000, false, &open), ClosePlan::Open);
    }

    #[test]
    fn reduce_only_over_reduce_caps_at_available() {
        // reduce_only SELL 150_000 vs long 100_000 → close all 100_000, drop the excess.
        let open = vec![(1, pos(1, 1, 100_000, 10))];
        assert_eq!(plan_reduce(-1, 150_000, true, &open), ClosePlan::Close(vec![(1, 100_000)]));
    }

    #[test]
    fn buy_closes_short_positions_only() {
        // Mixed hedging book: a short P1 and a long P2. A BUY may only close the SHORT.
        let open = vec![(1, pos(1, -1, 50_000, 10)), (2, pos(1, 1, 30_000, 20))];
        assert_eq!(plan_reduce(1, 50_000, false, &open), ClosePlan::Close(vec![(1, 50_000)]));
    }

    #[test]
    fn snap_close_volume_full_and_partial() {
        assert_eq!(snap_close_volume(100_000, 100_000, 100_000), 100_000, "full close = exact");
        assert_eq!(snap_close_volume(120_000, 100_000, 100_000), 100_000, "capped at position");
        assert_eq!(
            snap_close_volume(40_000, 100_000, 100_000),
            100_000,
            "40k rounds to 1 step=100k"
        );
        assert_eq!(snap_close_volume(40_000, 500_000, 20_000), 40_000, "on-grid partial unchanged");
        assert_eq!(
            snap_close_volume(0, 100_000, 100_000),
            100_000,
            "never drops a positive request"
        );
        assert_eq!(snap_close_volume(33_333, 500_000, 0), 33_333, "unknown grid passes through");
    }

    #[test]
    fn close_tracker_single_leg_accept_then_final() {
        let mut t = CloseTracker::default();
        t.register("c1", &[(7, 100_000)]);
        assert_eq!(t.coid_for(7), Some("c1"));
        // The one fill both accepts (first event) and finalizes.
        assert_eq!(t.on_event("c1", 100_000), CloseEmit::Final { accept_first: true });
        assert_eq!(t.coid_for(7), None, "final prunes the position entry");
    }

    #[test]
    fn close_tracker_accept_event_then_fill() {
        let mut t = CloseTracker::default();
        t.register("c1", &[(7, 100_000)]);
        assert_eq!(t.on_event("c1", 0), CloseEmit::AcceptOnly);
        assert_eq!(t.on_event("c1", 100_000), CloseEmit::Final { accept_first: false });
    }

    #[test]
    fn close_tracker_two_legs_partial_then_final() {
        let mut t = CloseTracker::default();
        t.register("c1", &[(1, 60_000), (2, 40_000)]);
        assert_eq!(t.on_event("c1", 60_000), CloseEmit::Partial { accept_first: true });
        assert_eq!(t.on_event("c1", 40_000), CloseEmit::Final { accept_first: false });
        assert_eq!(t.coid_for(1), None);
        assert_eq!(t.coid_for(2), None);
    }

    #[test]
    fn is_closing_and_is_tracked_reflect_registration() {
        let mut t = CloseTracker::default();
        assert!(!t.is_closing(7));
        assert!(!t.is_tracked("c1"));
        t.register("c1", &[(7, 100_000)]);
        assert!(t.is_closing(7), "position is now in flight");
        assert!(t.is_tracked("c1"));
        t.forget("c1");
        assert!(!t.is_closing(7), "forget frees the position");
        assert!(!t.is_tracked("c1"));
    }

    #[test]
    fn resolve_failure_with_no_progress_is_a_clean_reject() {
        let mut t = CloseTracker::default();
        t.register("c1", &[(7, 100_000)]);
        assert_eq!(t.resolve_failure("c1"), CloseFailure::Rejected);
        assert!(!t.is_tracked("c1"), "forgotten");
        assert!(!t.is_closing(7));
    }

    #[test]
    fn resolve_failure_after_a_partial_fill_terminalizes_as_filled() {
        // leg-1 (P1) fills, THEN leg-2 (P2) fails: the coid is at PartiallyFilled, so it must
        // terminalize as OrderFilled (a Rejected would be an illegal, dropped transition).
        let mut t = CloseTracker::default();
        t.register("c1", &[(1, 60_000), (2, 40_000)]);
        assert_eq!(t.on_event("c1", 60_000), CloseEmit::Partial { accept_first: true });
        assert_eq!(t.resolve_failure("c1"), CloseFailure::TerminalFilled);
        assert!(!t.is_tracked("c1"));
        assert!(!t.is_closing(2), "the un-filled leg's position is freed too");
    }

    /// THE property the type exists for: an empty book that was never fetched and an empty book
    /// that WAS fetched are different values. Collapse them (drop `fetched_at_ms`, or default it to
    /// `Some`) and this test is the one that goes red — which is the difference between "you are
    /// flat" and "I have never asked", i.e. between refusing a restart-case exit and admitting it.
    #[test]
    fn an_empty_book_distinguishes_never_fetched_from_fetched_and_flat() {
        let mut never = PositionBook::default();
        assert!(!never.is_fetched(), "a fresh book has NOT been told anything");
        assert_eq!(never.fetched_at_ms(), None);
        assert!(never.is_empty());

        never.replace_all(std::iter::empty(), 1_700_000_000_000);
        assert!(never.is_fetched(), "an empty reconcile answer IS an answer");
        assert_eq!(never.fetched_at_ms(), Some(1_700_000_000_000));
        assert!(never.is_empty(), "…and the account really is flat");
    }

    /// A socket death stops the book being EVIDENCE without changing what it routes against —
    /// dropping the entries here would turn every reconnect into a hedge-opening window.
    #[test]
    fn invalidate_clears_the_evidence_but_keeps_the_routing_entries() {
        let mut book = PositionBook::default();
        book.replace_all([(7, pos(1, 1, 100_000, 10))], 1_700_000_000_000);
        assert_eq!(book.for_symbol(1).len(), 1);

        book.invalidate();
        assert!(!book.is_fetched(), "the socket dropped — this is no longer evidence");
        assert_eq!(book.for_symbol(1), vec![(7, pos(1, 1, 100_000, 10))], "routing is unchanged");
        assert_eq!(
            plan_reduce(-1, 100_000, true, &book.for_symbol(1)),
            ClosePlan::Close(vec![(7, 100_000)])
        );
    }

    /// A wire position row, for the reconcile-fidelity tests below.
    fn wire(position_id: i64, status: ProtoOaPositionStatus, volume: i64) -> ProtoOaPosition {
        ProtoOaPosition {
            position_id,
            position_status: status as i32,
            trade_data: crate::proto::ProtoOaTradeData {
                symbol_id: 1,
                volume,
                trade_side: ProtoOaTradeSide::Buy as i32,
                open_timestamp: Some(10),
                ..Default::default()
            },
            ..Default::default()
        }
    }

    /// The split [`reconcile_rows`] exists for: a row the venue says is EMPTY is knowledge, a row
    /// this build cannot classify is a HOLE, and only the second may cost the book its evidence
    /// flag. `tracked_position`'s `Option` collapses the two, which is exactly how a decode failure
    /// used to become a "flat" account.
    ///
    /// ⚠ **MEASURED** (the CI box, 2026-08-08, lane a): collapse the two `None` arms back into one
    /// (`None => {}`, i.e. count nothing as unreadable) and this module runs
    /// `24 tests run: 23 passed, 1 failed` with THIS the failure, plus
    /// `17 tests run: 16 passed, 1 failed` end to end in
    /// `crates/bridges/ctrader/tests/exec_halt.rs`.
    #[test]
    fn reconcile_rows_separates_a_known_empty_row_from_one_it_cannot_read() {
        use ProtoOaPositionStatus::*;
        // Knowledge: the venue told us these hold nothing.
        let (tracked, unreadable) = reconcile_rows(&[
            wire(1, PositionStatusOpen, 100_000),
            wire(2, PositionStatusClosed, 0),
            wire(3, PositionStatusCreated, 0),
        ]);
        assert_eq!(tracked, vec![(1, pos(1, 1, 100_000, 10))]);
        assert_eq!(unreadable, 0, "CLOSED and CREATED are answers, not holes");

        // Holes, one per shape: an ERROR position, a status value this build has no variant for,
        // and an OPEN row whose volume is not positive.
        let mut unknown_status = wire(5, PositionStatusOpen, 100_000);
        unknown_status.position_status = 99;
        let (tracked, unreadable) = reconcile_rows(&[
            wire(4, PositionStatusError, 100_000),
            unknown_status,
            wire(6, PositionStatusOpen, 0),
        ]);
        assert!(tracked.is_empty());
        assert_eq!(unreadable, 3, "every row this build could not classify must be COUNTED");
    }

    /// …and the consequence at the book: an answer with a hole in it REPLACES the routing entries
    /// and is NOT evidence. Route `replace_unverified` to `replace_all` (i.e. mark it fetched
    /// anyway) and this test goes red — which is the mutation that would turn an unreadable row
    /// into a refused exit under `halt_admit = "verify"`.
    #[test]
    fn a_partially_unreadable_answer_replaces_the_routing_but_is_not_evidence() {
        let mut book = PositionBook::default();
        book.replace_all([(7, pos(1, 1, 100_000, 10))], 1_700_000_000_000);
        assert!(book.is_fetched());

        book.replace_unverified([(8, pos(1, -1, 40_000, 20))]);
        assert!(
            !book.is_fetched(),
            "a reconcile answer this build could not read in full must not make the book \
             authoritative — an absence in it is a hole, not a flat account"
        );
        assert_eq!(book.fetched_at_ms(), None);
        assert_eq!(book.for_symbol(1), vec![(8, pos(1, -1, 40_000, 20))], "routing still updates");
    }

    /// ⚠ **THE COVERAGE RULE, and the trap it closes.** A reconcile answer that OMITS a position
    /// the venue holds is well-formed and fully classifiable — nothing counts it — so provenance
    /// alone (`is_fetched`) says "authoritative" about a symbol nobody ever mentioned. Coverage is
    /// what refuses to speak for it.
    ///
    /// ⚠ **MEASURED** (the CI box, 2026-08-08, lane a): disable the symbol arm of
    /// [`Self::unauthoritative_for`] (answer `None` once `fetched_at_ms` is `Some`) and this
    /// module runs `24 tests run: 22 passed, 2 failed` — THIS test and
    /// `the_two_unauthoritative_reasons_are_distinguishable` — plus
    /// `17 tests run: 15 passed, 2 failed` in `crates/bridges/ctrader/tests/exec_halt.rs`, where
    /// `verify_admits_the_exit_from_a_position_the_reconcile_answer_omitted` is the same property
    /// end to end against a venue that really is holding the position.
    #[test]
    fn a_fetched_book_is_only_evidence_about_symbols_it_positively_covers() {
        let mut book = PositionBook::default();
        assert!(
            book.unauthoritative_for(1).is_some(),
            "a never-fetched book is evidence about nothing"
        );

        // An authoritative, fully-read, EMPTY answer. Provenance is perfect; coverage is nil.
        book.replace_all(std::iter::empty(), 1_700_000_000_000);
        assert!(book.is_fetched(), "provenance is genuinely there");
        assert!(
            book.unauthoritative_for(1).is_some(),
            "…and it must STILL not speak for symbol 1: an answer that omits a position the venue \
             holds looks exactly like this, and reading it as `flat` refuses a live exit"
        );

        // One positive row in symbol 1 — now, and only now, symbol 1 may be spoken for.
        book.replace_all([(7, pos(1, 1, 100_000, 10))], 1_700_000_000_001);
        assert_eq!(book.unauthoritative_for(1), None, "the venue positively reported symbol 1");
        assert!(
            book.unauthoritative_for(9).is_some(),
            "a row in symbol 1 is not evidence about symbol 9 — that would be an absence again, \
             with extra steps"
        );

        // Coverage without provenance is not evidence either: both halves are required.
        book.invalidate();
        assert!(book.unauthoritative_for(1).is_some(), "the socket dropped — provenance is gone");
    }

    /// The two refusal reasons are DISTINCT strings, because they are distinct facts an operator
    /// reads out of one incident's log: "I never got an answer" and "the answer never mentioned
    /// your symbol" have different fixes.
    #[test]
    fn the_two_unauthoritative_reasons_are_distinguishable() {
        let mut book = PositionBook::default();
        let no_provenance = book.unauthoritative_for(1).expect("never fetched");
        book.replace_all(std::iter::empty(), 1_700_000_000_000);
        let no_coverage = book.unauthoritative_for(1).expect("fetched, but empty");
        assert_ne!(no_provenance, no_coverage);
        assert!(no_coverage.contains("no position in this symbol"), "{no_coverage}");
    }

    /// A reconcile REPLACES rather than merges: a position closed while we were not listening is
    /// absent from the answer and must not survive in the book.
    #[test]
    fn replace_all_drops_positions_the_venue_no_longer_reports() {
        let mut book = PositionBook::default();
        book.upsert(1, pos(1, 1, 60_000, 10));
        book.upsert(2, pos(1, 1, 40_000, 20));
        book.replace_all([(2, pos(1, 1, 40_000, 20))], 1_700_000_000_001);
        assert_eq!(book.for_symbol(1), vec![(2, pos(1, 1, 40_000, 20))]);
        assert_eq!(book.len(), 1);
    }

    /// `for_symbol` is a filter, and `remove` is the close path.
    #[test]
    fn for_symbol_filters_and_remove_forgets() {
        let mut book = PositionBook::default();
        book.upsert(1, pos(1, 1, 60_000, 10));
        book.upsert(2, pos(9, -1, 40_000, 20));
        assert_eq!(book.for_symbol(1), vec![(1, pos(1, 1, 60_000, 10))]);
        assert_eq!(book.for_symbol(9), vec![(2, pos(9, -1, 40_000, 20))]);
        book.remove(1);
        assert!(book.for_symbol(1).is_empty());
        assert_eq!(book.len(), 1);
    }

    #[test]
    fn opposing_available_sums_only_the_reducible_side() {
        // A SELL (order_side -1) opposes LONGs; a short in the book is NOT reducible by a SELL.
        let open = vec![
            (1, pos(1, 1, 60_000, 10)),
            (2, pos(1, 1, 40_000, 20)),
            (3, pos(1, -1, 25_000, 30)),
        ];
        assert_eq!(opposing_available(-1, &open), 100_000, "both longs");
        assert_eq!(opposing_available(1, &open), 25_000, "only the short");
        assert_eq!(opposing_available(0, &open), 0);
    }
}
