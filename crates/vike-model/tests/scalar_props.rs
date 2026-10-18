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

// ---- the contract value (decision 0170) --------------------------------------------------------
//
// `Linear` must be the scalar helpers BIT FOR BIT (the differential half: it is what lets twelve
// bridges, the backtest and the frozen fixtures see no change); `Inverse` has its own laws.

use vike_model::{
    ContractValue, FillKind, compute_fill, compute_fill_value, gross_notional, initial_margin,
    maintenance_margin, order_notional, signed_notional,
};

proptest! {
    /// LINEAR IS THE OLD EXPRESSION, for random finite inputs, to the bit.
    #[test]
    fn a_linear_value_is_the_scalar_expressions_bit_for_bit(
        qty in -1e6f64..1e6,
        price in -1e6f64..1e6,
        unit in 1e-6f64..1e4,
        rate in 0.0f64..10.0,
        im in 0.0f64..1.0,
    ) {
        let v = ContractValue::linear(unit);
        prop_assert_eq!(v.order_notional(qty, price).to_bits(), order_notional(qty, price, unit).to_bits());
        prop_assert_eq!(v.signed_notional(qty, price).to_bits(), signed_notional(qty, price, unit).to_bits());
        prop_assert_eq!(v.gross_notional(qty, price).to_bits(), gross_notional(qty, price, unit).to_bits());
        prop_assert_eq!(
            v.initial_margin(price, qty, rate, im).to_bits(),
            initial_margin(price, qty, unit, rate, im).to_bits()
        );
        prop_assert_eq!(
            v.maintenance_margin(price, qty, rate, im).to_bits(),
            maintenance_margin(price, qty, unit, rate, im).to_bits()
        );
    }

    /// LINEAR FILL: `compute_fill_value` with a linear value is `compute_fill`, field for field.
    #[test]
    fn a_linear_fill_is_compute_fill_bit_for_bit(
        prior in -1e3f64..1e3,
        avg in 1.0f64..1e6,
        qty in 0.001f64..1e3,
        price in 1.0f64..1e6,
        unit in 1e-3f64..1e3,
        buy in any::<bool>(),
    ) {
        let side = if buy { 1 } else { -1 };
        let a = compute_fill(prior, avg, side, qty, price, unit);
        let b = compute_fill_value(prior, avg, side, qty, price, ContractValue::linear(unit));
        prop_assert_eq!(a.kind, b.kind);
        prop_assert_eq!(a.new_size.to_bits(), b.new_size.to_bits());
        prop_assert_eq!(a.new_avg_px.to_bits(), b.new_avg_px.to_bits());
        prop_assert_eq!(a.realized_pnl.to_bits(), b.realized_pnl.to_bits());
    }

    /// INVERSE ORDER NOTIONAL IS INDEPENDENT OF THE PRICE.
    #[test]
    fn an_inverse_order_notional_ignores_the_price(
        qty in -1e6f64..1e6,
        p1 in 1.0f64..1e6,
        p2 in 1.0f64..1e6,
        unit in 1e-3f64..1e3,
    ) {
        let v = ContractValue::inverse(unit);
        prop_assert_eq!(v.order_notional(qty, p1).to_bits(), v.order_notional(qty, p2).to_bits());
        prop_assert_eq!(v.order_notional(qty, p1).to_bits(), (qty.abs() * unit).to_bits());
    }

    /// INVERSE: open then close at one price realizes exactly zero, and a long and a short
    /// closed at the same two prices are exact negatives.
    #[test]
    fn an_inverse_round_trip_is_flat_and_mirrors(
        qty in 1.0f64..1e5,
        entry in 100.0f64..1e6,
        exit in 100.0f64..1e6,
    ) {
        let v = ContractValue::inverse(1.0);
        let flat = compute_fill_value(qty, entry, -1, qty, entry, v);
        prop_assert_eq!(flat.kind, FillKind::Close);
        prop_assert_eq!(flat.realized_pnl, 0.0);
        let long = compute_fill_value(qty, entry, -1, qty, exit, v);
        let short = compute_fill_value(-qty, entry, 1, qty, exit, v);
        prop_assert_eq!(long.realized_pnl.to_bits(), (-short.realized_pnl).to_bits());
    }

    /// INVERSE ADD: the booked average is the harmonic one, whatever the two legs.
    #[test]
    fn an_inverse_add_is_the_harmonic_average(
        held in 1.0f64..1e5,
        added in 1.0f64..1e5,
        e1 in 100.0f64..1e6,
        e2 in 100.0f64..1e6,
    ) {
        let v = ContractValue::inverse(1.0);
        let out = compute_fill_value(held, e1, 1, added, e2, v);
        prop_assert_eq!(out.kind, FillKind::Add);
        let want = (held + added) / (held / e1 + added / e2);
        prop_assert_eq!(out.new_avg_px.to_bits(), want.to_bits());
    }
}
