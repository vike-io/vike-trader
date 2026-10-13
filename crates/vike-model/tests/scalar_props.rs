//! Property-based tests for the pinned scalar rounding primitives (`vike_model::round_to_step` /
//! `round_to` / `nz_step`) — testing-arch plan Phase 6, target P2 (the vike-model half). Every live
//! order's price and qty flows through `round_to`/`round_to_step` (the SymbolProperties → tick/lot
//! grid), so their invariants are money-path critical. The existing unit tests pin a handful of
//! half-to-even boundary examples; these properties assert the grid-rounding LAWS over random
//! inputs.
//!
//! Domain note: values and steps are kept to the real price/tick range (|value| <= 1e6, step in
//! [1e-6, 1e3]) so `value/step` stays under f64's exact-integer ceiling (2^53). Beyond that ceiling
//! the multiple itself is unrepresentable and idempotence would fail as a fundamental f64 limit,
//! not a bug — that regime is out of the wire domain (no venue quotes a 1e-8 tick on a 1e6 price).
//! No tolerance is ever widened to force a pass: idempotence is asserted EXACTLY; the one
//! floating-slack epsilon (the half-step bound) is a documented ULP allowance, not a fudge.

use proptest::prelude::*;
use vike_model::{nz_step, round_to, round_to_step};

proptest! {
    /// IDEMPOTENCE: rounding an already-rounded value is a no-op. This is also the robust statement
    /// that the result lies exactly ON the step grid — a rounded value is a fixed point of the
    /// rounding, i.e. an integer multiple of `step`. Asserted with EXACT equality (no tolerance).
    #[test]
    fn round_to_step_is_idempotent(value in -1e6f64..1e6, step in 1e-6f64..1e3) {
        let once = round_to_step(value, step);
        let twice = round_to_step(once, step);
        prop_assert_eq!(
            once, twice,
            "round_to_step not idempotent: {} -> {} -> {} (step {})", value, once, twice, step
        );
    }

    /// RESULT IS A STEP-MULTIPLE, WITHIN TOLERANCE: the rounded value is at most half a step from
    /// the input (round-to-nearest never moves further than half a grid cell). The slack beyond
    /// `0.5·step` is a documented float allowance covering the `round(value/step)·step`
    /// multiply's rounding (<= 0.5 ULP of the result) — orders of magnitude below the half-step it
    /// guards, so it cannot hide a genuinely off-grid result.
    #[test]
    fn round_to_step_stays_within_half_a_step(value in -1e6f64..1e6, step in 1e-6f64..1e3) {
        let r = round_to_step(value, step);
        let slack = 1e-9 * (value.abs() + step);
        prop_assert!(
            (value - r).abs() <= 0.5 * step + slack,
            "round_to_step moved {} to {} — more than half a step ({})", value, r, step
        );
    }

    /// The GUARDED `round_to` contract: `None`, a zero step, and a NEGATIVE step all fall through to
    /// identity (never NaN); a strictly-positive step delegates to `round_to_step`. `nz_step`
    /// composes: `nz_step(0.0)` is `None` (identity), any other value is `Some` (guard then applies).
    #[test]
    fn round_to_guard_is_identity_off_grid_and_delegates_on_grid(
        value in -1e6f64..1e6,
        step in 1e-6f64..1e3,
    ) {
        prop_assert_eq!(round_to(value, None), value);
        prop_assert_eq!(round_to(value, Some(step)), round_to_step(value, step));
        prop_assert_eq!(round_to(value, Some(0.0)), value); // 0 step -> identity, NOT inf*0=NaN
        prop_assert_eq!(round_to(value, Some(-step)), value); // negative step -> identity
        prop_assert_eq!(round_to(value, nz_step(0.0)), value); // nz_step(0) == None
        prop_assert_eq!(round_to(value, nz_step(step)), round_to_step(value, step));
    }
}
