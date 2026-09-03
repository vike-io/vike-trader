//! The normalized [`BookView`] the quote-style pricing + own-order filtration are pure functions
//! of — split out of the crate root; behavior is byte-identical (whole items moved verbatim).

use vike_model::{L2Book, QuoteStyle, QuoteTick};

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
    pub(crate) bids: Vec<(f64, f64)>,
    /// lowest-price first
    pub(crate) asks: Vec<(f64, f64)>,
}

impl BookView {
    /// From an L1 [`QuoteTick`] — exactly one level per side (the touch). `tick_size` is the caller's
    /// configured grid (`0.0` = unknown → `Top` can't step and filtration is inert).
    pub(crate) fn from_quote(q: &QuoteTick, tick_size: f64) -> Self {
        BookView { tick_size, bids: vec![(q.bid, q.bid_size)], asks: vec![(q.ask, q.ask_size)] }
    }

    /// From an [`L2Book`] — the `n` best levels per side, on the book's own tick grid.
    pub(crate) fn from_l2(book: &L2Book, n: usize) -> Self {
        let (bids, asks) = book.top_n(n);
        BookView { tick_size: book.tick_size, bids, asks }
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
        let (best_bid, _) = *self.bids.first()?;
        let (best_ask, _) = *self.asks.first()?;
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
                (self.bids[bi].0, self.asks[ai].0)
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
pub(crate) fn subtract_own(levels: &mut Vec<(f64, f64)>, tick_size: f64, px: f64, sz: f64) {
    if tick_size <= 0.0 {
        return;
    }
    let want = (px / tick_size).round_ties_even() as i64;
    let at = levels.iter().position(|&(lp, _)| (lp / tick_size).round_ties_even() as i64 == want);
    if let Some(i) = at {
        let remaining = levels[i].1 - sz;
        if remaining > 0.0 {
            levels[i].1 = remaining;
        } else {
            levels.remove(i);
        }
    }
}
