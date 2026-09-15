//! [`OwnOrderBook`] — the maker's OWN resting orders as a per-side price ladder, with
//! **accepted-buffer race handling**. Net-new Rust surface: there is NO Python twin in
//! `vike-trader-app` (the oracle app has no market maker), so nothing here is parity-gated; the
//! contract below IS the specification.
//!
//! ## Why it exists
//!
//! [`SpreadMaker`](crate::SpreadMaker)'s existing own-order filtration
//! ([`with_own_order_filtration`](crate::SpreadMaker::with_own_order_filtration)) subtracts ONE
//! optimistically-remembered `(price, size)` pair per side from the public book. That is enough
//! for a maker that rests exactly one order per side and never races the feed, but it is wrong in
//! two ways the moment either assumption breaks:
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
//! [`SpreadMaker`](crate::SpreadMaker) wire-in feeds the maker's own event ts, which is epoch-
//! MILLIS on the live quote/book/fill lanes — so a maker-side `accepted_buffer_ns` is configured
//! in MILLIS. The only invariant is that `now_ns`, `accepted_buffer_ns` and every `ts_accepted`
//! share a unit.
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
//! - the **maker** drives them itself through [`OwnBookFiltration::place`]/`pull`, keyed by its own
//!   ORDER TAGS (`"bid"`/`"ask"`) — the only ids a `Strategy` ever sees, since the runtime mints
//!   real client-order-ids the strategy is deliberately never shown;
//! - a **mount/runtime** that DOES know the tag↔coid mapping (or drives a wider ladder) can reach
//!   the same book through [`SpreadMaker::own_book_mut`](crate::SpreadMaker::own_book_mut) and feed
//!   REAL venue accepts/cancels, replacing the maker's optimistic stance (see
//!   [`OwnBookFiltration::place`] for what "optimistic" means here).
//!
//! Nothing in this module logs (it runs inside `requote`, on the per-tick path) and nothing here
//! is persisted — the resting-order cache is deliberately excluded from `SpreadMakerStateV1`
//! (`strategy_impl.rs`), being rediscovered from the venue by the runtime's reconcile pass.
//!
//! ## Opt-in
//!
//! A `SpreadMaker` with no own book (`None`, the default) takes the pre-existing code path
//! verbatim — this whole module is inert unless
//! [`with_own_order_book`](crate::SpreadMaker::with_own_order_book) is called.

use std::collections::BTreeMap;

use indexmap::IndexMap;

use crate::book::BookView;

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
    /// maker's [`Fill`](vike_model::Fill) and `submit_limit_tagged` calls already use.
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

/// The maker's own resting-order ladder. See the module doc for the full contract.
#[derive(Clone, Debug)]
pub struct OwnOrderBook {
    /// The venue price grid own-order prices and queried public prices are BOTH quantized on.
    /// Must be positive for the book to match anything (mirrors `book::subtract_own`'s no-grid
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
    /// a float price to a level — the same rule `book::subtract_own` already follows).
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
    /// Byte-identical to `book::subtract_own`'s arithmetic for the single-own-order case: a level
    /// with no counting own size is passed through VERBATIM (no `x - 0.0` round-trip), the
    /// quantization is the same `round_ties_even` tick match, and the remainder is the same
    /// `level_size - own_size` subtraction. An inert (no-grid) book returns the ladder unchanged.
    pub fn subtract_levels(
        &self,
        side: OwnSide,
        levels: &[(f64, f64)],
        filter: &OwnQtyFilter,
    ) -> Vec<(f64, f64)> {
        let mut out = Vec::with_capacity(levels.len());
        for &(price, size) in levels {
            let own = self.qty_at(side, price, filter);
            if own <= 0.0 {
                // nothing of ours here — pass the level through untouched (no float op at all)
                out.push((price, size));
                continue;
            }
            let remaining = size - own;
            if remaining > 0.0 {
                out.push((price, remaining));
            }
            // else: the level was entirely ours — drop it, so the best shifts past our own order
        }
        out
    }

    /// [`OwnOrderBook::subtract_levels`] applied to BOTH sides of a [`BookView`] — the exact input
    /// the [`QuoteStyle`](vike_model::QuoteStyle) pricing consumes. Crate-private because
    /// `BookView` is.
    pub(crate) fn subtracted_view(&self, book: &BookView, filter: &OwnQtyFilter) -> BookView {
        BookView {
            tick_size: book.tick_size,
            bids: self.subtract_levels(OwnSide::Bid, &book.bids, filter),
            asks: self.subtract_levels(OwnSide::Ask, &book.asks, filter),
        }
    }

    // ---- internals ----

    /// Quantize a price onto the grid — the SAME `round_ties_even` tick index `book::subtract_own`
    /// and [`L2Book`](vike_model::L2Book) use, so an L2-derived level price maps back exactly.
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

/// The maker-side bundle: a live [`OwnOrderBook`] plus the two query knobs
/// ([`StatusMask`] + accepted-buffer) [`SpreadMaker`](crate::SpreadMaker) filters its public book
/// with. This is what
/// [`SpreadMaker::with_own_order_book`](crate::SpreadMaker::with_own_order_book) takes, and what
/// [`SpreadMaker::own_book_mut`](crate::SpreadMaker::own_book_mut) hands a runtime that wants to
/// drive REAL venue lifecycle events into the book.
#[derive(Clone, Debug)]
pub struct OwnBookFiltration {
    /// The ladder itself — drive its update verbs directly for real venue events.
    pub book: OwnOrderBook,
    /// Which statuses count toward subtracted depth. Default [`StatusMask::RESTING`].
    pub statuses: StatusMask,
    /// The modeled accept→public-feed delay (see [`OwnOrder::is_visible`] and the module doc's
    /// unit contract; the maker's clock is epoch-MILLIS on the live lanes). `0` ⇒ trust the accept
    /// immediately, which makes this path reduce to the pre-existing snapshot filtration.
    pub accepted_buffer_ns: i64,
}

impl OwnBookFiltration {
    /// A filtration bundle on the venue `tick_size` grid with an `accepted_buffer_ns` race window
    /// and the default [`StatusMask::RESTING`] mask.
    ///
    /// `tick_size` must be the grid the maker actually quotes on — the L1 quote lane's configured
    /// `tick_size` ([`SpreadMakerParams::tick_size`](vike_model::SpreadMakerParams::tick_size)), or the venue grid the L2
    /// [`L2Book`](vike_model::L2Book) carries. A non-positive grid builds an inert book (filtration
    /// then subtracts nothing, exactly like the existing no-grid rule).
    pub fn new(tick_size: f64, accepted_buffer_ns: i64) -> Self {
        OwnBookFiltration {
            book: OwnOrderBook::new(tick_size),
            statuses: StatusMask::RESTING,
            accepted_buffer_ns,
        }
    }

    /// Builder: choose which statuses count (default [`StatusMask::RESTING`]).
    pub fn with_statuses(mut self, statuses: StatusMask) -> Self {
        self.statuses = statuses;
        self
    }

    /// The [`OwnQtyFilter`] this bundle's knobs make, as of event ts `now`.
    pub fn filter_at(&self, now: i64) -> OwnQtyFilter {
        OwnQtyFilter {
            statuses: self.statuses,
            accepted_buffer_ns: self.accepted_buffer_ns,
            now_ns: now,
        }
    }

    /// PLACE (submit-or-re-price) the maker's tagged order, with an **optimistic ack**.
    ///
    /// A `Strategy` never sees its client-order-ids (the runtime mints them) and never sees the
    /// venue's accept for them, so the maker keys the book by its own tag (`"bid"`/`"ask"`) and
    /// stamps `ts_accepted` itself at the placing event ts. `accepted_buffer_ns` then models the
    /// ack + market-data propagation delay before that order shows up in the public feed — which
    /// is exactly the race this book exists to handle, just measured from send instead of ack.
    /// With a `0` buffer this reduces to the pre-existing "subtract my last known quote" behavior.
    ///
    /// A runtime that DOES know the tag↔coid mapping should instead drive the real
    /// [`OwnOrderBook::on_submit`]/[`on_accepted`](OwnOrderBook::on_accepted) through
    /// [`SpreadMaker::own_book_mut`](crate::SpreadMaker::own_book_mut); the stamp then reflects the
    /// venue's actual ack and the buffer covers propagation alone.
    pub(crate) fn place(&mut self, coid: &str, side: OwnSide, price: f64, qty: f64, ts: i64) {
        if self.book.contains(coid) {
            self.book.on_modify(coid, price, qty, ts);
        } else {
            self.book.on_submit(coid, side, price, qty, ts);
        }
        // optimistic ack: only for an order with no stamp yet (a fresh submit, or one whose
        // re-price cleared it) — a resting order keeps its original stamp, so the buffer does not
        // re-arm on every no-move re-quote.
        if self.book.get(coid).is_some_and(|order| order.ts_accepted.is_none()) {
            self.book.on_accepted(coid, ts);
        }
    }

    /// PULL: the maker canceled its tagged order — drop it from the ladder. (The maker's own
    /// cancels are the suppression pulls, which it treats as immediately gone; a runtime tracking
    /// real venue state would use [`OwnOrderBook::on_cancel_pending`] and only remove on the
    /// venue's confirmation.)
    pub(crate) fn pull(&mut self, coid: &str) {
        self.book.on_terminal(coid);
    }

    /// Own-filter a public [`BookView`] as of event ts `now` — the call the maker's `requote`
    /// makes in place of `book::filter_own`.
    pub(crate) fn filtered_view(&self, book: &BookView, now: i64) -> BookView {
        self.book.subtracted_view(book, &self.filter_at(now))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A book on a 0.5 grid with one accepted bid — the fixture most gate tests start from.
    /// `coid` rests at `price`/`qty`, submitted at ts 10 and accepted at ts 20.
    fn accepted_bid(coid: &str, price: f64, qty: f64) -> OwnOrderBook {
        let mut b = OwnOrderBook::new(0.5);
        assert!(b.on_submit(coid, OwnSide::Bid, price, qty, 10), "submit tracked");
        assert!(b.on_accepted(coid, 20), "accept stamped");
        b
    }

    // THE RACE: an order the venue just accepted is NOT yet in the public feed, so inside the
    // accepted-buffer it must not be subtracted — otherwise filtration removes depth the feed
    // never added (phantom-empty level).
    #[test]
    fn accepted_but_not_yet_public_is_excluded_by_the_buffer() {
        let b = accepted_bid("c1", 100.0, 3.0);
        // accepted at 20, buffer 1000 ⇒ not public until 1020
        let f = OwnQtyFilter::at(500).with_accepted_buffer(1_000);
        assert_eq!(b.bid_qty_at(100.0, &f).to_bits(), 0.0_f64.to_bits(), "inside the buffer ⇒ 0");
        // one tick BEFORE the deadline is still inside
        let f = OwnQtyFilter::at(1_019).with_accepted_buffer(1_000);
        assert_eq!(b.bid_qty_at(100.0, &f).to_bits(), 0.0_f64.to_bits(), "1019 < 1020 ⇒ still 0");
        // and the public ladder is therefore untouched — no phantom depth removed
        let levels = b.subtract_levels(OwnSide::Bid, &[(100.0, 3.0), (99.5, 8.0)], &f);
        assert_eq!(levels, vec![(100.0, 3.0), (99.5, 8.0)], "public depth is left alone");
    }

    // Once the buffer has elapsed the order IS assumed public and counts — and the level it wholly
    // owns is dropped from the public ladder, shifting the derived best to the genuine next level.
    #[test]
    fn buffer_elapsed_includes_the_order_and_shifts_the_best() {
        let b = accepted_bid("c1", 100.0, 3.0);
        let f = OwnQtyFilter::at(1_020).with_accepted_buffer(1_000);
        assert_eq!(
            b.bid_qty_at(100.0, &f).to_bits(),
            3.0_f64.to_bits(),
            "at the deadline ⇒ counts"
        );
        let later = OwnQtyFilter::at(9_999).with_accepted_buffer(1_000);
        assert_eq!(b.bid_qty_at(100.0, &later).to_bits(), 3.0_f64.to_bits(), "and stays counted");
        // whole level ours ⇒ dropped; a level we only partly own ⇒ reduced
        let levels = b.subtract_levels(OwnSide::Bid, &[(100.0, 3.0), (99.5, 8.0)], &f);
        assert_eq!(levels, vec![(99.5, 8.0)], "our whole level is removed");
        let levels = b.subtract_levels(OwnSide::Bid, &[(100.0, 10.0)], &f);
        assert_eq!(levels, vec![(100.0, 7.0)], "partly ours ⇒ only our size subtracted");
        // a zero buffer trusts the accept immediately (the pre-existing snapshot behavior)
        let now0 = OwnQtyFilter::at(20);
        assert_eq!(b.bid_qty_at(100.0, &now0).to_bits(), 3.0_f64.to_bits(), "0 buffer ⇒ at once");
    }

    // A never-accepted (Submitted) order is structurally invisible: it is not in the public book,
    // so no mask can make it subtractable.
    #[test]
    fn submitted_but_unaccepted_never_counts() {
        let mut b = OwnOrderBook::new(0.5);
        b.on_submit("c1", OwnSide::Bid, 100.0, 3.0, 10);
        for f in [
            OwnQtyFilter::at(1_000_000),
            OwnQtyFilter::at(1_000_000).with_statuses(StatusMask::ALL),
        ] {
            assert_eq!(b.bid_qty_at(100.0, &f).to_bits(), 0.0_f64.to_bits(), "unaccepted ⇒ 0");
        }
        assert_eq!(b.get("c1").map(|o| o.status), Some(OwnStatus::Submitted), "still Submitted");
        assert_eq!(b.get("c1").and_then(|o| o.ts_accepted), None, "and unstamped");
    }

    // A partial fill decrements the REMAINING size (so filtration subtracts less), and a decrement
    // to zero-or-below removes the order outright (a full fill is terminal).
    #[test]
    fn partial_fill_decrements_then_removes_when_exhausted() {
        let mut b = accepted_bid("c1", 100.0, 5.0);
        let f = OwnQtyFilter::at(100);
        assert_eq!(b.bid_qty_at(100.0, &f).to_bits(), 5.0_f64.to_bits(), "starts at 5");

        assert!(b.on_partial_fill("c1", 2.0), "known coid");
        assert_eq!(b.bid_qty_at(100.0, &f).to_bits(), 3.0_f64.to_bits(), "5 − 2 = 3 remaining");
        assert_eq!(b.len(), 1, "still tracked");
        // the accept stamp survives a partial — the residual never left the book
        assert_eq!(b.get("c1").and_then(|o| o.ts_accepted), Some(20), "stamp unchanged");

        assert!(b.on_partial_fill("c1", 3.0), "the fill that exhausts it");
        assert_eq!(b.bid_qty_at(100.0, &f).to_bits(), 0.0_f64.to_bits(), "nothing left");
        assert!(b.is_empty(), "an exhausted order is removed");
        assert!(!b.contains("c1"), "and its index entry with it");
        assert!(b.levels(OwnSide::Bid, &f).is_empty(), "its level is gone too");
        // an over-fill (venue rounding) also just removes it, never leaves negative size
        let mut b2 = accepted_bid("c2", 100.0, 5.0);
        assert!(b2.on_partial_fill("c2", 7.5));
        assert!(b2.is_empty(), "an over-fill removes the order");
        assert!(!b2.on_partial_fill("unknown", 1.0), "unknown coid is a no-op");
    }

    // A pending cancel is still RESTING at the venue, so it counts by default; the
    // ACCEPTED_ONLY mask is the opt-out for a maker that wants to lean on depth it is reclaiming.
    #[test]
    fn pending_cancel_counts_by_default_and_is_droppable_by_mask() {
        let mut b = accepted_bid("c1", 100.0, 4.0);
        assert!(b.on_cancel_pending("c1"), "known coid");
        assert_eq!(b.get("c1").map(|o| o.status), Some(OwnStatus::PendingCancel));

        let default = OwnQtyFilter::at(100);
        assert_eq!(default.statuses, StatusMask::RESTING, "RESTING is the default mask");
        assert_eq!(b.bid_qty_at(100.0, &default).to_bits(), 4.0_f64.to_bits(), "still counted");

        let accepted_only = OwnQtyFilter::at(100).with_statuses(StatusMask::ACCEPTED_ONLY);
        assert_eq!(b.bid_qty_at(100.0, &accepted_only).to_bits(), 0.0_f64.to_bits(), "masked out");
        // and it shows through to the ladder: masked out ⇒ the public level is left intact
        assert_eq!(
            b.subtract_levels(OwnSide::Bid, &[(100.0, 4.0)], &accepted_only),
            vec![(100.0, 4.0)],
            "masked-out size is not subtracted"
        );
        assert!(
            b.subtract_levels(OwnSide::Bid, &[(100.0, 4.0)], &default).is_empty(),
            "counted size still removes the level"
        );
        // a late ack must NOT un-request the cancel
        assert!(b.on_accepted("c1", 50), "stamp refreshes");
        assert_eq!(b.get("c1").map(|o| o.status), Some(OwnStatus::PendingCancel), "stays pending");
        assert!(!b.on_cancel_pending("unknown"), "unknown coid is a no-op");
    }

    // Terminal removes the order, its index entry and (when it empties) its level — and leaves
    // every OTHER order at that level untouched.
    #[test]
    fn terminal_removes_the_order_and_empty_levels() {
        let mut b = accepted_bid("c1", 100.0, 2.0);
        b.on_submit("c2", OwnSide::Bid, 100.0, 3.0, 11);
        b.on_accepted("c2", 21);
        b.on_submit("c3", OwnSide::Ask, 101.0, 4.0, 12);
        b.on_accepted("c3", 22);
        let f = OwnQtyFilter::at(100);
        assert_eq!(b.bid_qty_at(100.0, &f).to_bits(), 5.0_f64.to_bits(), "2 + 3 at the level");

        assert!(b.on_terminal("c1"), "known coid");
        assert_eq!(b.len(), 2, "only c1 left the book");
        assert!(!b.contains("c1"), "index entry gone");
        assert_eq!(b.bid_qty_at(100.0, &f).to_bits(), 3.0_f64.to_bits(), "c2 still rests there");

        assert!(b.on_terminal("c2"), "the last order at the level");
        assert_eq!(b.bid_qty_at(100.0, &f).to_bits(), 0.0_f64.to_bits(), "level now empty");
        assert!(b.levels(OwnSide::Bid, &f).is_empty(), "the empty level was dropped");
        assert_eq!(b.ask_qty_at(101.0, &f).to_bits(), 4.0_f64.to_bits(), "the ask side is intact");
        assert!(!b.on_terminal("c1"), "a second terminal is a no-op");
    }

    // Queue order is INSERTION order and survives a removal from the MIDDLE of a level
    // (`shift_remove`, never `swap_remove`) — and a re-registered coid goes to the BACK, which is
    // what a venue does to a re-priced order.
    #[test]
    fn level_iteration_is_deterministic_insertion_order() {
        let mut b = OwnOrderBook::new(0.5);
        for (i, coid) in ["a", "b", "c", "d"].iter().enumerate() {
            b.on_submit(coid, OwnSide::Bid, 100.0, 1.0 + i as f64, 10 + i as i64);
            b.on_accepted(coid, 20 + i as i64);
        }
        let coids: Vec<&str> = b.orders_at(OwnSide::Bid, 100.0).map(|(c, _)| c).collect();
        assert_eq!(coids, vec!["a", "b", "c", "d"], "insertion order preserved");

        // remove from the MIDDLE: a swap-remove would move "d" into b's slot
        b.on_terminal("b");
        let coids: Vec<&str> = b.orders_at(OwnSide::Bid, 100.0).map(|(c, _)| c).collect();
        assert_eq!(coids, vec!["a", "c", "d"], "middle removal keeps the remaining queue order");

        // re-registering an existing coid puts it at the BACK (queue priority is forfeited)
        b.on_submit("a", OwnSide::Bid, 100.0, 9.0, 30);
        let coids: Vec<&str> = b.orders_at(OwnSide::Bid, 100.0).map(|(c, _)| c).collect();
        assert_eq!(coids, vec!["c", "d", "a"], "a re-registered order re-enters at the back");
        assert_eq!(b.len(), 3, "still three orders — re-register did not duplicate");

        // the summed qty is a deterministic insertion-order fold, repeatable across calls
        let f = OwnQtyFilter::at(1_000).with_statuses(StatusMask::ALL);
        // c(3) + d(4) count; "a" was re-submitted (unaccepted) so it is not visible
        let first = b.bid_qty_at(100.0, &f);
        assert_eq!(first.to_bits(), 7.0_f64.to_bits(), "3 + 4 in queue order");
        assert_eq!(b.bid_qty_at(100.0, &f).to_bits(), first.to_bits(), "repeatable, bit-for-bit");

        // multi-LEVEL order is price-ordered, best-first, on both sides
        let mut m = OwnOrderBook::new(0.5);
        for (coid, side, px) in [
            ("b1", OwnSide::Bid, 99.5),
            ("b2", OwnSide::Bid, 100.0),
            ("a1", OwnSide::Ask, 101.0),
            ("a2", OwnSide::Ask, 100.5),
        ] {
            m.on_submit(coid, side, px, 1.0, 10);
            m.on_accepted(coid, 10);
        }
        let g = OwnQtyFilter::at(100);
        assert_eq!(m.levels(OwnSide::Bid, &g), vec![(100.0, 1.0), (99.5, 1.0)], "bids high→low");
        assert_eq!(m.levels(OwnSide::Ask, &g), vec![(100.5, 1.0), (101.0, 1.0)], "asks low→high");
    }

    // Prices match a level by TICK (the same round_ties_even quantization the public book uses), so
    // a float that lands on the same tick still matches; and an unknown grid is fully inert.
    #[test]
    fn tick_matching_and_the_inert_no_grid_book() {
        let b = accepted_bid("c1", 100.0, 3.0);
        let f = OwnQtyFilter::at(100);
        // 100.0 and a float a hair off it quantize to the same tick on a 0.5 grid
        assert_eq!(b.bid_qty_at(100.0 + 1e-12, &f).to_bits(), 3.0_f64.to_bits(), "same tick");
        assert_eq!(b.bid_qty_at(99.5, &f).to_bits(), 0.0_f64.to_bits(), "a different tick ⇒ 0");
        assert_eq!(b.bid_qty_at(f64::NAN, &f).to_bits(), 0.0_f64.to_bits(), "non-finite ⇒ 0");
        assert_eq!(b.ask_qty_at(100.0, &f).to_bits(), 0.0_f64.to_bits(), "the other side ⇒ 0");

        // no grid ⇒ nothing is tracked and nothing is ever subtracted
        let mut inert = OwnOrderBook::new(0.0);
        assert!(!inert.on_submit("c1", OwnSide::Bid, 100.0, 3.0, 10), "no grid ⇒ not tracked");
        assert!(inert.is_empty(), "inert book stays empty");
        assert!(!inert.on_accepted("c1", 20), "and its later events are no-ops");
        assert_eq!(inert.bid_qty_at(100.0, &f).to_bits(), 0.0_f64.to_bits(), "queries are 0");
        assert_eq!(
            inert.subtract_levels(OwnSide::Bid, &[(100.0, 3.0)], &f),
            vec![(100.0, 3.0)],
            "the public ladder is returned unchanged"
        );
        // a nonsense submit is refused without corrupting the book
        let mut b2 = OwnOrderBook::new(0.5);
        assert!(!b2.on_submit("x", OwnSide::Bid, 100.0, 0.0, 1), "zero qty refused");
        assert!(!b2.on_submit("x", OwnSide::Bid, f64::INFINITY, 1.0, 1), "non-finite px refused");
        assert!(b2.is_empty(), "nothing tracked");
    }

    // on_modify: a price move re-arms the race gate (back of the new level, awaiting a fresh ack);
    // a qty INCREASE re-arms it too (the feed has not shown the added size); a qty REDUCTION keeps
    // the stamp (subtracting less can only under-subtract, the safe direction).
    #[test]
    fn modify_repricing_rearms_the_buffer_and_resize_rules_hold() {
        let mut b = accepted_bid("c1", 100.0, 3.0);
        let f = |now: i64| OwnQtyFilter::at(now).with_accepted_buffer(100);
        assert_eq!(
            b.bid_qty_at(100.0, &f(1_000)).to_bits(),
            3.0_f64.to_bits(),
            "resting + visible"
        );

        // re-price to 99.5 at ts 1000: gone from the old level, and NOT yet visible at the new one
        assert!(b.on_modify("c1", 99.5, 3.0, 1_000), "known coid");
        assert_eq!(b.bid_qty_at(100.0, &f(1_000)).to_bits(), 0.0_f64.to_bits(), "old level empty");
        assert_eq!(b.bid_qty_at(99.5, &f(1_000)).to_bits(), 0.0_f64.to_bits(), "new one unacked");
        assert_eq!(b.get("c1").map(|o| o.status), Some(OwnStatus::Submitted), "awaiting re-ack");
        assert_eq!(b.len(), 1, "still exactly one order");
        // once re-acked and past the buffer it counts at the NEW price
        b.on_accepted("c1", 1_010);
        assert_eq!(b.bid_qty_at(99.5, &f(1_110)).to_bits(), 3.0_f64.to_bits(), "visible again");

        // qty INCREASE re-arms the buffer from the modify ts
        assert!(b.on_modify("c1", 99.5, 5.0, 1_200), "size up");
        assert_eq!(b.get("c1").and_then(|o| o.ts_accepted), Some(1_200), "stamp re-armed");
        assert_eq!(b.bid_qty_at(99.5, &f(1_250)).to_bits(), 0.0_f64.to_bits(), "inside the buffer");
        assert_eq!(b.bid_qty_at(99.5, &f(1_300)).to_bits(), 5.0_f64.to_bits(), "then the new size");
        // qty REDUCTION keeps the stamp — no re-arm, subtract the smaller size at once
        assert!(b.on_modify("c1", 99.5, 2.0, 1_400), "size down");
        assert_eq!(b.get("c1").and_then(|o| o.ts_accepted), Some(1_200), "stamp untouched");
        assert_eq!(b.bid_qty_at(99.5, &f(1_400)).to_bits(), 2.0_f64.to_bits(), "smaller size");
        assert!(!b.on_modify("unknown", 99.5, 1.0, 1_500), "unknown coid is a no-op");
    }

    // Two own orders at the SAME level sum — the case a single `(price, size)` snapshot cannot
    // represent at all, and the reason this structure exists.
    #[test]
    fn multiple_own_orders_at_one_level_sum() {
        let mut b = accepted_bid("c1", 100.0, 2.0);
        b.on_submit("c2", OwnSide::Bid, 100.0, 3.0, 11);
        b.on_accepted("c2", 21);
        let f = OwnQtyFilter::at(100);
        assert_eq!(b.bid_qty_at(100.0, &f).to_bits(), 5.0_f64.to_bits(), "2 + 3 subtractable");
        // a public level of 12 with 5 ours leaves 7; with only c1 visible it would leave 10
        assert_eq!(
            b.subtract_levels(OwnSide::Bid, &[(100.0, 12.0)], &f),
            vec![(100.0, 7.0)],
            "BOTH own orders are subtracted, not just the last one"
        );
        // and the buffer gates them INDEPENDENTLY: at ts 20 only c1 (accepted at 20) is past a
        // zero-buffer gate; c2 (accepted at 21) is not yet accepted at all
        let early = OwnQtyFilter::at(20);
        assert_eq!(b.bid_qty_at(100.0, &early).to_bits(), 2.0_f64.to_bits(), "only c1 yet");
    }
}

/// The WIRE-IN tests: the ladder driven end-to-end through a real [`SpreadMaker`] on the L2 book
/// lane, against a concrete `LiveBroker`. Kept here (with local helper copies) rather than in
/// `lib.rs`'s test module — the same split `strategy_impl.rs` already uses for its own white-box
/// tests.
#[cfg(test)]
mod wire_in_tests {
    use vike_core::LiveBroker;
    use vike_model::{L2Book, QuoteStyle, Strategy};

    use crate::{OwnSide, SpreadMaker};

    /// A bare in-crate `LiveBroker` at a given EVENT ts — mirrors `lib.rs`/`strategy_impl.rs`'s own
    /// test helpers of the same shape (this module can't reach those private copies).
    fn broker(now: i64) -> LiveBroker {
        LiveBroker {
            positions: Vec::new(),
            prices: Vec::new(),
            bar_views: Vec::new(),
            position: 0.0,
            price: 0.0,
            equity: 0.0,
            bars: std::sync::Arc::new(Vec::new()),
            index: 0,
            now,
            multiplier: 1.0,
            lot_size: 0.0,
            submissions: Vec::new(),
            modifications: Vec::new(),
            cancels: Vec::new(),
            brackets: Vec::new(),
            conditionals: Vec::new(),
            mass_cancel: false,
        }
    }

    /// The price the tagged order was submitted at.
    fn submit_px(b: &LiveBroker, tag: &str) -> f64 {
        let s = b.submissions.iter().find(|s| s.tag.as_deref() == Some(tag)).expect("a submit");
        s.price.expect("limit submit has a price")
    }

    /// The price the tagged order was re-priced to.
    fn modify_px(b: &LiveBroker, tag: &str) -> f64 {
        let m = b.modifications.iter().find(|m| m.tag == tag).expect("a modify");
        m.new_price.expect("re-price has a price")
    }

    /// Every buffered verb as a comparable `(kind, tag, price, qty)` tape — the equivalence probe.
    fn tape(b: &LiveBroker) -> Vec<(&'static str, String, Option<u64>, Option<u64>)> {
        let submits = b.submissions.iter().map(|s| {
            (
                "submit",
                s.tag.clone().unwrap_or_default(),
                s.price.map(f64::to_bits),
                Some(s.qty.to_bits()),
            )
        });
        let modifies = b.modifications.iter().map(|m| {
            ("modify", m.tag.clone(), m.new_price.map(f64::to_bits), m.new_qty.map(f64::to_bits))
        });
        submits.chain(modifies).collect()
    }

    /// bids `[(100.0, 5.0), (99.5, 8.0)]` / asks `[(100.5, 5.0), (101.0, 8.0)]` on a 0.5 grid —
    /// the top level is exactly our quote size, so once we rest there we ARE the whole best level.
    fn l2() -> L2Book {
        let mut book = L2Book::new(0.5);
        book.apply_snapshot(1, &[(100.0, 5.0), (99.5, 8.0)], &[(100.5, 5.0), (101.0, 8.0)]);
        book
    }

    /// A `Join` maker quoting exactly the top level's size, with the ladder on at `buffer`.
    fn ladder_maker(buffer: i64) -> SpreadMaker {
        SpreadMaker::new(5.0, 0.0)
            .with_quote_style(QuoteStyle::Join, 0, 0.0)
            .with_own_order_book(0.5, buffer)
    }

    // THE RACE, end to end. Inside the accepted-buffer our just-placed order is NOT yet echoed by
    // the public feed, so filtration must leave the displayed depth alone; once the buffer elapses
    // it subtracts and the derived best shifts to the genuine next level. The contrast maker (same
    // config, ZERO buffer) shifts immediately — proving the buffer is what makes the difference.
    #[test]
    fn accepted_buffer_holds_the_public_book_then_subtracts_after_it() {
        let book = l2();
        let mut mm = ladder_maker(1_000);

        // tick 1 @ ts 0: nothing of ours rests yet → Join at the touch
        let mut b0 = broker(0);
        mm.on_order_book(&mut b0, &book);
        assert_eq!(submit_px(&b0, "bid").to_bits(), 100.0_f64.to_bits(), "first quote joins touch");
        assert_eq!(submit_px(&b0, "ask").to_bits(), 100.5_f64.to_bits(), "ask joins the touch too");

        // tick 2 @ ts 500 — INSIDE the buffer (accepted at 0, public at 1000). Our size is not in
        // the feed yet, so subtracting it would invent a phantom-empty level: the maker must still
        // see the full public book and re-quote at the SAME touch.
        let mut b1 = broker(500);
        mm.on_order_book(&mut b1, &book);
        assert_eq!(
            modify_px(&b1, "bid").to_bits(),
            100.0_f64.to_bits(),
            "inside the buffer: public depth is NOT double-subtracted"
        );
        assert_eq!(modify_px(&b1, "ask").to_bits(), 100.5_f64.to_bits(), "same on the ask side");

        // tick 3 @ ts 1000 — the buffer has elapsed, so our order IS assumed public: filtration
        // removes the level we wholly own and Join re-prices onto the genuine next level.
        let mut b2 = broker(1_000);
        mm.on_order_book(&mut b2, &book);
        assert_eq!(
            modify_px(&b2, "bid").to_bits(),
            99.5_f64.to_bits(),
            "buffer elapsed: our own level is subtracted, best shifts"
        );
        assert_eq!(modify_px(&b2, "ask").to_bits(), 101.0_f64.to_bits(), "ask shifts out too");

        // CONTRAST: the identical maker with a ZERO buffer shifts at the very next tick instead.
        let mut zero = ladder_maker(0);
        zero.on_order_book(&mut broker(0), &book);
        let mut z1 = broker(500);
        zero.on_order_book(&mut z1, &book);
        assert_eq!(
            modify_px(&z1, "bid").to_bits(),
            99.5_f64.to_bits(),
            "zero buffer trusts the ack at once — the buffer alone caused the hold above"
        );
    }

    // A zero-buffer ladder must reproduce the pre-existing single-snapshot filtration EXACTLY: the
    // ladder is a strictly richer representation, not a different policy. Bit-for-bit over the
    // whole buffered verb tape, across a multi-tick run that moves our order between levels.
    #[test]
    fn zero_buffer_ladder_matches_the_snapshot_path_bit_for_bit() {
        let book = l2();
        let mut ladder = ladder_maker(0);
        let mut snapshot = SpreadMaker::new(5.0, 0.0)
            .with_quote_style(QuoteStyle::Join, 0, 0.0)
            .with_own_order_filtration();

        for ts in [10, 20, 30, 40] {
            let (mut a, mut b) = (broker(ts), broker(ts));
            ladder.on_order_book(&mut a, &book);
            snapshot.on_order_book(&mut b, &book);
            assert_eq!(tape(&a), tape(&b), "tick {ts}: ladder and snapshot agree bit-for-bit");
        }
    }

    // The ladder is OFF by default and the snapshot opt-in does NOT turn it on — so both
    // pre-existing configurations keep their exact prior code path.
    #[test]
    fn ladder_is_off_by_default_and_snapshot_filtration_does_not_enable_it() {
        let plain = SpreadMaker::new(1.0, 0.5);
        assert!(plain.own_book().is_none(), "no ladder by default");
        assert!(!plain.params().filter_own, "and filtration is off by default");

        let snapshot = SpreadMaker::new(1.0, 0.5).with_own_order_filtration();
        assert!(snapshot.own_book().is_none(), "snapshot filtration uses no ladder");
        assert!(snapshot.params().filter_own, "but filtration is on");

        // the ladder builder implies filtration — one call is the whole opt-in
        let ladder = ladder_maker(100);
        assert!(ladder.own_book().is_some(), "ladder mounted");
        assert!(ladder.params().filter_own, "and filtration turned on with it");
    }

    // A runtime driving a REAL venue partial fill through `own_book_mut` shrinks what filtration
    // subtracts, so the maker stops treating a level it only partly owns as wholly its own.
    #[test]
    fn runtime_driven_partial_fill_shrinks_the_subtraction() {
        let book = l2();
        let mut mm = ladder_maker(0);
        mm.on_order_book(&mut broker(0), &book);
        assert_eq!(
            mm.own_book().and_then(|f| f.book.get("bid")).map(|o| o.qty.to_bits()),
            Some(5.0_f64.to_bits()),
            "the maker's own bid is tracked at full size"
        );

        // the venue partially fills 2.0 of our 5.0 — a runtime folds it into the ladder
        assert!(
            mm.own_book_mut().expect("ladder mounted").book.on_partial_fill("bid", 2.0),
            "the tagged order is known to the ladder"
        );

        // now only 3.0 of the 5.0 top level is ours, so 2.0 of genuine market depth remains: the
        // level SURVIVES filtration and Join keeps quoting the touch (without the fill it would
        // have been wholly ours and dropped, shifting the quote to 99.5).
        let mut b1 = broker(10);
        mm.on_order_book(&mut b1, &book);
        assert_eq!(
            modify_px(&b1, "bid").to_bits(),
            100.0_f64.to_bits(),
            "partial fill leaves real depth at the level, so the best does not shift"
        );
    }

    // A ladder built on an unknown (non-positive) grid is INERT — it tracks nothing and subtracts
    // nothing, so the maker prices straight off the feed. The same no-grid rule the snapshot path
    // already follows, and the documented footgun of the ladder's fixed grid.
    #[test]
    fn inert_no_grid_ladder_subtracts_nothing() {
        let book = l2();
        let mut mm = SpreadMaker::new(5.0, 0.0)
            .with_quote_style(QuoteStyle::Join, 0, 0.0)
            .with_own_order_book(0.0, 0);

        mm.on_order_book(&mut broker(0), &book);
        assert!(mm.own_book().expect("mounted").book.is_empty(), "no grid ⇒ nothing tracked");

        let mut b1 = broker(10);
        mm.on_order_book(&mut b1, &book);
        assert_eq!(
            modify_px(&b1, "bid").to_bits(),
            100.0_f64.to_bits(),
            "an inert ladder never subtracts — the maker keeps joining the raw touch"
        );
    }

    // A suppression PULL removes the quote from the ladder, so the depth we no longer have resting
    // stops being subtracted from the public book.
    #[test]
    fn a_pulled_quote_leaves_the_ladder() {
        let book = l2();
        // breaker armed so a single 5.0 bid fill trips the bid side and pulls that quote
        let mut mm = SpreadMaker::new(5.0, 0.0)
            .with_quote_style(QuoteStyle::Join, 0, 0.0)
            .with_own_order_book(0.5, 0)
            .with_fill_breaker(1_000, 2.5, 5_000);

        mm.on_order_book(&mut broker(0), &book);
        assert!(mm.own_book().expect("mounted").book.contains("bid"), "bid rests in the ladder");

        // a big same-side fill trips the bid breaker; the next tick pulls that side
        let fill = vike_model::Fill {
            side: 1,
            size: 5.0,
            price: 100.0,
            fee: 0.0,
            ts: 10,
            is_maker: true,
            symbol: String::new(),
        };
        Strategy::<LiveBroker>::on_fill(&mut mm, &mut broker(10), &fill);

        let mut b1 = broker(20);
        mm.on_order_book(&mut b1, &book);
        assert!(b1.cancels.iter().any(|t| t == "bid"), "the suppressed side is canceled");
        assert!(
            !mm.own_book().expect("mounted").book.contains("bid"),
            "and it leaves the ladder, so its size is no longer subtracted"
        );
        // the ask side is untouched by the bid-side pull — it still rests and is still tracked.
        // NOTE it has RE-PRICED 100.5 → 101.0: unsuppressed, it re-quoted this tick, and with the
        // zero buffer its own 5.0 at 100.5 was subtracted, wholly emptying that level so `Join`
        // moved to the next genuine ask. So its ladder entry moved levels with it.
        let own = mm.own_book().expect("mounted");
        assert!(own.book.contains("ask"), "the ask still rests");
        assert_eq!(
            own.book.qty_at(OwnSide::Ask, 101.0, &own.filter_at(20)).to_bits(),
            5.0_f64.to_bits(),
            "the ask's own size is tracked at the level it re-priced onto"
        );
        assert_eq!(
            own.book.qty_at(OwnSide::Ask, 100.5, &own.filter_at(20)).to_bits(),
            0.0_f64.to_bits(),
            "and nothing is left behind at the level it vacated"
        );
    }
}
