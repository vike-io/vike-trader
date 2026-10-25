//! Depth-imbalance studies over `vike_marketdata::L2Book` (VisualHFT-style book pressure;
//! independent implementation). `depth_imbalance`: (Σ bidQ − Σ askQ)/(Σ bidQ + Σ askQ)
//! over the top `n` levels of each side — the depth-N generalization of
//! `L2Book::imbalance` (which is the n=1 top-of-book special case, kept where it is);
//! sign is bid pressure (+1 all-bid … −1 all-ask). `weighted_depth_imbalance`: the same
//! ratio with every level's size damped by inverse distance to mid, w = 1/(1+d)² where
//! d = |price − mid|/mid — so touch depth counts (almost) fully and far depth fades
//! quadratically. Pure functions over `&L2Book`.
//!
//! Fold order is deterministic: `top_n` yields bids high→low and asks low→high, and both
//! sums are naive `+=` folds in that order.
use vike_marketdata::{BookLevel, L2Book};

/// Depth-N size imbalance in [−1, 1]: (Σ bidQ − Σ askQ)/(Σ bidQ + Σ askQ) over the top
/// `n` levels of each side. None if either side is empty or all sizes are zero.
pub fn depth_imbalance(book: &L2Book, n: usize) -> Option<f64> {
    let (bids, asks) = book.top_n(n);
    if bids.is_empty() || asks.is_empty() {
        return None;
    }
    let mut bid_sum = 0.0;
    for &BookLevel { qty: q, .. } in &bids {
        bid_sum += q;
    }
    let mut ask_sum = 0.0;
    for &BookLevel { qty: q, .. } in &asks {
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
    for &BookLevel { price: px, qty: q } in &bids {
        bid_sum += weight(px, mid) * q;
    }
    let mut ask_sum = 0.0;
    for &BookLevel { price: px, qty: q } in &asks {
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

#[path = "imbalance_tests.rs"]
#[cfg(test)]
mod imbalance_tests;
