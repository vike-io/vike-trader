//! The strategy-specific half of the resting-ask entry rule for [`crate::CheapNp`]: re-scoring the
//! dual-β edge at a price OTHER than the one the gate fired on, and inverting that for the highest
//! price still worth paying. Pure — no book, no engine, no I/O.
//!
//! # The flaw, and where each half of the fix lives
//!
//! `cheap_np` fires on a taker BUY **print** and books that print's price, which somebody else
//! already took; a taker can obtain only the **resting ask**. Over 835 April-2026 entries against
//! the reconstructed L2 book (`cheap_np_depth`): print avg **0.2683**, resting best ask **0.3063**
//! (**0.3493** after the entry's own Polygon block); 738 of 835 had no resting ask at the printed
//! price at all. Paying 4–8 cents more thins `edge = prob_wc − price − fee(price)` as much, so a
//! signal clearing θ = 0.055 on the print may not clear it on the ask: *not trades*, not smaller.
//!
//! Filling a taker at a price nobody offered is an ENGINE property shared by every liquidity
//! taker, so the fix is layered:
//!
//! | layer | what it owns | where |
//! |---|---|---|
//! | engine fill | a taker pays the WALK of the resting book | `vike_fills::fill_model::L2BookFillModel` |
//! | broker read | what `qty` would cost right now | [`vike_model::Broker::quote_vwap`] / [`vike_model::Broker::depth_within_price`] |
//! | venue rule | match at `T + 250 ms`, rest if no longer crossing | `vike_model::POLYMARKET_ITODE_HOLD_MS` + the L2 tier |
//! | **this module** | re-score MY edge at the price I would pay; skip below θ | [`edge_at`] / [`price_at_edge`] |
//!
//! Only the last row is strategy knowledge.
//!
//! # Why no spot or σ is re-evaluated
//!
//! `prob_wc` (the dual-β worst-case probability) depends on spot, σ and time only, never on the
//! price paid, so [`prob_wc`] recovers it EXACTLY from [`crate::CheapNpSignal::edge`]
//! (`prob_wc − ask − fee(ask)`) and [`edge_at`] re-scores at any price with no σ recomputation.
//! [`price_at_edge`] inverts that for the highest price still clearing θ — the order's LIMIT and
//! the depth read's price cap.

use crate::strategies::fair_value::{FEE_RATE, fee};

/// Recover the dual-β worst-case probability a fired signal was scored on, EXACTLY (inverts
/// `edge = prob_wc − ask − fee(ask)`). Every re-scoring here is built on it.
#[inline]
pub fn prob_wc(ask: f64, edge: f64) -> f64 {
    edge + ask + fee(ask)
}

/// The dual-β worst-case edge of buying at `price`, given the `prob_wc` the gate fired on — the
/// one line the ask gate turns on (`price` is the only input that differs from the print).
#[inline]
pub fn edge_at(prob_wc: f64, price: f64) -> f64 {
    prob_wc - price - fee(price)
}

/// The highest fill price whose dual-β worst-case edge still clears `theta`.
///
/// `edge(p) = prob_wc − p − FEE_RATE·p·(1−p) > theta` ⟺ `FEE_RATE·p² − (1+FEE_RATE)·p + C > 0`
/// with `C = prob_wc − theta`. The left side is a downward-opening parabola in `p` whose smaller
/// root is the boundary (the cost `p + fee(p)` is strictly increasing on `[0, 1]`, its derivative
/// being `1 + FEE_RATE(1 − 2p) >= 1 − FEE_RATE > 0`), so the answer is that root. `None` when even
/// a free fill cannot clear θ (`C <= 0`) — a signal whose own edge was already at the bar.
///
/// The ONE θ-clearing price shared by the strategy and the depth and ask-gate measurements.
pub fn price_at_edge(prob_wc: f64, theta: f64) -> Option<f64> {
    let c = prob_wc - theta;
    if c <= 0.0 {
        return None;
    }
    let (a, b) = (FEE_RATE, 1.0 + FEE_RATE);
    let disc = b * b - 4.0 * a * c;
    if disc < 0.0 {
        // No real root: the cost curve never reaches C on [0, 1] — i.e. any price up to 1 clears.
        return Some(1.0);
    }
    Some(((b - disc.sqrt()) / (2.0 * a)).clamp(0.0, 1.0))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::strategies::fair_value::THETA;

    /// `prob_wc` must round-trip the gate exactly, because every re-score is built on it. Exact
    /// equality, not a tolerance: it is the same three f64 operations run backwards.
    #[test]
    fn prob_wc_round_trips_the_signal_edge_exactly() {
        for &(ask, pw) in &[(0.20, 0.40), (0.3125, 0.55), (0.10, 0.2), (0.349, 0.9)] {
            let edge = pw - ask - fee(ask);
            assert_eq!(prob_wc(ask, edge), pw);
            assert_eq!(edge_at(pw, ask), edge);
        }
    }

    /// The whole capacity curve rests on this inversion, so it is checked against the forward edge
    /// formula rather than a magic number. (Moved with the function from `cheap_np_depth`.)
    #[test]
    fn price_at_edge_inverts_the_edge_formula() {
        for &(ask, edge) in &[(0.20, 0.10), (0.30, 0.06), (0.12, 0.25)] {
            let pw = prob_wc(ask, edge);
            let p = price_at_edge(pw, THETA).unwrap();
            assert!(
                (edge_at(pw, p) - THETA).abs() < 1e-12,
                "edge at p_max must be exactly θ: {} vs {THETA}",
                edge_at(pw, p)
            );
            // and it is above the price actually paid, since that entry cleared θ by `edge − θ`
            assert!(p > ask, "p_max {p} must exceed the entry ask {ask}");
            // one tick further must NOT clear
            assert!(edge_at(pw, p + 1e-4) < THETA);
        }
    }

    #[test]
    fn price_at_edge_is_none_when_even_a_free_fill_misses_theta() {
        assert_eq!(price_at_edge(0.05, THETA), None);
        assert_eq!(price_at_edge(THETA, THETA), None, "exactly at the bar does not clear");
    }

    /// THE flaw, as a test: a signal that clears θ on the PRINT does not clear it on the ask.
    #[test]
    fn a_signal_that_clears_theta_on_the_print_can_fail_on_the_resting_ask() {
        // print 0.27 with edge 0.06 (clears θ = 0.055)
        let (print_px, edge) = (0.27, 0.06);
        let pw = prob_wc(print_px, edge);
        assert!(edge_at(pw, print_px) > THETA, "the print cleared the bar");

        // the measured April reality: the resting ask is ~4 cents worse than the print
        let resting_ask = 0.31;
        assert!(
            edge_at(pw, resting_ask) < THETA,
            "...and the ask does not: {}",
            edge_at(pw, resting_ask)
        );
        // which is exactly `resting_ask > p_edge`
        assert!(resting_ask > price_at_edge(pw, THETA).unwrap());
    }

    /// The edge is strictly decreasing in the price paid over the cheap band — the property the
    /// gate's "enter only if it still clears θ" relies on for `price_at_edge` to be a THRESHOLD.
    #[test]
    fn the_edge_falls_monotonically_as_the_price_paid_rises() {
        let pw = prob_wc(0.20, 0.15);
        let mut prev = f64::INFINITY;
        let mut p = 0.05;
        while p < 0.95 {
            let e = edge_at(pw, p);
            assert!(e < prev, "edge must fall as price rises: {e} !< {prev} at p={p}");
            prev = e;
            p += 0.01;
        }
    }
}
