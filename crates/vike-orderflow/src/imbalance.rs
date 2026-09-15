//! Depth-imbalance studies over `vike_model::L2Book` (VisualHFT-style book pressure;
//! independent implementation). `depth_imbalance`: (Σ bidQ − Σ askQ)/(Σ bidQ + Σ askQ)
//! over the top `n` levels of each side — the depth-N generalization of
//! `L2Book::imbalance` (which is the n=1 top-of-book special case, kept where it is);
//! sign is bid pressure (+1 all-bid … −1 all-ask). `weighted_depth_imbalance`: the same
//! ratio with every level's size damped by inverse distance to mid, w = 1/(1+d)² where
//! d = |price − mid|/mid — so touch depth counts (almost) fully and far depth fades
//! quadratically. Pure functions over `&L2Book` + `ImbalanceTracker`, a small streaming
//! wrapper that recomputes on every book update and caches the last GOOD value across
//! momentarily unreadable books (one side empty mid-resync).
//!
//! Fold order is deterministic: `top_n` yields bids high→low and asks low→high, and both
//! sums are naive `+=` folds in that order.
use vike_model::L2Book;

/// Depth-N size imbalance in [−1, 1]: (Σ bidQ − Σ askQ)/(Σ bidQ + Σ askQ) over the top
/// `n` levels of each side. None if either side is empty or all sizes are zero.
pub fn depth_imbalance(book: &L2Book, n: usize) -> Option<f64> {
    let (bids, asks) = book.top_n(n);
    if bids.is_empty() || asks.is_empty() {
        return None;
    }
    let mut bid_sum = 0.0;
    for &(_, q) in &bids {
        bid_sum += q;
    }
    let mut ask_sum = 0.0;
    for &(_, q) in &asks {
        ask_sum += q;
    }
    ratio(bid_sum, ask_sum)
}

/// Inverse-distance-weighted depth-N imbalance: each level contributes `w·q` with
/// w = 1/(1+d)² and d = |price − mid|/mid. None if either side is empty, the mid is
/// unavailable/non-positive (Polymarket-style books can sit at 0), or all weighted
/// sizes are zero.
pub fn weighted_depth_imbalance(book: &L2Book, n: usize) -> Option<f64> {
    let mid = book.mid()?;
    if mid <= 0.0 {
        return None;
    }
    let (bids, asks) = book.top_n(n);
    if bids.is_empty() || asks.is_empty() {
        return None;
    }
    let mut bid_sum = 0.0;
    for &(px, q) in &bids {
        bid_sum += weight(px, mid) * q;
    }
    let mut ask_sum = 0.0;
    for &(px, q) in &asks {
        ask_sum += weight(px, mid) * q;
    }
    ratio(bid_sum, ask_sum)
}

/// w = 1/(1+d)², d = |price − mid|/mid.
fn weight(price: f64, mid: f64) -> f64 {
    let d = (price - mid).abs() / mid;
    1.0 / ((1.0 + d) * (1.0 + d))
}

fn ratio(bid_sum: f64, ask_sum: f64) -> Option<f64> {
    let denom = bid_sum + ask_sum;
    if denom == 0.0 { None } else { Some((bid_sum - ask_sum) / denom) }
}

/// Streaming wrapper: recomputes the chosen variant on each `update` and caches the last
/// readable value — a momentarily one-sided book (resync flicker) returns None from
/// `update` but does NOT wipe `last()`.
pub struct ImbalanceTracker {
    n: usize,
    weighted: bool,
    last: Option<f64>,
}

impl ImbalanceTracker {
    pub fn new(n: usize, weighted: bool) -> Self {
        assert!(n >= 1, "ImbalanceTracker n must be >= 1");
        ImbalanceTracker { n, weighted, last: None }
    }

    /// Recompute over `book`; a Some result replaces the cache and is returned.
    pub fn update(&mut self, book: &L2Book) -> Option<f64> {
        let v = if self.weighted {
            weighted_depth_imbalance(book, self.n)
        } else {
            depth_imbalance(book, self.n)
        };
        if v.is_some() {
            self.last = v;
        }
        v
    }

    /// Last good value (survives unreadable-book updates).
    pub fn last(&self) -> Option<f64> {
        self.last
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use vike_model::Level;

    fn book(bids: &[Level], asks: &[Level]) -> L2Book {
        let mut b = L2Book::new(0.5);
        b.apply_snapshot(1, bids, asks);
        b
    }

    // bids (99,10)(98,5), asks (101,10)(102,15): n=2 → (15−25)/40 = −0.25;
    // n=1 → (10−10)/20 = 0 (matches L2Book::imbalance, the n=1 special case).
    #[test]
    fn depth_imbalance_expected_values() {
        let b = book(&[(99.0, 10.0), (98.0, 5.0)], &[(101.0, 10.0), (102.0, 15.0)]);
        assert_eq!(depth_imbalance(&b, 2), Some(-0.25));
        assert_eq!(depth_imbalance(&b, 1), Some(0.0));
        assert_eq!(depth_imbalance(&b, 1), b.imbalance());
        // n beyond the book depth just uses what exists:
        assert_eq!(depth_imbalance(&b, 10), Some(-0.25));
    }

    #[test]
    fn depth_imbalance_unreadable_books() {
        assert_eq!(depth_imbalance(&book(&[(99.0, 10.0)], &[]), 2), None); // one side empty
        assert_eq!(depth_imbalance(&L2Book::new(0.5), 2), None); // both empty
    }

    // Symmetric distances (d equal level-for-level) → the weights cancel out of the
    // ratio: bids (99,6)(98,4) asks (101,2)(102,4) has pairwise-equal d, so weighted ==
    // computable by hand from raw sizes with per-pair weights w1, w2.
    #[test]
    fn weighted_imbalance_symmetric_distances() {
        let b = book(&[(99.0, 6.0), (98.0, 4.0)], &[(101.0, 2.0), (102.0, 4.0)]);
        // mid = 100; d = 0.01 (touch pair), 0.02 (second pair)
        let w1 = 1.0 / (1.01_f64 * 1.01_f64);
        let w2 = 1.0 / (1.02_f64 * 1.02_f64);
        let num = w1 * 6.0 + w2 * 4.0 - (w1 * 2.0 + w2 * 4.0);
        let den = w1 * 6.0 + w2 * 4.0 + (w1 * 2.0 + w2 * 4.0);
        let got = weighted_depth_imbalance(&b, 2).unwrap();
        assert!((got - num / den).abs() < 1e-15, "got {got}");
        // single symmetric pair with equal sizes → exactly 0
        let s = book(&[(99.0, 7.0)], &[(101.0, 7.0)]);
        assert_eq!(weighted_depth_imbalance(&s, 1), Some(0.0));
    }

    // Far depth counts less: unweighted sees balance (10 vs 10) but the bid mass is at
    // the touch and the ask mass far away → weighted tips positive (bid pressure).
    #[test]
    fn weighting_dampens_far_levels() {
        let b = book(&[(99.5, 10.0)], &[(100.5, 1.0), (110.0, 9.0)]);
        assert_eq!(depth_imbalance(&b, 2), Some(0.0));
        let w = weighted_depth_imbalance(&b, 2).unwrap();
        assert!(w > 0.0, "weighted should tip toward the near-side mass, got {w}");
        assert!(w < 1.0);
    }

    #[test]
    fn imbalance_bounds_and_sign_flip() {
        let b = book(&[(99.0, 3.0), (98.5, 2.0)], &[(101.0, 9.0)]);
        let m = book(&[(99.0, 9.0)], &[(101.0, 3.0), (101.5, 2.0)]); // mirrored sizes
        for n in 1..=3 {
            let x = depth_imbalance(&b, n).unwrap();
            let y = depth_imbalance(&m, n).unwrap();
            assert!((-1.0..=1.0).contains(&x));
            assert_eq!(x.to_bits(), (-y).to_bits(), "mirror must flip the sign exactly");
        }
    }

    #[test]
    fn tracker_caches_last_good_value() {
        let mut t = ImbalanceTracker::new(2, false);
        assert_eq!(t.last(), None);
        let b = book(&[(99.0, 10.0), (98.0, 5.0)], &[(101.0, 10.0), (102.0, 15.0)]);
        assert_eq!(t.update(&b), Some(-0.25));
        assert_eq!(t.last(), Some(-0.25));
        // resync flicker: one-sided book → update None, cache survives
        assert_eq!(t.update(&book(&[(99.0, 1.0)], &[])), None);
        assert_eq!(t.last(), Some(-0.25));
        // weighted tracker wires the weighted variant
        let mut wt = ImbalanceTracker::new(1, true);
        assert_eq!(wt.update(&book(&[(99.0, 7.0)], &[(101.0, 7.0)])), Some(0.0));
        assert_eq!(wt.last(), Some(0.0));
    }
}
