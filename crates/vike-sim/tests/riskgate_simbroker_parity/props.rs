//! The PROPTEST arms: the hand matrix generalized to random cases, FLOOR and MARGIN lanes.

use proptest::prelude::*;

use super::Row;
use super::reconcile::{expected_floor_divergence, reconcile};
use super::sides::{live_verdict, sim_outcome};

// ---------------------------------------------------------------------------------------------
// PROPTEST arms (testing-arch plan Phase 6, target P4) — the hand matrix above, generalized to
// hundreds of random cases per lane. Both reuse the exact `live_verdict` / `sim_outcome` engines
// and the shared `reconcile` verdict; the curated hand rows stay as the human-readable regression
// pins. The lanes mirror the file's two decision surfaces:
//   * FLOOR lane — gate UNARMED, so `apply_fill`'s fill-time floors are the sole decider and can
//     diverge from the live gate (the one documented `BelowMinReversal` pin);
//   * MARGIN lane — gate ARMED, so both sides cross the SAME `RiskGate::check` and must AGREE
//     (phase 2 unified this lane; a disagreement is the #468 context-construction bug class).
// ---------------------------------------------------------------------------------------------
proptest! {
    #![proptest_config(ProptestConfig { cases: 256, ..ProptestConfig::default() })]

    /// FLOOR LANE: over random (position, side, qty, price, multiplier, floors) with the pre-trade
    /// gate unarmed, the live min_qty/min_notional gate and `apply_fill`'s fill-time floors AGREE,
    /// except the documented below-min REVERSAL — reconciled through the shared [`reconcile`] with
    /// a structurally-classified `expected`. `reduce_only` is held false: the hand rows cover that
    /// axis, and the flag's `is_covered_reduce` interaction is not a floor-lane divergence.
    #[test]
    fn floor_lane_agrees_or_pins_below_min_reversal(
        position in prop_oneof![Just(0.0f64), -20.0f64..20.0],
        side in prop_oneof![Just(1i32), Just(-1i32)],
        qty in 0.0001f64..40.0,
        price in 0.5f64..2_000.0,
        multiplier in 0.1f64..100.0,
        min_qty in prop_oneof![Just(0.0f64), 0.0001f64..10.0],
        min_notional in prop_oneof![Just(0.0f64), 0.01f64..1_000.0],
    ) {
        let row = Row {
            name: "prop/floor",
            position,
            side,
            qty,
            price,
            multiplier,
            min_qty,
            min_notional,
            reduce_only: false,
            im: None,
            leverage: None,
            clamp: false,
            free_bp_pct: 0.0,
            equity: 1_000_000.0,
            ..Row::default()
        };
        let (live_ok, reason) = live_verdict(&row);
        let (before, _other, after, _dropped) = sim_outcome(&row);
        // The gate is unarmed, so the setup leg always establishes the declared position exactly.
        prop_assert!(
            (before - position).abs() <= 1e-9 * (1.0 + position.abs()),
            "harness: setup established {before}, not {position}"
        );
        let expected_after = before + side as f64 * qty;
        let sim_filled = (after - expected_after).abs() <= 1e-9 * (1.0 + expected_after.abs());
        reconcile(
            "prop/floor",
            live_ok,
            &reason,
            sim_filled,
            before,
            after,
            expected_floor_divergence(&row),
        );
    }

    /// MARGIN LANE: with the gate ARMED via `leverage`, both sides cross the SAME `RiskGate::check`
    /// over contexts each builds from its own state. Deny-vs-clamp phase 2 unified this lane, so the
    /// two must AGREE for EVERY random (order, position, leverage, multiplier, equity) — there is no
    /// pinned margin divergence. position/qty are scaled to the leverage room so the setup leg fits
    /// and the test order spans both sides of the budget boundary.
    #[test]
    fn margin_lane_always_agrees(
        leverage in 1.0f64..20.0,
        price in 1.0f64..1_000.0,
        multiplier in 1.0f64..10.0,
        equity in 10_000.0f64..1_000_000.0,
        side in prop_oneof![Just(1i32), Just(-1i32)],
        position_frac in -0.95f64..0.95,
        qty_frac in 0.01f64..3.0,
    ) {
        let im = 1.0 / leverage;
        let room = equity / (price * multiplier * im); // = equity * leverage / (price * mult)
        let row = Row {
            name: "prop/margin",
            position: position_frac * room,
            side,
            qty: qty_frac * room,
            price,
            multiplier,
            min_qty: 0.0,
            min_notional: 0.0,
            reduce_only: false,
            im: Some(im),
            leverage: Some(leverage),
            free_bp_pct: 0.0,
            clamp: false,
            equity,
            ..Row::default()
        };
        let (live_ok, reason) = live_verdict(&row);
        let (before, _other, after, _dropped) = sim_outcome(&row);
        // The position fits the room by construction; skip (don't fail) the rare boundary case
        // where the gate refuses the setup leg, so `before` would not equal the declared position.
        prop_assume!((before - row.position).abs() <= 1e-6 * (1.0 + row.position.abs()));
        let expected_after = before + side as f64 * row.qty;
        let sim_filled = (after - expected_after).abs() <= 1e-6 * (1.0 + expected_after.abs());
        // No pin: phase 2 made the margin lane one shared judge evaluated on both sides.
        reconcile("prop/margin", live_ok, &reason, sim_filled, before, after, None);
    }
}
