//! LMSR logistic fair-value / inventory-skew reservation for a bounded [0,1] outcome token
//! (Polymarket) — the pure pricing core of [`SpreadMaker`](crate::SpreadMaker)'s reservation site,
//! isolated so it unit-tests without a runtime. Ports Hanson's Logarithmic Market Scoring Rule
//! (LMSR) reservation to a live-anchored form: the binary LMSR price is the logistic
//! `1/(1+exp(-Δ/b))` of net inventory `Δ` over liquidity depth `b` (Hanson, "Logarithmic Market
//! Scoring Rules for Modular Combinatorial Information Aggregation",
//! <http://mason.gmu.edu/~rhanson/mktscore.pdf>). Working in log-odds keeps the reservation on the
//! bounded [0,1] simplex — a linear cash-skew would walk a near-0/near-1 quote straight off the
//! edge — and reproduces the LMSR "wall" near the boundaries, where a unit of inventory moves price
//! by `mid·(1−mid)/b`, vanishingly small at the edges and largest at the coin-flip mid.
//!
//! Style matches `skew.rs`/`book.rs`: `pub(crate)`, pure, NAIVE left-to-right f64 folds (never
//! `mul_add`), std only, no external deps, no cross-module `use`. Tests live in this file.

/// LMSR reservation price anchored to the LIVE market `mid` (this is the one the maker uses at the
/// reservation site). We convert the mid to log-odds, walk it by inventory in log-odds space, then
/// map back with the logistic — so the reservation stays pinned to the market yet leans with the
/// book:
///
/// ```text
/// logit_mid = ln(mid / (1 − mid))
/// r         = 1 / (1 + exp( −(logit_mid − net_inventory / b) ))
/// ```
///
/// Semantics: `net_inventory > 0` (long YES) subtracts from the logit → reservation strictly BELOW
/// `mid` (skew DOWN to sell the position off); short (`net_inventory < 0`) → strictly ABOVE. The
/// per-unit-inventory move is `mid·(1−mid)/b` — the LMSR wall: tiny near `mid = 0`/`mid = 1`, largest
/// near `mid = 0.5`.
///
/// INERT GUARD — byte-identical to "just use `mid`": a degenerate depth (`b <= 0`) or a mid outside
/// the OPEN interval `(0, 1)` (`mid <= 0.0` or `mid >= 1.0`, where the log-odds is undefined) returns
/// `mid` unchanged, so a bad input can never move the quote.
pub(crate) fn lmsr_reservation(mid: f64, net_inventory: f64, b: f64) -> f64 {
    // Degenerate depth or a boundary/out-of-range mid (log-odds undefined) → return mid verbatim.
    if b <= 0.0 || mid <= 0.0 || mid >= 1.0 {
        return mid;
    }
    // No inventory ⇒ the reservation IS the market mid, exactly. Short-circuit rather than round-trip
    // through ln→logistic (two independently-rounded transcendentals need not compose back to `mid`
    // bit-for-bit), which keeps the zero-inventory identity EXACT. Comparing to the `0.0` literal is
    // clippy-clean (float_cmp exempts zero). Semantically this is the `net_inventory / b == 0` case of
    // the formula below, just computed without the lossy round-trip.
    if net_inventory == 0.0 {
        return mid;
    }
    // naive left-to-right throughout: log-odds of the live mid, shifted by inventory/depth, mapped
    // back through the logistic. Reached only for nonzero inventory (the exact-mid case is handled
    // above), so the shift always moves the quote off the mid.
    let logit_mid = libm::log(mid / (1.0 - mid));
    let shifted = logit_mid - net_inventory / b;
    1.0 / (1.0 + libm::exp(-shifted))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn zero_inventory_is_identity() {
        // r(mid, 0, b) == mid EXACTLY for several interior mids: the logistic is the exact inverse
        // of ln(mid/(1-mid)) at the round-trip point.
        for &m in &[0.02_f64, 0.1, 0.25, 0.5, 0.5001, 0.75, 0.9, 0.98] {
            for &b in &[0.5_f64, 1.0, 10.0, 250.0] {
                assert_eq!(lmsr_reservation(m, 0.0, b), m, "mid={m} b={b}");
            }
        }
    }

    #[test]
    fn long_below_short_above() {
        let (m, b) = (0.5_f64, 100.0);
        let long = lmsr_reservation(m, 25.0, b);
        let short = lmsr_reservation(m, -25.0, b);
        assert!(long < m, "long {long} must be strictly below mid {m}");
        assert!(short > m, "short {short} must be strictly above mid {m}");
        // symmetric about the mid in log-odds → symmetric distances here.
        assert!(((m - long) - (short - m)).abs() < 1e-12);
    }

    #[test]
    fn monotone_decreasing_in_inventory() {
        let (m, b) = (0.4_f64, 50.0);
        let mut prev = f64::INFINITY;
        let mut q = -60.0_f64;
        while q <= 60.0 {
            let r = lmsr_reservation(m, q, b);
            assert!(r < prev, "reservation must strictly decrease as inventory rises: q={q}");
            prev = r;
            q += 5.0;
        }
    }

    #[test]
    fn wall_shape_edges_move_less_than_center() {
        // per-unit move ≈ mid*(1-mid)/b near a small inventory step; verify numerically that the
        // edges (0.02, 0.98) move MUCH less than the center (0.5).
        let b = 100.0_f64;
        let eps = 1e-3_f64; // tiny inventory step
        let move_at = |m: f64| (m - lmsr_reservation(m, eps, b)).abs() / eps;

        let center = move_at(0.5);
        let low = move_at(0.02);
        let high = move_at(0.98);

        // analytic slope magnitude is mid*(1-mid)/b.
        let slope = |m: f64| m * (1.0 - m) / b;
        assert!((center - slope(0.5)).abs() < 1e-6, "center slope {center} vs {}", slope(0.5));
        assert!((low - slope(0.02)).abs() < 1e-6, "low slope {low} vs {}", slope(0.02));
        assert!((high - slope(0.98)).abs() < 1e-6, "high slope {high} vs {}", slope(0.98));

        // the wall: edges move far less than the coin-flip center. 0.02*(0.98) = 0.0196 vs 0.25 →
        // more than a 10x damping.
        assert!(low * 10.0 < center, "low {low} not << center {center}");
        assert!(high * 10.0 < center, "high {high} not << center {center}");
    }

    #[test]
    fn huge_depth_returns_to_mid() {
        // b → ∞ ⇒ inventory/b → 0 ⇒ reservation → mid. The log-odds shift is net_inventory/b; with
        // b = 1e12 and a unit inventory the shift is 1e-12, so the price move (≈ mid·(1−mid)·1e-12 ≤
        // 2.5e-13) plus the ln→logistic round-trip noise (~1e-15) stays under the 1e-12 bar. NOTE the
        // inventory here is deliberately nonzero so this exercises the formula, not the fast path.
        let b = 1e12_f64;
        for &m in &[0.05_f64, 0.3, 0.5, 0.7, 0.95] {
            let r = lmsr_reservation(m, 1.0, b);
            assert!((r - m).abs() < 1e-12, "mid={m} r={r} did not collapse to mid");
        }
    }

    #[test]
    fn inert_guards_return_mid_unchanged() {
        // boundary/out-of-range mid or degenerate depth → mid verbatim, any inventory.
        assert_eq!(lmsr_reservation(0.0, 42.0, 100.0), 0.0);
        assert_eq!(lmsr_reservation(1.0, 42.0, 100.0), 1.0);
        assert_eq!(lmsr_reservation(0.5, 42.0, 0.0), 0.5);
        assert_eq!(lmsr_reservation(0.5, 42.0, -3.0), 0.5);
        // out of the open interval entirely.
        assert_eq!(lmsr_reservation(-0.2, 5.0, 100.0), -0.2);
        assert_eq!(lmsr_reservation(1.3, 5.0, 100.0), 1.3);
    }
}
