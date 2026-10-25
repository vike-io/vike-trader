//! Two [0,1]-native half-spread SOURCES for the maker's `half_spread` slot — pure functions of a
//! book/belief, isolated so they unit-test without a runtime. Both are alternatives to the
//! Avellaneda–Stoikov `half_spread = ½·[γ·V + (2/γ)·ln(1 + γ/κ)]`: they price the spread from
//! prediction-market microstructure instead of an inventory/vol/liquidity trio, which is the natural
//! fit for a [0,1] binary-outcome venue (Polymarket up/down).
//!
//! - **A) LS-LMSR** — Othman & Sandholm, *A Practical Liquidity-Sensitive Automated Market Maker*,
//!   EC'10 (cs.cmu.edu/~sandholm). The liquidity parameter scales with volume, `b(q) = α·Σ_i q_i`,
//!   so the market maker's built-in overround (Σ_i p_i − 1) IS a volume-scaled spread. We read that
//!   overround off as a half-spread.
//! - **B) Glosten–Milgrom** — Glosten & Milgrom, *Bid, ask and transaction prices in a specialist
//!   market with heterogeneously informed traders*, JFE 14:71-100 (1985). The spread is pure adverse
//!   selection: an informed fraction `μ` trades on a known binary value `V∈{0,1}`, noise traders flip
//!   a coin, and the competitive dealer's zero-profit bid/ask are the conditional expectations
//!   `E[V|sell]`/`E[V|buy]`. The half-spread is the price of being picked off.
//!
//! Pure, `std`-only, naive f64 folds (no `mul_add`) — no other-module `use`, no deps.

/// A tiny interior clamp keeping the Glosten–Milgrom belief strictly inside `(0, 1)` so the
/// zero-profit conditional expectations never form `0/0` (which happens at `π∈{0,1}` with `μ=1`).
/// Identity for any real interior belief, so the `μ=0 ⇒ ask==bid==π` exactness holds untouched.
const PI_GUARD: f64 = 1e-12;

// ============================================================================================
// A) LS-LMSR volume-scaled half-spread (Othman–Sandholm, EC'10)
// ============================================================================================

/// The two instantaneous LS-LMSR prices `p_i = ∂C/∂q_i` of the binary (2-outcome) cost function
/// `C(q) = b(q)·ln(Σ_i exp(q_i/b(q)))` with liquidity `b(q) = α·(q_yes + q_no)`. Returns
/// `(p_yes, p_no)`; because `b` itself depends on `q`, the pair SUMS TO MORE THAN 1 — that overround
/// is the maker's liquidity-sensitive spread.
///
/// Derivation of the `∂C/∂q_i` we implement (the `b(q)`-dependence adds a term beyond plain LMSR).
/// Let `Q = q_yes + q_no`, `b = α·Q` (so `∂b/∂q_i = α`), `S = Σ_j exp(q_j/b)`. Then
/// ```text
///   ∂C/∂q_i = (∂b/∂q_i)·ln(S) + b·(1/S)·∂S/∂q_i
///   ∂S/∂q_i = (1/b²)·[ b·exp(q_i/b) − α·Σ_j q_j·exp(q_j/b) ]
///   ⇒ p_i = α·ln(S) + exp(q_i/b)/S − (α/(b·S))·Σ_j q_j·exp(q_j/b)
/// ```
/// The middle term is the ordinary LMSR softmax price; `α·ln(S)` and the `Σ_j q_j·exp` term are the
/// liquidity-sensitivity correction. At the symmetric point `q_yes==q_no` this yields the known
/// closed form `p_i = ½ + α·ln(2)` (checked in tests).
///
/// Guard: `α ≤ 0` or `Q ≤ 0` ⇒ the degenerate uniform prior `(0.5, 0.5)` (no overround).
pub(crate) fn ls_lmsr_prices(q_yes: f64, q_no: f64, alpha: f64) -> (f64, f64) {
    let q = q_yes + q_no;
    if alpha <= 0.0 || q <= 0.0 {
        return (0.5, 0.5);
    }
    let b = alpha * q;
    let e_yes = libm::exp(q_yes / b);
    let e_no = libm::exp(q_no / b);
    let s = e_yes + e_no; // naive fold
    let ln_s = libm::log(s);
    // Σ_j q_j·exp(q_j/b) — naive fold, no mul_add.
    let weighted = q_yes * e_yes + q_no * e_no;
    let correction = (alpha / (b * s)) * weighted; // the shared liquidity-sensitivity term
    let p_yes = alpha * ln_s + e_yes / s - correction;
    let p_no = alpha * ln_s + e_no / s - correction;
    (p_yes, p_no)
}

/// The LS-LMSR half-spread = half the overround `(p_yes + p_no − 1)/2`. This is the `half_spread`
/// feed for `as_quotes`: a volume-scaled, liquidity-sensitive spread instead of the A–S vol/κ term.
///
/// At the symmetric book `q_yes==q_no` (>0) the overround is exactly `α·n·ln(n) = α·2·ln(2)`, so
/// this returns exactly `α·ln(2)` (the pinned identity). More balanced books carry a WIDER spread
/// than lopsided ones (a near-resolved book, `p→(1,0)`, has overround → 0). Guard: `α ≤ 0` or
/// `Q ≤ 0` ⇒ `0.0`.
pub(crate) fn ls_lmsr_half_spread(q_yes: f64, q_no: f64, alpha: f64) -> f64 {
    let (p_yes, p_no) = ls_lmsr_prices(q_yes, q_no, alpha);
    (p_yes + p_no - 1.0) / 2.0
}

// ============================================================================================
// B) Glosten–Milgrom adverse-selection spread (binary V∈{0,1}, JFE 1985)
// ============================================================================================

/// The competitive dealer's zero-profit `(bid, ask)` for a binary asset `V∈{0,1}` under belief
/// `π = Pr(V=1)` (the mid) and informed-trader fraction `μ∈[0,1]` (noise traders buy/sell 50/50):
/// ```text
///   ask = E[V|buy]  = π·(1+μ) / ( π·(1+μ) + (1−π)·(1−μ) )
///   bid = E[V|sell] = π·(1−μ) / ( π·(1−μ) + (1−π)·(1+μ) )
/// ```
/// `bid ≤ π ≤ ask` (strict for `0<μ<1`, `0<π<1`); `μ=0 ⇒ ask==bid==π` (no adverse selection, exact);
/// `μ=1 ⇒ (bid, ask) = (0, 1)`. `π` is clamped to `(0,1)` by [`PI_GUARD`] so the degenerate priors
/// return finite values (never `0/0`).
pub(crate) fn glosten_milgrom_quotes(pi: f64, mu: f64) -> (f64, f64) {
    let p = pi.clamp(PI_GUARD, 1.0 - PI_GUARD);
    let q = 1.0 - p;
    // ask = E[V | buy]: a buy is more likely when V=1, so the informed edge tilts the numerator up.
    let ask_num = p * (1.0 + mu);
    let ask_den = ask_num + q * (1.0 - mu);
    let ask = ask_num / ask_den;
    // bid = E[V | sell]: a sell is more likely when V=0, tilting the belief down.
    let bid_num = p * (1.0 - mu);
    let bid_den = bid_num + q * (1.0 + mu);
    let bid = bid_num / bid_den;
    (bid, ask)
}

/// The Glosten–Milgrom half-spread `(ask − bid)/2` — the price of adverse selection. This is the
/// `half_spread` feed for `as_quotes` (an alternative to the A–S vol/κ term). It has the `π·(1−π)`
/// shape: maximised near `π=0.5` and → 0 as `π → 0` or `π → 1` (a near-resolved binary has nothing
/// left to be picked off on).
pub(crate) fn glosten_milgrom_half_spread(pi: f64, mu: f64) -> f64 {
    let (bid, ask) = glosten_milgrom_quotes(pi, mu);
    (ask - bid) / 2.0
}

#[cfg(test)]
mod tests {
    use super::*;

    const EPS: f64 = 1e-9;

    // ---- A) LS-LMSR -----------------------------------------------------------------------

    #[test]
    fn ls_lmsr_symmetric_identity() {
        // At q_yes==q_no, overround == α·2·ln(2), so half_spread == α·ln(2), exactly.
        for &x in &[0.5_f64, 1.0, 10.0, 137.25, 5000.0] {
            for &alpha in &[0.01_f64, 0.05, 0.1, 0.3] {
                let hs = ls_lmsr_half_spread(x, x, alpha);
                let want = alpha * 2.0_f64.ln();
                assert!((hs - want).abs() < EPS, "x={x} alpha={alpha}: hs={hs} want={want}");
                // and each price is ½ + α·ln(2)
                let (py, pn) = ls_lmsr_prices(x, x, alpha);
                let pw = 0.5 + alpha * 2.0_f64.ln();
                assert!((py - pw).abs() < EPS && (pn - pw).abs() < EPS);
            }
        }
    }

    #[test]
    fn ls_lmsr_overround_positive_for_asymmetric_books() {
        for &(qy, qn) in &[(100.0, 1.0), (3.0, 7.0), (250.0, 40.0), (1.0, 9.0)] {
            let (py, pn) = ls_lmsr_prices(qy, qn, 0.05);
            let over = py + pn - 1.0;
            assert!(over > 0.0, "qy={qy} qn={qn}: overround={over} not > 0");
            // Both prices are POSITIVE and sum to > 1 (the overround). NOTE: LS-LMSR does NOT bound an
            // individual price below 1 — for a near-resolved (extreme-ratio) book the near-certain
            // outcome's price approaches 1 while the overround shrinks toward 0. The maker only consumes
            // the half-spread (overround/2) and feeds the bounded split (mid, 1−mid), never these
            // extremes, so the `< 1` bound is neither guaranteed nor needed.
            assert!(py > 0.0 && pn > 0.0, "qy={qy} qn={qn}: py={py} pn={pn} not both positive");
        }
    }

    #[test]
    fn ls_lmsr_half_spread_increases_with_alpha() {
        let mut prev = f64::NEG_INFINITY;
        for &alpha in &[0.01_f64, 0.02, 0.05, 0.1, 0.2, 0.4] {
            let hs = ls_lmsr_half_spread(30.0, 12.0, alpha);
            assert!(hs > prev, "alpha={alpha}: hs={hs} not > prev={prev}");
            prev = hs;
        }
    }

    #[test]
    fn ls_lmsr_balanced_wider_than_lopsided() {
        let alpha = 0.05;
        let balanced = ls_lmsr_half_spread(50.0, 50.0, alpha);
        let mild = ls_lmsr_half_spread(70.0, 30.0, alpha);
        let lopsided = ls_lmsr_half_spread(99.0, 1.0, alpha);
        assert!(
            balanced > mild && mild > lopsided,
            "balanced={balanced} mild={mild} lopsided={lopsided}"
        );
        assert!(lopsided > 0.0);
    }

    #[test]
    fn ls_lmsr_guards() {
        assert_eq!(ls_lmsr_prices(0.0, 0.0, 0.05), (0.5, 0.5));
        assert_eq!(ls_lmsr_prices(-1.0, -2.0, 0.05), (0.5, 0.5));
        assert_eq!(ls_lmsr_prices(10.0, 10.0, 0.0), (0.5, 0.5));
        assert_eq!(ls_lmsr_prices(10.0, 10.0, -0.1), (0.5, 0.5));
        assert_eq!(ls_lmsr_half_spread(0.0, 0.0, 0.05), 0.0);
        assert_eq!(ls_lmsr_half_spread(10.0, 10.0, 0.0), 0.0);
    }

    // ---- B) Glosten–Milgrom ---------------------------------------------------------------

    #[test]
    fn gm_no_informed_zero_spread_exact() {
        // μ=0 ⇒ ask==bid==π, exactly (no adverse selection).
        for &pi in &[0.1_f64, 0.25, 0.5, 0.73, 0.9] {
            let (bid, ask) = glosten_milgrom_quotes(pi, 0.0);
            assert_eq!(ask, pi, "ask");
            assert_eq!(bid, pi, "bid");
            assert_eq!(glosten_milgrom_half_spread(pi, 0.0), 0.0);
        }
    }

    #[test]
    fn gm_fully_informed_saturates() {
        // μ=1 ⇒ ask==1, bid==0.
        for &pi in &[0.2_f64, 0.5, 0.8] {
            let (bid, ask) = glosten_milgrom_quotes(pi, 1.0);
            assert!((ask - 1.0).abs() < EPS, "ask={ask}");
            assert!(bid.abs() < EPS, "bid={bid}");
        }
    }

    #[test]
    fn gm_ask_above_pi_above_bid() {
        for &pi in &[0.15_f64, 0.4, 0.5, 0.66, 0.85] {
            for &mu in &[0.1_f64, 0.3, 0.6, 0.9] {
                let (bid, ask) = glosten_milgrom_quotes(pi, mu);
                assert!(ask > pi, "pi={pi} mu={mu}: ask={ask} !> pi");
                assert!(bid < pi, "pi={pi} mu={mu}: bid={bid} !< pi");
            }
        }
    }

    #[test]
    fn gm_half_spread_peaks_at_half_and_vanishes_at_edges() {
        let mu = 0.4;
        let center = glosten_milgrom_half_spread(0.5, mu);
        let mid = glosten_milgrom_half_spread(0.25, mu);
        let edge = glosten_milgrom_half_spread(0.05, mu);
        assert!(center > mid && mid > edge, "center={center} mid={mid} edge={edge}");
        // p(1−p) symmetry: same spread at π and 1−π
        assert!(
            (glosten_milgrom_half_spread(0.3, mu) - glosten_milgrom_half_spread(0.7, mu)).abs()
                < EPS
        );
        // → 0 at the edges
        assert!(glosten_milgrom_half_spread(1e-9, mu) < 1e-6);
        assert!(glosten_milgrom_half_spread(1.0 - 1e-9, mu) < 1e-6);
    }

    #[test]
    fn gm_degenerate_priors_are_finite() {
        for &pi in &[0.0_f64, 1.0] {
            for &mu in &[0.0_f64, 0.5, 1.0] {
                let (bid, ask) = glosten_milgrom_quotes(pi, mu);
                assert!(bid.is_finite() && ask.is_finite(), "pi={pi} mu={mu}: bid={bid} ask={ask}");
                assert!(glosten_milgrom_half_spread(pi, mu).is_finite());
            }
        }
    }
}
