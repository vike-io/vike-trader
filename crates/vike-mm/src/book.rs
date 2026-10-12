//! The normalized [`BookView`] the quote-style pricing + own-order filtration are pure functions
//! of — split out of the crate root; behavior is byte-identical (whole items moved verbatim).

#[cfg(doc)]
use vike_model::OwnOrder;
use vike_model::{
    BookLevel, L2Book, OwnOrderBook, OwnQtyFilter, OwnSide, QuoteStyle, QuoteTick, StatusMask,
};

/// A normalized, best-first view of one book that the [`QuoteStyle`] registry + own-order filtration
/// are PURE functions of. `bids` are highest-price first, `asks` lowest-price first; each level is
/// `(price, size)`. `tick_size` is the venue price grid — [`QuoteStyle::Top`] steps one tick in front
/// of the best, and filtration matches our resting orders to a level by tick (so it needs a positive
/// grid: an L1 quote feed supplies it via the maker's configured `tick_size`
/// ([`SpreadMakerParams::tick_size`](vike_model::SpreadMakerParams::tick_size)), an L2 book
/// carries its own). Built from an L1 [`QuoteTick`] (one level per side) or the top of an [`L2Book`].
#[derive(Clone, Debug)]
pub(crate) struct BookView {
    pub(crate) tick_size: f64,
    /// highest-price first
    pub(crate) bids: Vec<BookLevel>,
    /// lowest-price first
    pub(crate) asks: Vec<BookLevel>,
}

impl BookView {
    /// From an L1 [`QuoteTick`] — exactly one level per side (the touch). `tick_size` is the caller's
    /// configured grid (`0.0` = unknown → `Top` can't step and filtration is inert).
    pub(crate) fn from_quote(q: &QuoteTick, tick_size: f64) -> Self {
        BookView {
            tick_size,
            bids: vec![BookLevel::new(q.bid, q.bid_size)],
            asks: vec![BookLevel::new(q.ask, q.ask_size)],
        }
    }

    /// From an [`L2Book`] — the `n` best levels per side, on the book's own tick grid.
    pub(crate) fn from_l2(book: &L2Book, n: usize) -> Self {
        let (bids, asks) = book.top_n(n);
        BookView { tick_size: book.tick_size, bids, asks }
    }

    /// [`OwnOrderBook::subtract_levels`] applied to BOTH sides of this view — the exact input the
    /// [`QuoteStyle`] pricing consumes. A method here, not on the ladder, because `BookView` is
    /// crate-private and the ladder lives in vike-model.
    pub(crate) fn subtracted(&self, own: &OwnOrderBook, filter: &OwnQtyFilter) -> BookView {
        BookView {
            tick_size: self.tick_size,
            bids: own.subtract_levels(OwnSide::Bid, &self.bids, filter),
            asks: own.subtract_levels(OwnSide::Ask, &self.asks, filter),
        }
    }

    /// Derive `(bid_px, ask_px)` from THIS book (already own-filtered by the caller) under `style`.
    /// PURE. Returns `None` when the book lacks a side (can't quote two-sided). `half_spread` is used
    /// by `Mid` only, `depth_levels` by `Depth` only. This is the pricing arithmetic for
    /// [`QuoteStyle`] — the DATA half of which lives in vike-model so it can ride the live-params
    /// payload; the arithmetic stays here because it needs this crate-private book view.
    ///
    /// - `Mid` (DEFAULT): `mid = 0.5·(best_bid + best_ask)`, `bid = mid − half_spread`,
    ///   `ask = mid + half_spread` — the ORIGINAL SpreadMaker pricing, reproduced bit-for-bit.
    /// - `Join`: `bid = best_bid`, `ask = best_ask` (quote at the touch).
    /// - `Top`: `bid = best_bid + tick`, `ask = best_ask − tick` (one tick in front; `tick` is the
    ///   book's `tick_size`, so a `0.0` grid collapses `Top` to the touch).
    /// - `Depth`: `bid = bids[n].price`, `ask = asks[n].price` for `n = depth_levels`, clamped to the
    ///   deepest available level (so a 1-level L1 view collapses to the touch).
    pub(crate) fn priced(
        &self,
        style: QuoteStyle,
        half_spread: f64,
        depth_levels: usize,
    ) -> Option<(f64, f64)> {
        let BookLevel { price: best_bid, qty: _ } = *self.bids.first()?;
        let BookLevel { price: best_ask, qty: _ } = *self.asks.first()?;
        Some(match style {
            QuoteStyle::Mid => {
                let mid = 0.5 * (best_bid + best_ask);
                (mid - half_spread, mid + half_spread)
            }
            QuoteStyle::Join => (best_bid, best_ask),
            QuoteStyle::Top => (best_bid + self.tick_size, best_ask - self.tick_size),
            QuoteStyle::Depth => {
                // first()? above guarantees each side is non-empty, so len − 1 can't underflow
                let bi = depth_levels.min(self.bids.len() - 1);
                let ai = depth_levels.min(self.asks.len() - 1);
                (self.bids[bi].price, self.asks[ai].price)
            }
        })
    }
}

/// Subtract our OWN resting orders from the public book so the maker never joins or leans on its own
/// quote — the correctness fix for quoting on the same feed it consumes. PURE: returns a filtered
/// copy. For each own order `(price, size)` we find the level at the SAME tick on that side and
/// subtract our size; a level whose remaining size is `<= 0` is REMOVED, so the derived best shifts to
/// the next genuine market level (exactly what stops a `Top`/`Join` maker from stepping in front of —
/// or resting on — itself). Matching needs a positive `tick_size`; an unknown grid (`<= 0`) is a
/// no-op (returns the book unchanged).
pub(crate) fn filter_own(
    book: &BookView,
    own_bid: Option<(f64, f64)>,
    own_ask: Option<(f64, f64)>,
) -> BookView {
    let mut out = book.clone();
    if let Some((px, sz)) = own_bid {
        subtract_own(&mut out.bids, out.tick_size, px, sz);
    }
    if let Some((px, sz)) = own_ask {
        subtract_own(&mut out.asks, out.tick_size, px, sz);
    }
    out
}

/// Subtract `sz` from the level at `px`'s tick on one side; REMOVE the level if it goes non-positive.
/// No grid (`tick_size <= 0`) ⇒ no-op (can't reliably match a float price to a level). Matching by
/// tick index mirrors [`L2Book`]'s own quantization, so an L2-derived level price maps back exactly.
pub(crate) fn subtract_own(levels: &mut Vec<BookLevel>, tick_size: f64, px: f64, sz: f64) {
    if tick_size <= 0.0 {
        return;
    }
    let want = (px / tick_size).round_ties_even() as i64;
    let at = levels
        .iter()
        .position(|&BookLevel { price: lp, .. }| (lp / tick_size).round_ties_even() as i64 == want);
    if let Some(i) = at {
        let remaining = levels[i].qty - sz;
        if remaining > 0.0 {
            levels[i].qty = remaining;
        } else {
            levels.remove(i);
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
        book.subtracted(&self.book, &self.filter_at(now))
    }
}

#[path = "wire_in_tests.rs"]
#[cfg(test)]
mod wire_in_tests;
