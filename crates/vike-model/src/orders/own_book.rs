//! [`OwnOrderBook`] — our OWN resting orders as a per-side price ladder, with
//! **accepted-buffer race handling**. Net-new Rust surface: there is NO Python twin in
//! `vike-trader-app` (the oracle app has no market maker), so nothing here is parity-gated; the
//! contract below IS the specification.
//!
//! Lives in the vocabulary crate so a consumer below the strategy layer (the execution engine, the
//! live core) can maintain the ladder from REAL venue lifecycle events; today its one driver is the
//! `vike-mm` maker, which wraps it in `OwnBookFiltration` (`crates/vike-mm/src/book.rs`).
//!
//! ## Why it exists
//!
//! Subtracting our own quotes from the public book is only right for a quoter that rests exactly
//! one order per side, remembers one `(price, size)` pair per side and never races the feed. It is
//! wrong in two ways the moment either assumption breaks:
//!
//! 1. **Multiple own orders per level.** A ladder/refresh maker (or one whose in-flight replace
//!    briefly leaves two orders resting) has N orders at a price, not one — the single snapshot
//!    can only subtract the last one it remembered.
//! 2. **The accepted→public race.** A venue ACKs an order microseconds before the public market-
//!    data feed shows it. Subtracting it in that window removes depth the feed never added —
//!    inventing a phantom-empty level and pushing a `Top`/`Join` maker to quote THROUGH real
//!    resting depth. This is the failure the `accepted_buffer` gate exists to prevent: an own
//!    order counts against displayed depth ONLY once it has been accepted long enough that the
//!    public book is expected to carry it.
//!
//! ## Shape
//!
//! Per side, a `BTreeMap<price_ticks, level>` (price-ordered, so a best-first walk is a plain
//! forward/reverse iteration), where a level is an insertion-ordered `IndexMap<coid, OwnOrder>` —
//! insertion order IS venue queue order, and it also makes the per-level f64 qty fold
//! deterministic (the `IndexMap`-not-`HashMap` rule in CLAUDE.md, here for fold determinism rather
//! than Python-dict parity). Removals go through `shift_remove`, never `swap_remove`, so queue
//! order survives a cancel in the middle of a level. A `coid -> (side, price_ticks)` index makes
//! every event-driven update (which names only the coid) an O(log n) hop instead of a scan.
//!
//! ## Clock and units
//!
//! Every timestamp here is ONE caller-chosen EVENT clock (never wall-clock — same discipline as
//! the fill-rate breaker). The `_ns` suffixes follow the HFT nanosecond convention; **the code
//! never converts or divides a timestamp**, so any single consistent unit is valid. The
//! `vike-mm` maker feeds its own event ts, which is epoch-MILLIS on the live quote/book/fill
//! lanes — so a maker-side `accepted_buffer_ns` is configured in MILLIS. The only invariant is
//! that `now_ns`, `accepted_buffer_ns` and every `ts_accepted` share a unit.
//!
//! ## Who drives it
//!
//! The update verbs ([`OwnOrderBook::on_submit`] / [`on_accepted`](OwnOrderBook::on_accepted) /
//! [`on_partial_fill`](OwnOrderBook::on_partial_fill) /
//! [`on_cancel_pending`](OwnOrderBook::on_cancel_pending) /
//! [`on_terminal`](OwnOrderBook::on_terminal) / [`on_modify`](OwnOrderBook::on_modify)) mirror the
//! order-lifecycle transitions the runtime already folds. They are keyed by an opaque `coid`
//! string, so:
//!
//! - a **strategy** drives them itself, keyed by its own ORDER TAGS (`"bid"`/`"ask"`) — the only
//!   ids a `Strategy` ever sees, since the runtime mints real client-order-ids the strategy is
//!   deliberately never shown (the `vike-mm` maker does exactly this, with an optimistic ack);
//! - a **mount/runtime** that DOES know the tag↔coid mapping (or drives a wider ladder) can feed
//!   REAL venue accepts/cancels, replacing the strategy's optimistic stance.
//!
//! Nothing in this module logs (it runs on a per-tick path) and nothing here is persisted — the
//! resting-order cache is rediscovered from the venue by the runtime's reconcile pass.

use std::collections::BTreeMap;

use indexmap::IndexMap;

/// Which side of the book an own order rests on. (`+1`/`-1` venue side codes convert via
/// [`OwnSide::from_signed`].)
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum OwnSide {
    /// A buy order — subtracted from the public BID ladder.
    Bid,
    /// A sell order — subtracted from the public ASK ladder.
    Ask,
}

impl OwnSide {
    /// Map a signed venue side code to a book side: `> 0` ⇒ [`OwnSide::Bid`], `< 0` ⇒
    /// [`OwnSide::Ask`], `0` ⇒ `None` (no side). Mirrors the `+1 bid / −1 ask` convention the
    /// maker's [`Fill`](crate::Fill) and `submit_limit_tagged` calls already use.
    pub fn from_signed(side: i32) -> Option<OwnSide> {
        match side.signum() {
            1 => Some(OwnSide::Bid),
            -1 => Some(OwnSide::Ask),
            _ => None,
        }
    }
}

/// Lifecycle status of one own order, as far as THIS process knows.
///
/// Deliberately only three states: they are the distinctions that change whether the order should
/// be subtracted from displayed depth. Terminal outcomes (filled / canceled / rejected / expired)
/// are not a status — they REMOVE the order ([`OwnOrderBook::on_terminal`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OwnStatus {
    /// Sent, not yet acknowledged. It is NOT in the public book, so it must never be subtracted —
    /// enforced structurally: a `Submitted` order has no `ts_accepted`, and the visibility gate
    /// (see [`OwnOrder::is_visible`]) fails without one.
    Submitted,
    /// Acknowledged by the venue and resting. Subtractable once the accepted-buffer has elapsed.
    Accepted,
    /// A cancel has been REQUESTED but not confirmed. The order is still resting at the venue (and
    /// still in the public book) until the cancel lands, so by default it still counts — a caller
    /// that would rather lean on the depth it is about to reclaim drops it via
    /// [`StatusMask::ACCEPTED_ONLY`].
    PendingCancel,
}

/// One own resting order at a level.
///
/// `qty` is the REMAINING quantity: [`OwnOrderBook::on_partial_fill`] decrements it, and an order
/// decremented to `<= 0` is removed (a full fill is terminal).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct OwnOrder {
    /// Where this order is in its lifecycle (see [`OwnStatus`]).
    pub status: OwnStatus,
    /// REMAINING quantity — the amount still resting, after any partial fills.
    pub qty: f64,
    /// Event ts at which the order was sent (see the module doc for the unit contract).
    pub ts_submitted: i64,
    /// Event ts at which the venue ACCEPTED it, or `None` while unacknowledged. The
    /// accepted-buffer gate is measured from here — which is why a never-accepted order can never
    /// be subtracted from the public book.
    pub ts_accepted: Option<i64>,
}

impl OwnOrder {
    /// Is this order expected to be VISIBLE in the public book at `now_ns`?
    ///
    /// True only when the venue has accepted it AND `ts_accepted + accepted_buffer_ns <= now_ns`.
    /// The buffer is the modeled ack→market-data propagation delay: inside it, the public feed has
    /// not yet added our size, so subtracting it would remove depth that was never there. A
    /// never-accepted order (`ts_accepted == None`) is never visible.
    ///
    /// A `0` buffer means "trust the accept immediately" — the behavior of the pre-existing
    /// single-snapshot filtration, which is why `0` makes the own-book path reduce to it.
    pub fn is_visible(&self, accepted_buffer_ns: i64, now_ns: i64) -> bool {
        match self.ts_accepted {
            // saturating so a pathological buffer can't wrap into "visible"
            Some(ts) => ts.saturating_add(accepted_buffer_ns) <= now_ns,
            None => false,
        }
    }
}

/// Which [`OwnStatus`]es a query counts — a tiny 3-flag set (no `bitflags` dependency; the
/// workspace pins its deps with rationale and this needs no new one).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct StatusMask {
    /// Count [`OwnStatus::Submitted`] orders. NOTE this is INERT for the qty queries: a submitted
    /// order has no `ts_accepted`, so it always fails the visibility gate. It exists so a mask can
    /// name the full status space (and for [`OwnOrderBook::orders_at`]-style introspection).
    pub submitted: bool,
    /// Count [`OwnStatus::Accepted`] orders (the normal case).
    pub accepted: bool,
    /// Count [`OwnStatus::PendingCancel`] orders — still resting until the cancel confirms.
    pub pending_cancel: bool,
}

impl StatusMask {
    /// Every status.
    pub const ALL: StatusMask =
        StatusMask { submitted: true, accepted: true, pending_cancel: true };
    /// Everything that is (or may still be) RESTING in the public book: accepted + pending-cancel.
    /// The DEFAULT, and the conservative choice — a cancel we requested but the venue has not
    /// confirmed is still our size sitting in the displayed depth.
    pub const RESTING: StatusMask =
        StatusMask { submitted: false, accepted: true, pending_cancel: true };
    /// Accepted only — drops orders we have asked to pull, for a maker that wants to lean on the
    /// depth it is about to reclaim.
    pub const ACCEPTED_ONLY: StatusMask =
        StatusMask { submitted: false, accepted: true, pending_cancel: false };

    /// Does this mask include `status`?
    pub fn contains(self, status: OwnStatus) -> bool {
        match status {
            OwnStatus::Submitted => self.submitted,
            OwnStatus::Accepted => self.accepted,
            OwnStatus::PendingCancel => self.pending_cancel,
        }
    }
}

impl Default for StatusMask {
    /// [`StatusMask::RESTING`] — see its doc for why pending-cancel counts by default.
    fn default() -> Self {
        StatusMask::RESTING
    }
}

/// The query knobs for [`OwnOrderBook::bid_qty_at`]/[`ask_qty_at`](OwnOrderBook::ask_qty_at): an
/// own order counts toward the returned qty **iff** its status is in `statuses` AND it is visible
/// at `now_ns` under `accepted_buffer_ns` (see [`OwnOrder::is_visible`]).
///
/// See the module doc for the clock/unit contract behind the `_ns` names.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct OwnQtyFilter {
    /// Which lifecycle statuses count.
    pub statuses: StatusMask,
    /// How long after a venue accept our size is assumed NOT yet visible in the public feed. `0`
    /// trusts the accept immediately (reduces to the pre-existing single-snapshot filtration).
    pub accepted_buffer_ns: i64,
    /// The event ts the query is asked "as of".
    pub now_ns: i64,
}

impl OwnQtyFilter {
    /// The default filter at `now_ns`: [`StatusMask::RESTING`], zero buffer.
    pub fn at(now_ns: i64) -> Self {
        OwnQtyFilter { statuses: StatusMask::RESTING, accepted_buffer_ns: 0, now_ns }
    }

    /// Builder: set the accepted-buffer.
    pub fn with_accepted_buffer(mut self, accepted_buffer_ns: i64) -> Self {
        self.accepted_buffer_ns = accepted_buffer_ns;
        self
    }

    /// Builder: set the status mask.
    pub fn with_statuses(mut self, statuses: StatusMask) -> Self {
        self.statuses = statuses;
        self
    }

    /// Does `order` count under this filter? The single place the two gates are ANDed.
    fn counts(&self, order: &OwnOrder) -> bool {
        self.statuses.contains(order.status)
            && order.is_visible(self.accepted_buffer_ns, self.now_ns)
    }
}

/// One price level: our orders there, in INSERTION (= venue queue) order.
type Level = IndexMap<String, OwnOrder>;

/// Our own resting-order ladder. See the module doc for the full contract.
#[derive(Clone, Debug)]
pub struct OwnOrderBook {
    /// The venue price grid own-order prices and queried public prices are BOTH quantized on.
    /// Must be positive for the book to match anything (mirrors `vike-mm`'s `book::subtract_own` no-grid
    /// rule: an unknown grid ⇒ inert, never a guess).
    tick_size: f64,
    /// Buy side, keyed by price in ticks (ascending; a best-first walk iterates in REVERSE).
    bids: BTreeMap<i64, Level>,
    /// Sell side, keyed by price in ticks (ascending = best-first).
    asks: BTreeMap<i64, Level>,
    /// `coid -> (side, price_ticks)`, so an event that names only the coid finds its level without
    /// scanning. Insertion-ordered for deterministic iteration if ever walked.
    index: IndexMap<String, (OwnSide, i64)>,
}

impl OwnOrderBook {
    /// A new, empty own book on the venue `tick_size` grid. A non-positive `tick_size` builds an
    /// INERT book: nothing is tracked and every query returns `0.0` (an unknown grid cannot match
    /// a float price to a level — the same rule `vike-mm`'s `book::subtract_own` follows).
    pub fn new(tick_size: f64) -> Self {
        OwnOrderBook {
            tick_size,
            bids: BTreeMap::new(),
            asks: BTreeMap::new(),
            index: IndexMap::new(),
        }
    }

    /// The grid this book quantizes on.
    pub fn tick_size(&self) -> f64 {
        self.tick_size
    }

    /// How many own orders are tracked (all sides, all levels, all statuses).
    pub fn len(&self) -> usize {
        self.index.len()
    }

    /// No own orders tracked.
    pub fn is_empty(&self) -> bool {
        self.index.is_empty()
    }

    /// Is `coid` tracked?
    pub fn contains(&self, coid: &str) -> bool {
        self.index.contains_key(coid)
    }

    /// Read one tracked order, or `None`.
    pub fn get(&self, coid: &str) -> Option<&OwnOrder> {
        let &(side, ticks) = self.index.get(coid)?;
        self.side(side).get(&ticks)?.get(coid)
    }

    /// Drop every tracked order (e.g. after a venue-side mass cancel or a reconcile resync).
    pub fn clear(&mut self) {
        self.bids.clear();
        self.asks.clear();
        self.index.clear();
    }

    // ---- update verbs (the order-lifecycle transitions) ----

    /// SUBMIT: register a new own order at `price` on `side`, status [`OwnStatus::Submitted`]
    /// (hence NOT yet subtractable — it is not in the public book until accepted).
    ///
    /// Returns `false` (and tracks nothing) for a non-positive/non-finite `qty`, a non-finite
    /// `price`, or an inert (no-grid) book. Re-using a live `coid` RE-REGISTERS it from scratch at
    /// the new level, reusing the owned key (no allocation).
    pub fn on_submit(&mut self, coid: &str, side: OwnSide, price: f64, qty: f64, ts: i64) -> bool {
        let Some(ticks) = self.to_ticks(price) else { return false };
        if !qty.is_finite() || qty <= 0.0 {
            return false;
        }
        // a repeat coid re-registers: detach the old entry and reuse its owned key
        let key = match self.detach(coid) {
            Some((key, _)) => key,
            None => coid.to_string(),
        };
        let order =
            OwnOrder { status: OwnStatus::Submitted, qty, ts_submitted: ts, ts_accepted: None };
        self.side_mut(side).entry(ticks).or_default().insert(key, order);
        self.set_index(coid, side, ticks);
        true
    }

    /// ACCEPT: the venue acknowledged `coid` at event ts `ts` — stamp `ts_accepted` (arming the
    /// accepted-buffer) and promote [`OwnStatus::Submitted`] to [`OwnStatus::Accepted`].
    ///
    /// A [`OwnStatus::PendingCancel`] order is NOT demoted back to accepted (a late ack must not
    /// un-request our cancel); only its stamp refreshes. Unknown `coid` ⇒ `false`, no-op.
    pub fn on_accepted(&mut self, coid: &str, ts: i64) -> bool {
        let Some(order) = self.order_mut(coid) else { return false };
        if order.status == OwnStatus::Submitted {
            order.status = OwnStatus::Accepted;
        }
        order.ts_accepted = Some(ts);
        true
    }

    /// PARTIAL FILL: decrement the remaining qty by `filled_qty`. An order decremented to `<= 0`
    /// is REMOVED (a full fill is terminal). Unknown `coid` ⇒ `false`, no-op.
    pub fn on_partial_fill(&mut self, coid: &str, filled_qty: f64) -> bool {
        let Some(order) = self.order_mut(coid) else { return false };
        // naive fold (net-new Rust, no Python twin to mirror a compensated sum from)
        order.qty -= filled_qty;
        let exhausted = order.qty <= 0.0;
        if exhausted {
            self.on_terminal(coid);
        }
        true
    }

    /// CANCEL REQUESTED: mark `coid` [`OwnStatus::PendingCancel`]. It stays in the ladder — the
    /// order is still resting (and still in the public book) until the venue confirms; the
    /// confirmation is [`OwnOrderBook::on_terminal`]. Unknown `coid` ⇒ `false`, no-op.
    pub fn on_cancel_pending(&mut self, coid: &str) -> bool {
        let Some(order) = self.order_mut(coid) else { return false };
        order.status = OwnStatus::PendingCancel;
        true
    }

    /// TERMINAL: the order is gone (canceled / rejected / expired / fully filled) — remove it
    /// entirely, dropping its level when that empties. Unknown `coid` ⇒ `false`, no-op.
    pub fn on_terminal(&mut self, coid: &str) -> bool {
        let detached = self.detach(coid).is_some();
        self.index.shift_remove(coid);
        detached
    }

    /// MODIFY (re-price / re-size in place). Not one of the five core lifecycle verbs — it exists
    /// because the maker re-quotes through `modify_tagged` rather than cancel/replace.
    ///
    /// Semantics, both chosen so the book never OVER-subtracts (which would invent phantom depth):
    /// - **price moved** ⇒ the order leaves its old level and re-enters the new one at the BACK
    ///   (a venue re-price forfeits queue priority), status back to [`OwnStatus::Submitted`] and
    ///   `ts_accepted` cleared — it is not at the new price in the public book until re-acked;
    /// - **qty increased** (same price) ⇒ the level/queue position is kept but `ts_accepted`
    ///   re-arms to `ts`, since the feed has not shown the added size yet;
    /// - **qty reduced** (same price) ⇒ qty updates, stamp untouched (subtracting the smaller new
    ///   size can only under-subtract, which is the safe direction).
    ///
    /// Unknown `coid`, non-finite price, non-positive qty or an inert book ⇒ `false`, no-op.
    pub fn on_modify(&mut self, coid: &str, new_price: f64, new_qty: f64, ts: i64) -> bool {
        let Some(ticks) = self.to_ticks(new_price) else { return false };
        if !new_qty.is_finite() || new_qty <= 0.0 {
            return false;
        }
        let Some(&(side, old_ticks)) = self.index.get(coid) else { return false };
        if old_ticks == ticks {
            let Some(order) = self.order_mut(coid) else { return false };
            let grew = new_qty > order.qty;
            order.qty = new_qty;
            if grew && order.ts_accepted.is_some() {
                order.ts_accepted = Some(ts);
            }
            return true;
        }
        // price moved: detach (reusing the owned key) and re-enter the new level at the back,
        // awaiting a fresh ack at the new price
        let Some((key, _replaced)) = self.detach(coid) else { return false };
        let moved = OwnOrder {
            status: OwnStatus::Submitted,
            qty: new_qty,
            ts_submitted: ts,
            ts_accepted: None,
        };
        self.side_mut(side).entry(ticks).or_default().insert(key, moved);
        self.set_index(coid, side, ticks);
        true
    }

    // ---- queries ----

    /// Our SUBTRACTABLE size resting at `price` on the BID side under `filter` — i.e. how much of
    /// the public bid level at that price is ours. `0.0` when nothing there counts (or the book is
    /// inert).
    pub fn bid_qty_at(&self, price: f64, filter: &OwnQtyFilter) -> f64 {
        self.qty_at(OwnSide::Bid, price, filter)
    }

    /// Our subtractable size resting at `price` on the ASK side — the mirror of
    /// [`OwnOrderBook::bid_qty_at`].
    pub fn ask_qty_at(&self, price: f64, filter: &OwnQtyFilter) -> f64 {
        self.qty_at(OwnSide::Ask, price, filter)
    }

    /// Side-generic form of [`OwnOrderBook::bid_qty_at`]/[`ask_qty_at`](OwnOrderBook::ask_qty_at).
    /// The fold walks the level in INSERTION order, so the f64 sum is deterministic.
    pub fn qty_at(&self, side: OwnSide, price: f64, filter: &OwnQtyFilter) -> f64 {
        let Some(ticks) = self.to_ticks(price) else { return 0.0 };
        let Some(level) = self.side(side).get(&ticks) else { return 0.0 };
        let mut total = 0.0_f64;
        for order in level.values() {
            if filter.counts(order) {
                total += order.qty;
            }
        }
        total
    }

    /// Every own order at `(side, price)` in queue (insertion) order, as `(coid, order)`. Empty
    /// when the level is unknown. Diagnostics/tests — the pricing path uses the qty queries.
    pub fn orders_at(
        &self,
        side: OwnSide,
        price: f64,
    ) -> impl Iterator<Item = (&str, &OwnOrder)> + '_ {
        self.to_ticks(price)
            .and_then(|ticks| self.side(side).get(&ticks))
            .into_iter()
            .flat_map(|level| level.iter().map(|(coid, order)| (coid.as_str(), order)))
    }

    /// Our own ladder on `side` as BEST-FIRST `(price, subtractable_qty)` levels under `filter`
    /// (bids high→low, asks low→high). Levels with no counting size are skipped. Diagnostics and
    /// tests; the pricing path uses [`OwnOrderBook::subtract_levels`].
    pub fn levels(&self, side: OwnSide, filter: &OwnQtyFilter) -> Vec<(f64, f64)> {
        if self.tick_size <= 0.0 {
            return Vec::new();
        }
        let mut out = Vec::new();
        // the BTreeMap walks ticks ascending = best-first for asks; bids reverse below
        for (ticks, level) in self.side(side) {
            let mut total = 0.0_f64;
            for order in level.values() {
                if filter.counts(order) {
                    total += order.qty;
                }
            }
            if total > 0.0 {
                out.push((*ticks as f64 * self.tick_size, total));
            }
        }
        if side == OwnSide::Bid {
            out.reverse();
        }
        out
    }

    /// **The subtracted-depth helper the quote styles price off.** Take one side of a PUBLIC
    /// ladder (best-first `(price, size)` levels) and return it with our own counting size removed:
    /// each level loses [`OwnOrderBook::qty_at`] for its price, and a level whose remainder is
    /// `<= 0` is DROPPED so the derived best shifts to the next genuine market level.
    ///
    /// Byte-identical to `vike-mm`'s `book::subtract_own` arithmetic for the single-own-order case: a level
    /// with no counting own size is passed through VERBATIM (no `x - 0.0` round-trip), the
    /// quantization is the same `round_ties_even` tick match, and the remainder is the same
    /// `level_size - own_size` subtraction. An inert (no-grid) book returns the ladder unchanged.
    pub fn subtract_levels(
        &self,
        side: OwnSide,
        levels: &[crate::BookLevel],
        filter: &OwnQtyFilter,
    ) -> Vec<crate::BookLevel> {
        let mut out = Vec::with_capacity(levels.len());
        for &crate::BookLevel { price, qty: size } in levels {
            let own = self.qty_at(side, price, filter);
            if own <= 0.0 {
                // nothing of ours here — pass the level through untouched (no float op at all)
                out.push(crate::BookLevel::new(price, size));
                continue;
            }
            let remaining = size - own;
            if remaining > 0.0 {
                out.push(crate::BookLevel::new(price, remaining));
            }
            // else: the level was entirely ours — drop it, so the best shifts past our own order
        }
        out
    }

    // ---- internals ----

    /// Quantize a price onto the grid — the SAME `round_ties_even` tick index `vike-mm`'s `book::subtract_own`
    /// and [`L2Book`](crate::L2Book) use, so an L2-derived level price maps back exactly.
    /// `None` for an inert (non-positive) grid or a non-finite price.
    fn to_ticks(&self, price: f64) -> Option<i64> {
        if self.tick_size <= 0.0 || !price.is_finite() {
            return None;
        }
        Some((price / self.tick_size).round_ties_even() as i64)
    }

    fn side(&self, side: OwnSide) -> &BTreeMap<i64, Level> {
        match side {
            OwnSide::Bid => &self.bids,
            OwnSide::Ask => &self.asks,
        }
    }

    fn side_mut(&mut self, side: OwnSide) -> &mut BTreeMap<i64, Level> {
        match side {
            OwnSide::Bid => &mut self.bids,
            OwnSide::Ask => &mut self.asks,
        }
    }

    /// Mutable access to a tracked order via the coid index, or `None`.
    fn order_mut(&mut self, coid: &str) -> Option<&mut OwnOrder> {
        let &(side, ticks) = self.index.get(coid)?;
        self.side_mut(side).get_mut(&ticks)?.get_mut(coid)
    }

    /// Remove `coid` from its LEVEL (dropping the level when it empties) and hand back the owned
    /// key + order so a caller can re-insert without re-allocating the key. The coid INDEX is left
    /// alone — callers either re-point it ([`OwnOrderBook::on_submit`]/`on_modify`) or drop it
    /// ([`OwnOrderBook::on_terminal`]). `shift_remove`, never `swap_remove`: queue order survives a
    /// removal from the middle of a level.
    fn detach(&mut self, coid: &str) -> Option<(String, OwnOrder)> {
        let &(side, ticks) = self.index.get(coid)?;
        let map = self.side_mut(side);
        let level = map.get_mut(&ticks)?;
        let out = level.shift_remove_entry(coid);
        if level.is_empty() {
            map.remove(&ticks);
        }
        out
    }

    /// Point the coid index at `(side, ticks)`, re-using the existing key when the coid is already
    /// tracked (so a re-price allocates nothing).
    fn set_index(&mut self, coid: &str, side: OwnSide, ticks: i64) {
        match self.index.get_mut(coid) {
            Some(slot) => *slot = (side, ticks),
            None => {
                self.index.insert(coid.to_string(), (side, ticks));
            }
        }
    }
}

#[path = "own_book_tests.rs"]
#[cfg(test)]
mod own_book_tests;
