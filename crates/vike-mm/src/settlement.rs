//! Resolution-aware force-flatten + time-accelerated breaker pulls for a binary [0,1] prediction
//! market — the pure τ-driven risk heuristics of [`SpreadMaker`](crate::SpreadMaker), isolated so
//! they unit-test without a runtime. On a binary market, holding inventory to resolution on the
//! WRONG side is a TOTAL loss, so as τ = (T − t) → 0 the maker must (1) actively unwind toward flat
//! and (2) tighten its circuit breaker on the toxic side faster than symmetric time decay would.
//!
//! The existing `avellaneda` settlement code already SKEWS harder and WIDENS via a settlement-
//! variance term and blacks out near resolution. What lives HERE is the thing it lacks: an active
//! FORCE-FLATTEN schedule ([`flatten_weight`]/[`force_flatten_skew`]) that ramps the target
//! inventory toward ZERO as τ → 0.
//!
//! ⚠ These are PRACTITIONER HEURISTICS. There is NO canonical closed-form `f(τ)` for a force-flatten
//! schedule — the literature (Avellaneda–Stoikov and its settlement
//! variants) prices the reservation/spread but does not prescribe an active-liquidation ramp. The
//! shapes chosen here are deliberately the simplest defensible ones: LINEAR in τ. Callers that want a
//! convex late-panic curve compose these; the linear primitives are the pinned, testable baseline.
//!
//! INERT-WHEN-OFF invariant: every function returns its NEUTRAL value (`0.0` shift / `1.0` multiplier)
//! when its window param is `0` (or non-positive), so a maker that has not opted in is byte-identical
//! to the pre-settlement path.

/// How much of the current position SHOULD already be unwound by now, `∈ [0, 1]` — the force-flatten
/// SCHEDULE. `tau_ms` = (T − t) time-to-resolution (ms); `flatten_by_ms` = the τ at which we want to
/// be fully flat (measured as time-to-resolution, e.g. `30_000` = "flat by 30s before resolution").
///
/// - `flatten_by_ms <= 0` → DISABLED → `0.0` (the inert default; no force-flatten pressure at all).
/// - `tau_ms >= flatten_by_ms` → still outside the window → `0.0` (nothing to unwind yet).
/// - inside `(0, flatten_by_ms)` → ramps LINEARLY from `0.0` up to `1.0` as `tau_ms → 0`:
///   `weight = 1 − tau_ms / flatten_by_ms`.
/// - `tau_ms <= 0` (at/after the flatten point, i.e. resolution reached) → `1.0` (fully flat wanted).
///
/// Monotone non-increasing in `tau_ms`; always clamped to `[0, 1]`.
pub(crate) fn flatten_weight(tau_ms: i64, flatten_by_ms: i64) -> f64 {
    // Disabled, or still outside the flatten window → no unwind pressure.
    if flatten_by_ms <= 0 || tau_ms >= flatten_by_ms {
        return 0.0;
    }
    // At/after the flatten point (τ has run out) → want the whole position gone.
    if tau_ms <= 0 {
        return 1.0;
    }
    // Linear ramp: full window → 0.0, resolution → 1.0. Both operands are in (0, flatten_by_ms), so
    // the ratio is in (0, 1) and the clamp is belt-and-suspenders against i64→f64 edge rounding.
    (1.0 - (tau_ms as f64) / (flatten_by_ms as f64)).clamp(0.0, 1.0)
}

/// Additive reservation-price shift that pushes the maker toward FLAT — the force-flatten SKEW.
/// `q_norm` = signed normalized inventory (`> 0` long, `< 0` short); `flatten_w` = the
/// [`flatten_weight`] schedule value `∈ [0, 1]`; `strength` = how hard to lean at full weight.
///
/// `shift = −strength · flatten_w · q_norm`. A long book (`q_norm > 0`) yields a NEGATIVE shift →
/// lower reservation price → the maker leans to SELL and flatten; a short book mirrors to a positive
/// shift. The lean GROWS with `flatten_w` (→ `1` at resolution), so the pressure to be flat rises as
/// τ → 0. Returns `0.0` when `flatten_w == 0.0` (window not entered / disabled) or `strength == 0.0`
/// (opt-out) — inert, byte-identical to no force-flatten.
pub(crate) fn force_flatten_skew(q_norm: f64, flatten_w: f64, strength: f64) -> f64 {
    // Naive product (no mul_add) to match the crate's fold-order convention.
    -strength * flatten_w * q_norm
}

/// Time-accelerated fill-rate-breaker pull MULTIPLIER for a binary market near resolution (PR-3) — the
/// τ-driven factor the [`SpreadMaker`](crate::SpreadMaker)'s per-side breaker DIVIDES its effective
/// net-fill threshold by, so the breaker trips SOONER as τ → 0 (holding inventory on the wrong side to
/// resolution is a total loss, so the toxic side must be pulled faster than symmetric time decay).
/// `tau_ms` = (T − t) time-to-resolution (ms); `ramp_ms` = the τ window over which the acceleration
/// ramps in; `max_accel` = the extra acceleration at τ = 0 (the multiplier tops out at `1 + max_accel`).
///
/// - `ramp_ms <= 0` → DISABLED → `1.0` (the inert default; no acceleration).
/// - `tau_ms >= ramp_ms` → still outside the window → `1.0` (no acceleration yet).
/// - inside `(0, ramp_ms)` → ramps LINEARLY from `1.0` up to `1 + max_accel` as `tau_ms → 0`:
///   `mult = 1 + max_accel·(1 − tau_ms/ramp_ms)`.
/// - `tau_ms <= 0` (at/after resolution) → `1 + max_accel` (full acceleration).
///
/// ALWAYS `>= 1.0` (floored, so a negative `max_accel` can never WEAKEN the breaker), and monotone
/// non-decreasing as `tau_ms → 0`. Because it is `1.0` bit-for-bit when off (or `resolution_ts` is
/// unknown), dividing the breaker threshold by it is byte-identical on the default path (`x / 1.0 ==
/// x`). Naïve folds, no `mul_add`.
pub(crate) fn accelerated_pull(tau_ms: i64, ramp_ms: i64, max_accel: f64) -> f64 {
    // Disabled, or still outside the ramp window → no acceleration.
    if ramp_ms <= 0 || tau_ms >= ramp_ms {
        return 1.0;
    }
    // At/after resolution (τ has run out) → full acceleration (floored at 1.0).
    if tau_ms <= 0 {
        return (1.0 + max_accel).max(1.0);
    }
    // Linear ramp: window edge → 1.0, resolution → 1 + max_accel. Both operands are in (0, ramp_ms),
    // so the ratio is in (0, 1); the clamp is belt-and-suspenders against i64→f64 edge rounding.
    let w = (1.0 - (tau_ms as f64) / (ramp_ms as f64)).clamp(0.0, 1.0);
    (1.0 + max_accel * w).max(1.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn flatten_weight_disabled_is_zero() {
        // flatten_by_ms <= 0 → off, for any tau.
        assert_eq!(flatten_weight(5_000, 0), 0.0);
        assert_eq!(flatten_weight(5_000, -1), 0.0);
        assert_eq!(flatten_weight(-100, 0), 0.0);
    }

    #[test]
    fn flatten_weight_far_is_zero() {
        // Outside the window (tau >= flatten_by) → nothing to unwind.
        assert_eq!(flatten_weight(30_000, 30_000), 0.0);
        assert_eq!(flatten_weight(60_000, 30_000), 0.0);
    }

    #[test]
    fn flatten_weight_at_or_after_point_is_one() {
        // tau <= 0 (resolution reached/passed) → want fully flat.
        assert_eq!(flatten_weight(0, 30_000), 1.0);
        assert_eq!(flatten_weight(-5_000, 30_000), 1.0);
    }

    #[test]
    fn flatten_weight_ramps_monotone_and_clamped() {
        // Halfway through a 30s window → 0.5.
        assert!((flatten_weight(15_000, 30_000) - 0.5).abs() < 1e-12);
        // Quarter remaining → 0.75 unwound.
        assert!((flatten_weight(7_500, 30_000) - 0.75).abs() < 1e-12);
        // Monotone non-increasing in tau; every value in [0,1].
        let mut prev = 1.0_f64;
        for tau in (0..=30_000).step_by(1_000) {
            let w = flatten_weight(tau, 30_000);
            assert!((0.0..=1.0).contains(&w));
            assert!(w <= prev + 1e-12, "not monotone at tau={tau}");
            prev = w;
        }
    }

    #[test]
    fn force_flatten_skew_sign_and_growth() {
        // Long (q_norm > 0) → negative shift (lean to sell).
        assert!(force_flatten_skew(0.5, 1.0, 0.2) < 0.0);
        // Short (q_norm < 0) → positive shift (lean to buy).
        assert!(force_flatten_skew(-0.5, 1.0, 0.2) > 0.0);
        // Grows in magnitude with flatten_w.
        let early = force_flatten_skew(0.5, 0.25, 0.2).abs();
        let late = force_flatten_skew(0.5, 1.0, 0.2).abs();
        assert!(late > early);
        // Exact value.
        assert!((force_flatten_skew(0.5, 1.0, 0.2) - (-0.1)).abs() < 1e-12);
    }

    #[test]
    fn force_flatten_skew_inert() {
        // flatten_w == 0 → no lean.
        assert_eq!(force_flatten_skew(0.9, 0.0, 0.5), 0.0);
        // strength == 0 → opt-out.
        assert_eq!(force_flatten_skew(0.9, 1.0, 0.0), 0.0);
    }

    #[test]
    fn accelerated_pull_disabled_is_one() {
        // ramp_ms <= 0 → off, for any tau (and BIT-EXACT 1.0 so the divided threshold is unchanged).
        assert_eq!(accelerated_pull(5_000, 0, 1.0).to_bits(), 1.0_f64.to_bits());
        assert_eq!(accelerated_pull(5_000, -1, 1.0).to_bits(), 1.0_f64.to_bits());
        assert_eq!(accelerated_pull(-100, 0, 1.0).to_bits(), 1.0_f64.to_bits());
    }

    #[test]
    fn accelerated_pull_far_is_one() {
        // Outside the window (tau >= ramp) → no acceleration yet.
        assert_eq!(accelerated_pull(10_000, 10_000, 1.0).to_bits(), 1.0_f64.to_bits());
        assert_eq!(accelerated_pull(60_000, 10_000, 1.0).to_bits(), 1.0_f64.to_bits());
    }

    #[test]
    fn accelerated_pull_at_or_after_resolution_is_full() {
        // tau <= 0 (resolution reached/passed) → 1 + max_accel.
        assert!((accelerated_pull(0, 10_000, 1.5) - 2.5).abs() < 1e-12);
        assert!((accelerated_pull(-5_000, 10_000, 1.5) - 2.5).abs() < 1e-12);
    }

    #[test]
    fn accelerated_pull_ramps_monotone_and_floored_at_one() {
        // Halfway through a 10s ramp with max 2.0 → 1 + 2·0.5 = 2.0.
        assert!((accelerated_pull(5_000, 10_000, 2.0) - 2.0).abs() < 1e-12);
        // Quarter remaining → 1 + 2·0.75 = 2.5.
        assert!((accelerated_pull(2_500, 10_000, 2.0) - 2.5).abs() < 1e-12);
        // Monotone NON-INCREASING as tau RISES (i.e. non-decreasing toward resolution); every value
        // >= 1.0. Sweep tau ascending and check each step is no larger than the previous.
        let mut prev = f64::INFINITY;
        for tau in (0..=10_000).step_by(500) {
            let m = accelerated_pull(tau, 10_000, 2.0);
            assert!(m >= 1.0, "never below 1.0 at tau={tau}");
            assert!(m <= prev + 1e-12, "not monotone (should fall as tau rises) at tau={tau}");
            prev = m;
        }
        // A NEGATIVE max_accel can never weaken the breaker: floored at 1.0.
        assert_eq!(accelerated_pull(0, 10_000, -0.5).to_bits(), 1.0_f64.to_bits());
        assert_eq!(accelerated_pull(5_000, 10_000, -0.5).to_bits(), 1.0_f64.to_bits());
    }
}
